//! Ballot boxes of anonymous polls (issue #2121, e2ee.md §4.12.1). The server can't read a ballot,
//! and hands members the ballots without their senders once a box is closed. It keeps one ballot
//! per account while the box is open and forgets the voters when it closes.

use teamcraft_db::{BallotOutcome, StoredBallotBox};
use teamcraft_proto::{
  BallotBoxResponse, Base64Bytes, ConversationId, GatewayEvent, MAX_BALLOT_BYTES, MessageId, Permission,
};
use time::OffsetDateTime;
use tracing::{info, warn};

use super::access::{ConversationAccess, load_conversation, require};
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

/// Boxes a sweep closes per run at most; the rest wait for the next run or close when read.
const MAX_CLOSED_PER_SWEEP: i64 = 500;

/// Opens the ballot box of the poll `message`: only the account that sent the message may. A box
/// already open stays as it is.
pub async fn open_box(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  message: MessageId,
  maybe_closes_at_ms: Option<i64>,
) -> Result<(), ApiError> {
  member_access(state, caller, conversation).await?;
  let sender = state
    .database
    .message_sender_user(conversation, message)
    .await?
    .ok_or_else(|| ApiError::InvalidRequest("unknown message".into()))?;
  if sender != caller.user.id {
    return Err(ApiError::Forbidden(
      "only the poll's sender opens its ballot box".into(),
    ));
  }
  let maybe_closes_at = maybe_closes_at_ms
    .map(|ms| {
      OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .map_err(|_| ApiError::InvalidRequest("closes_at is out of range".into()))
    })
    .transpose()?;
  let did_open = state
    .database
    .open_ballot_box(conversation, message, caller.user.id, maybe_closes_at)
    .await?;
  if did_open {
    info!(conversation_id = %conversation, message_id = %message, "ballot box opened");
    announce(state, conversation, message, 0, false).await;
  }
  Ok(())
}

/// The box as the caller sees it.
pub async fn get_box(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  message: MessageId,
) -> Result<BallotBoxResponse, ApiError> {
  member_access(state, caller, conversation).await?;
  let stored = state
    .database
    .ballot_box(conversation, message, caller.user.id, OffsetDateTime::now_utc())
    .await?
    .ok_or_else(ApiError::ballot_box_not_found)?;
  Ok(response(stored))
}

/// Keeps `maybe_ballot` as the caller's one ballot, or withdraws it with `None`, while open.
pub async fn set_ballot(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  message: MessageId,
  maybe_ballot: Option<Base64Bytes>,
) -> Result<BallotBoxResponse, ApiError> {
  member_access(state, caller, conversation).await?;
  if let Some(ballot) = &maybe_ballot {
    let length = ballot.as_ref().len();
    if length == 0 || length > MAX_BALLOT_BYTES {
      return Err(ApiError::InvalidRequest(format!(
        "a ballot is 1 to {MAX_BALLOT_BYTES} bytes"
      )));
    }
  }
  let now = OffsetDateTime::now_utc();
  let outcome = state
    .database
    .set_ballot(
      conversation,
      message,
      caller.user.id,
      maybe_ballot.as_ref().map(AsRef::as_ref),
      now,
    )
    .await?;
  match outcome {
    BallotOutcome::Taken { ballot_count } => {
      announce(state, conversation, message, ballot_count, false).await
    },
    BallotOutcome::NoBox => return Err(ApiError::ballot_box_not_found()),
    BallotOutcome::Closed => return Err(ApiError::PollClosed),
  }
  get_box(state, caller, conversation, message).await
}

/// Closes the box: the poll's sender, or who may delete others' posts in the conversation.
pub async fn close_box(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  message: MessageId,
) -> Result<BallotBoxResponse, ApiError> {
  let access = member_access(state, caller, conversation).await?;
  let now = OffsetDateTime::now_utc();
  let stored = state
    .database
    .ballot_box(conversation, message, caller.user.id, now)
    .await?
    .ok_or_else(ApiError::ballot_box_not_found)?;
  if stored.creator_user_id != caller.user.id {
    require(state, caller, Permission::DeleteOthersPosts, &access.target())?;
  }
  if let Some((true, ballot_count)) = state
    .database
    .close_ballot_box(conversation, message, now)
    .await?
  {
    info!(conversation_id = %conversation, message_id = %message, "ballot box closed");
    announce(state, conversation, message, ballot_count, true).await;
  }
  get_box(state, caller, conversation, message).await
}

/// Closes the boxes whose closing time passed at `now`, telling their members. Run with the
/// retention job; a box read before that closes on reading.
pub async fn close_due(state: &AppState, now: OffsetDateTime) -> Result<usize, ApiError> {
  let closed = state
    .database
    .close_due_ballot_boxes(now, MAX_CLOSED_PER_SWEEP)
    .await?;
  for ballot_box in &closed {
    announce(
      state,
      ballot_box.conversation_id,
      ballot_box.message_id,
      ballot_box.ballot_count,
      true,
    )
    .await;
  }
  Ok(closed.len())
}

/// The caller's access to `conversation`, which must include membership: only members vote and
/// read ballots.
async fn member_access(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
) -> Result<ConversationAccess, ApiError> {
  let access = load_conversation(state, caller, conversation).await?;
  if !access.is_member() {
    return Err(ApiError::Forbidden("only members take part in polls".into()));
  }
  Ok(access)
}

/// Tells the conversation's members that the box changed; they fetch it.
async fn announce(
  state: &AppState,
  conversation: ConversationId,
  message: MessageId,
  ballot_count: u32,
  is_closed: bool,
) {
  let members = match state.database.conversation_member_ids(conversation).await {
    Ok(members) => members,
    Err(error) => {
      // The change is stored; members see it when they next fetch the box (opening the poll,
      // reconnecting).
      warn!(conversation_id = %conversation, %error, "ballot box event not sent");
      return;
    },
  };
  state.hub.publish(
    members,
    &GatewayEvent::BallotBoxChanged {
      conversation_id: conversation,
      message_id: message,
      ballot_count,
      is_closed,
    },
  );
}

fn response(stored: StoredBallotBox) -> BallotBoxResponse {
  BallotBoxResponse {
    ballot_count: stored.ballot_count,
    is_closed: stored.is_closed,
    maybe_closes_at_ms: stored
      .maybe_closes_at
      .map(|at| i64::try_from(at.unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX)),
    maybe_mine: stored.maybe_mine.map(Base64Bytes::from),
    ballots: stored.ballots.into_iter().map(Base64Bytes::from).collect(),
  }
}
