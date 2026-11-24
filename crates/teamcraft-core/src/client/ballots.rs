//! Anonymous polls on this device (issue #2121, e2ee.md §4.12.1): opening a poll's ballot box,
//! handing in, withdrawing and closing go through the queue of pending actions, so they survive
//! being offline and a restart; what the box holds is read from the server, counted with the
//! poll's key and kept in [`Table::BallotBoxes`](crate::store::Table::BallotBoxes) for the screens.

use teamcraft_proto::{ClientMessageId, ConversationId, ErrorCode, MessageId};

use super::Client;
use super::pending_actions::PendingAction;
use crate::anonymous_poll::{self as polls, BallotBoxRecord};
use crate::api::{ApiError, ServerApi};
use crate::error::CoreError;
use crate::store::LocalStore;

/// What the screens ask of an anonymous poll.
#[cfg(feature = "ui")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BallotCommand {
  /// The user sent the poll: its box opens once the poll has its server ID.
  Open {
    conversation: ConversationId,
    poll: ClientMessageId,
  },
  /// The user's choices; none withdraws the ballot.
  Vote {
    conversation: ConversationId,
    poll: ClientMessageId,
    choices: Vec<u32>,
  },
  Close {
    conversation: ConversationId,
    poll: ClientMessageId,
  },
  /// The poll shows and this device read nothing of its box yet.
  Refresh {
    conversation: ConversationId,
    poll: ClientMessageId,
  },
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  #[cfg(feature = "ui")]
  pub(crate) async fn ballot_command(&mut self, command: BallotCommand) -> Result<(), CoreError> {
    match command {
      BallotCommand::Open { conversation, poll } => {
        self
          .queue_action(PendingAction::OpenBallotBox { conversation, poll })
          .await
      },
      BallotCommand::Vote {
        conversation,
        poll,
        choices,
      } => {
        // Shown at once; the server's answer (or a refusal) sets it right.
        let mut record = polls::stored_ballot_box(&self.store, conversation, poll)?.unwrap_or_default();
        record.my_choices.clone_from(&choices);
        polls::store_ballot_box(&self.store, conversation, poll, &record)?;
        self
          .queue_action(PendingAction::Ballot {
            conversation,
            poll,
            choices,
          })
          .await
      },
      BallotCommand::Close { conversation, poll } => {
        self
          .queue_action(PendingAction::CloseBallotBox { conversation, poll })
          .await
      },
      BallotCommand::Refresh { conversation, poll } => self.refresh_ballot_box(conversation, poll).await,
    }
  }

  /// Reads the box of the anonymous poll `poll` and keeps it counted. A poll this device doesn't
  /// hold, or one still in the outbox, is left alone; a box not opened yet is kept as such.
  pub async fn refresh_ballot_box(
    &mut self,
    conversation: ConversationId,
    poll: ClientMessageId,
  ) -> Result<(), CoreError> {
    let Some(message) = self.store.message(conversation, poll)? else {
      return Ok(());
    };
    let (Some(anonymous), Some(server_id)) = (polls::anonymous_poll_of(&message), message.maybe_server_id)
    else {
      return Ok(());
    };
    let maybe_previous = polls::stored_ballot_box(&self.store, conversation, poll)?;
    let record = match self.api.ballot_box(conversation, server_id).await {
      Ok(response) => polls::counted_ballot_box(&message, anonymous, &response, maybe_previous.as_ref()),
      Err(error) if error.error_code() == Some(&ErrorCode::BallotBoxNotFound) => BallotBoxRecord {
        is_open: false,
        ..maybe_previous.unwrap_or_default()
      },
      Err(error) => return Err(error.into()),
    };
    if record.skipped > 0 {
      tracing::warn!(
        conversation_id = %conversation,
        skipped = record.skipped,
        "ballots of an anonymous poll didn't count"
      );
    }
    polls::store_ballot_box(&self.store, conversation, poll, &record)
  }

  /// The gateway said the box of the poll with server ID `message` changed: the count is taken
  /// from the event; the box is read when this device read none yet or it just closed (the
  /// ballots come only then).
  pub(super) async fn note_ballot_box_changed(
    &mut self,
    conversation: ConversationId,
    message: MessageId,
    ballot_count: u32,
    is_closed: bool,
  ) -> Result<(), CoreError> {
    let Some(stored) = self.store.message_by_server_id(conversation, message)? else {
      return Ok(());
    };
    if polls::anonymous_poll_of(&stored).is_none() {
      return Ok(());
    }
    let poll = stored.client_message_id;
    match polls::stored_ballot_box(&self.store, conversation, poll)? {
      Some(record) if record.is_open && (record.is_closed || !is_closed) => {
        let record = BallotBoxRecord {
          ballot_count,
          ..record
        };
        polls::store_ballot_box(&self.store, conversation, poll, &record)
      },
      _ => self.refresh_ballot_box(conversation, poll).await,
    }
  }

  /// Sends a queued ballot action for the poll `poll`: `Ok(None)` while the poll has no server ID
  /// yet, `Ok(Some(()))` once there is nothing (more) to send.
  pub(super) async fn send_ballot_action(&mut self, action: &PendingAction) -> Result<Option<()>, ApiError> {
    let (conversation, poll) = match action {
      PendingAction::OpenBallotBox { conversation, poll }
      | PendingAction::Ballot {
        conversation, poll, ..
      }
      | PendingAction::CloseBallotBox { conversation, poll } => (*conversation, *poll),
      _ => return Ok(Some(())),
    };
    let message = match self.store.message(conversation, poll) {
      Ok(Some(message)) => message,
      // Gone from this device (deleted, conversation left): nothing to send.
      Ok(None) => return Ok(Some(())),
      // An unreadable store: every other entry would fail the same way; the flush stops.
      Err(error) => return Err(ApiError::Network(error.to_string())),
    };
    let (Some(anonymous), Some(server_id)) = (polls::anonymous_poll_of(&message), message.maybe_server_id)
    else {
      return Ok(message.maybe_server_id.map(|_| ()));
    };
    match action {
      PendingAction::OpenBallotBox { .. } => {
        let request = teamcraft_proto::OpenBallotBoxRequest {
          maybe_closes_at_ms: anonymous.spec.maybe_closes_at_ms,
        };
        self
          .api
          .open_ballot_box(conversation, server_id, &request)
          .await?;
      },
      PendingAction::Ballot { choices, .. } => {
        let maybe_request = if choices.is_empty() {
          None
        } else {
          // Invariant-like: the poll's key was read by `anonymous_poll_of`, and the choices were
          // checked against the poll before they were queued. A failure drops the action.
          let key = anonymous
            .key()
            .map_err(|error| ApiError::Decode(error.to_string()))?;
          let ballot = key
            .seal(polls::ballot_scope(&message), choices, &mut rand_core::OsRng)
            .map_err(|error| ApiError::Decode(error.to_string()))?;
          Some(teamcraft_proto::CastBallotRequest {
            ballot: ballot.into(),
          })
        };
        self
          .api
          .set_ballot(conversation, server_id, maybe_request.as_ref())
          .await?;
      },
      PendingAction::CloseBallotBox { .. } => {
        self.api.close_ballot_box(conversation, server_id).await?;
      },
      _ => {},
    }
    Ok(Some(()))
  }
}
