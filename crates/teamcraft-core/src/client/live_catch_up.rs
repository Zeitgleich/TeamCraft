//! Live events during the catch-up of a fresh gateway session (issue #1021).
//!
//! The gateway connects before the catch-up: `identify` is answered, then the full sync runs while
//! [`super::gateway::keep_alive`] keeps the connection alive and holds what arrives. Until #1021 all of
//! it waited for the end of the catch-up, which after a long absence takes long: a message to the
//! chat the user has open, caught up within a second, showed only once every other conversation was
//! through.
//!
//! Now the catch-up applies held events between two of its steps, as long as nothing they depend on
//! is still being caught up. Only the oldest held frames are taken, in their order, and the first
//! that has to wait stops the rest behind it: the frames applied early come before those still
//! held, so the order is the server's, as before. What is applied early is exactly what would have
//! been applied after the catch-up, only sooner:
//!
//! - messages, MLS messages (not welcomes), read receipts and acknowledgements of a conversation
//!   this catch-up already finished: its group is at the epoch the queue reached, and the next
//!   commit or message comes next, as on a live connection. One the catch-up already read is
//!   skipped as a repeat, as it was when the held frames came after the catch-up;
//! - the same of a conversation the catch-up doesn't visit because nothing moved in it since the
//!   last sync (its heads, issue #714, and nothing local due): it counts as finished from the start
//!   (issue #1423). A welcome for it would have moved its MLS head; one a lying server sends
//!   anyway is joined from the queue first (`join_from_welcome`), so a commit applied early only
//!   brings that read forward;
//! - membership and conversation changes of a finished conversation (`MembershipAdd`,
//!   `MembershipRemove`, `ConversationUpdate`, issue #1423). They read and store that
//!   conversation's record and member list, its history after a finished import, or forget it;
//!   the only commit among them, a meeting guest's signed leave, goes into that conversation's
//!   group, which is caught up. Nothing the catch-up still does reads that conversation again,
//!   other than welcomes, which follow the queue first;
//! - typing and presence, which touch no conversation's state.
//!
//! Everything else (a welcome, a channel's role log, a deleted conversation, workspace and space
//! events, an event of a conversation still being caught up, a closed connection) waits for the end
//! of the catch-up, as before, and so does everything after it. Waiting membership changes are not
//! committed in between (`apply_pending_changes`): they may concern groups not yet caught up, and
//! the sync commits them at its end.
//!
//! A resumed session runs no catch-up, and nothing of this (looked at for issue #1448): what arrives
//! while it reads the unread counts, presence and the running calls (`resync_live_state`) waits for
//! those three reads only, and is applied right after them in its order; the counts are read once
//! the replay drained, as `GET /unread` already counts the replayed messages. The backlogs a
//! catch-up leaves are read after it where the host reads them (`history_gaps.rs`), with the
//! connection live.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use teamcraft_proto::{ConversationId, GatewayEvent, GatewayServerMessage, MlsMessageKind};

use super::Client;
use super::gateway::HeldFrames;
use crate::api::{ApiError, ServerApi};
use crate::error::CoreError;
use crate::store::LocalStore;

/// The held frames of a connection whose catch-up runs, shared with [`super::gateway::keep_alive`],
/// and what the catch-up has finished so far.
#[derive(Debug, Default)]
pub(super) struct LiveDuringCatchUp {
  pub(super) held: Arc<Mutex<HeldFrames>>,
  /// Conversations this catch-up has caught up completely.
  caught_up: BTreeSet<ConversationId>,
  /// The newest event sequence number applied early, for the session the sync then keeps: a
  /// resume replays from there, not from the start.
  pub(super) max_seq: u64,
  /// How many frames were applied early (traces and tests).
  pub(super) applied: usize,
}

impl LiveDuringCatchUp {
  pub(super) fn new(held: Arc<Mutex<HeldFrames>>) -> Self {
    Self {
      held,
      ..Self::default()
    }
  }
}

/// Whether `frame` may be applied while the catch-up still runs, with `caught_up` finished.
pub(super) fn is_live_now(
  frame: &Option<Result<GatewayServerMessage, ApiError>>,
  caught_up: &BTreeSet<ConversationId>,
) -> bool {
  let Some(Ok(GatewayServerMessage::Event { event, .. })) = frame else {
    return false;
  };
  match event {
    GatewayEvent::MessageCreate { message } => caught_up.contains(&message.conversation_id),
    GatewayEvent::MlsMessage { message } => {
      message.kind != MlsMessageKind::Welcome && caught_up.contains(&message.conversation_id)
    },
    GatewayEvent::ReadReceipt { conversation_id, .. }
    | GatewayEvent::MessageAck { conversation_id, .. }
    | GatewayEvent::MembershipAdd { conversation_id, .. }
    | GatewayEvent::MembershipRemove { conversation_id, .. }
    | GatewayEvent::ConversationUpdate { conversation_id } => caught_up.contains(conversation_id),
    GatewayEvent::TypingStart { .. } | GatewayEvent::PresenceUpdate { .. } => true,
    _ => false,
  }
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// The catch-up finished `conversation`: live events of it may now be applied.
  pub(super) fn note_live_caught_up(&mut self, conversation: ConversationId) {
    if let Some(live) = &mut self.maybe_live_catch_up {
      live.caught_up.insert(conversation);
    }
  }

  /// Conversations this catch-up doesn't visit, nothing having moved in them since the last sync:
  /// current from the start, so what arrives for them is applied between the catch-up's steps.
  pub(super) fn note_live_current(&mut self, conversations: impl IntoIterator<Item = ConversationId>) {
    if let Some(live) = &mut self.maybe_live_catch_up {
      live.caught_up.extend(conversations);
    }
  }

  /// Applies the oldest held frames that may be applied now ([`is_live_now`]), in their order, up
  /// to the first that has to wait. Nothing outside a fresh session's catch-up. A failure fails the
  /// catch-up like one of its own steps (the connection is then replaced and the sync runs again).
  pub(super) async fn apply_live_during_catch_up(&mut self) -> Result<(), CoreError> {
    let _step = crate::timing::step("apply_live_frames");
    let mut applied = 0;
    loop {
      let Some(live) = &self.maybe_live_catch_up else {
        return Ok(());
      };
      let maybe_frame = {
        let Ok(mut held) = live.held.lock() else {
          return Ok(());
        };
        match held.front() {
          Some(front) if is_live_now(front, &live.caught_up) => held.pop(),
          _ => None,
        }
      };
      let Some(Some(Ok(GatewayServerMessage::Event { seq, event }))) = maybe_frame else {
        break;
      };
      self.maybe_last_heard_ms = Some(self.now_ms());
      // Boxed: an event's future is large, and it would sit in every step of the catch-up.
      Box::pin(self.handle_event(event)).await?;
      if let Some(live) = &mut self.maybe_live_catch_up {
        live.max_seq = live.max_seq.max(seq);
        live.applied += 1;
      }
      applied += 1;
    }
    if applied > 0 {
      tracing::debug!(applied, "catch-up: live events applied");
      self.note_sync_progress();
    }
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{
    Base64Bytes, ClientMessageId, DeviceId, Id, MessageId, MessageResponse, MlsMessageId, MlsMessageResponse,
    PresenceState, Snowflake, UserId, UserKind, UserSummary,
  };

  use super::*;

  fn id<T: Id>(value: i64) -> T {
    T::from_snowflake(Snowflake::new(value).unwrap())
  }

  fn event(event: GatewayEvent) -> Option<Result<GatewayServerMessage, ApiError>> {
    Some(Ok(GatewayServerMessage::Event { seq: 1, event }))
  }

  fn message_in(conversation: ConversationId) -> GatewayEvent {
    GatewayEvent::MessageCreate {
      message: MessageResponse {
        id: id::<MessageId>(1),
        client_message_id: ClientMessageId::from_bytes([1; 16]),
        conversation_id: conversation,
        sender_user_id: id::<UserId>(2),
        sender_device_id: id::<DeviceId>(3),
        maybe_thread_id: None,
        mentioned_user_ids: Vec::new(),
        is_urgent: false,
        content_key_id: Base64Bytes::new(Vec::new()),
        ciphertext: Base64Bytes::new(Vec::new()),
      },
    }
  }

  fn mls_in(conversation: ConversationId, kind: MlsMessageKind) -> GatewayEvent {
    GatewayEvent::MlsMessage {
      message: MlsMessageResponse {
        id: id::<MlsMessageId>(4),
        conversation_id: conversation,
        epoch: 5,
        kind,
        sender_device_id: id::<DeviceId>(3),
        maybe_recipient_device_id: None,
        payload: Base64Bytes::new(Vec::new()),
      },
    }
  }

  #[test]
  fn only_events_of_finished_conversations_and_ephemeral_ones_go_ahead() {
    let done = id::<ConversationId>(7);
    let waiting = id::<ConversationId>(8);
    let caught_up = BTreeSet::from([done]);
    assert!(is_live_now(&event(message_in(done)), &caught_up));
    assert!(is_live_now(
      &event(mls_in(done, MlsMessageKind::Commit)),
      &caught_up
    ));
    assert!(!is_live_now(&event(message_in(waiting)), &caught_up));
    assert!(!is_live_now(
      &event(mls_in(waiting, MlsMessageKind::Commit)),
      &caught_up
    ));
    let presence = GatewayEvent::PresenceUpdate {
      user_id: id::<UserId>(2),
      state: PresenceState::Online,
      maybe_status: None,
    };
    assert!(is_live_now(&event(presence), &caught_up));
    assert!(!is_live_now(
      &Some(Ok(GatewayServerMessage::SessionInvalid)),
      &caught_up
    ));
    assert!(!is_live_now(&None, &caught_up));
  }

  #[test]
  fn membership_and_conversation_changes_go_ahead_only_for_finished_conversations() {
    let done = id::<ConversationId>(7);
    let waiting = id::<ConversationId>(8);
    let caught_up = BTreeSet::from([done]);
    let changes = |conversation_id: ConversationId| {
      [
        GatewayEvent::ConversationUpdate { conversation_id },
        GatewayEvent::MembershipRemove {
          conversation_id,
          user_id: id::<UserId>(2),
        },
        GatewayEvent::MembershipAdd {
          conversation_id,
          user: UserSummary {
            id: id::<UserId>(2),
            maybe_username: None,
            display_name: "Ann".into(),
            kind: UserKind::Member,
            maybe_email: None,
            maybe_organization: None,
            maybe_has_devices: None,
            is_deactivated: false,
            is_deleted: false,
          },
        },
      ]
    };
    for change in changes(done) {
      assert!(is_live_now(&event(change.clone()), &caught_up), "{change:?}");
    }
    for change in changes(waiting) {
      assert!(!is_live_now(&event(change.clone()), &caught_up), "{change:?}");
    }
    // A channel's role log and a deletion wait, also for a finished conversation.
    assert!(!is_live_now(
      &event(GatewayEvent::ConversationDelete {
        conversation_id: done,
        maybe_signed_deletion: None,
      }),
      &caught_up
    ));
  }

  #[test]
  fn a_welcome_waits_also_for_a_finished_conversation() {
    let done = id::<ConversationId>(7);
    assert!(!is_live_now(
      &event(mls_in(done, MlsMessageKind::Welcome)),
      &BTreeSet::from([done])
    ));
  }
}
