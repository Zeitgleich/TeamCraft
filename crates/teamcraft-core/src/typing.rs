//! Typing indicators, shared by every shell (the mobile runner, the web core and the desktop runtime):
//! who is typing where ([`TypingState`], filled from `typing_start` events) and the user's own
//! announcements ([`TypingAnnouncer`]).
//!
//! Typing is metadata the server sees: who types in which conversation, never what (e2ee.md §4.4).
//! It is ephemeral on both sides: the server neither numbers nor buffers it (gateway.md), and the
//! clients keep it in memory only. Whether the user shares their own typing is a local preference
//! the UI checks before it announces anything.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use teamcraft_proto::{ConversationId, GatewayClientMessage, UserId};

use crate::api::GatewayConnection;
use crate::connection::ConnectionSupervisor;

/// How long a "typing" indicator stays without a new `typing_start` (the server forwards one per
/// [`TYPING_INTERVAL_MS`], so a typing person refreshes it in time).
pub const TYPING_TIMEOUT_MS: i64 = 6_000;
/// How often the user's typing is announced per conversation: the server forwards no more than one
/// per 3 s and conversation (gateway.md), so sending more is wasted.
pub const TYPING_INTERVAL_MS: i64 = 3_000;

/// Who is typing where, with the time the indicator was last refreshed. Ephemeral, not stored.
/// Cloning shares it: a UI adapter keeps a clone to read while the sync loop owns the client.
#[derive(Clone, Debug, Default)]
pub struct TypingState {
  seen: Arc<Mutex<HashMap<(ConversationId, UserId), i64>>>,
}

/// Sends the user's own typing to the other members of a conversation: at most one frame per
/// [`TYPING_INTERVAL_MS`] and conversation, and none while offline (a late indicator would be wrong).
/// Each shell's loop keeps one.
#[derive(Debug, Default)]
pub struct TypingAnnouncer {
  /// Where and when typing last went out.
  maybe_last: Option<(ConversationId, i64)>,
}

impl TypingState {
  /// People other than `me` typing in a conversation at `now_ms`, sorted.
  pub fn users(&self, conversation: ConversationId, me: UserId, now_ms: i64) -> Vec<UserId> {
    let mut users: Vec<UserId> = self
      .lock()
      .iter()
      .filter(|((typed_in, user), at)| *typed_in == conversation && *user != me && is_shown(**at, now_ms))
      .map(|((_, user), _)| *user)
      .collect();
    users.sort();
    users
  }

  /// When what [`Self::users`] answers changes by itself next: the moment the first indicator runs
  /// out, or `now_ms` when one already has and is still held ([`Self::drop_expired`] forgets it).
  /// `None` while nobody types. A shell wakes up then and tells its UI, which reads again.
  pub fn next_change_ms(&self, now_ms: i64) -> Option<i64> {
    self
      .lock()
      .values()
      .map(|at| {
        if is_shown(*at, now_ms) {
          at.saturating_add(TYPING_TIMEOUT_MS)
        } else {
          now_ms
        }
      })
      .min()
  }

  /// Forgets the indicators that ran out by `now_ms`; whether there were any (what the UI shows
  /// changed).
  pub fn drop_expired(&self, now_ms: i64) -> bool {
    let mut seen = self.lock();
    let before = seen.len();
    seen.retain(|_, at| is_shown(*at, now_ms));
    seen.len() != before
  }

  pub(crate) fn note(&self, conversation: ConversationId, user: UserId, now_ms: i64) {
    let mut seen = self.lock();
    seen.insert((conversation, user), now_ms);
    seen.retain(|_, at| is_shown(*at, now_ms));
  }

  pub(crate) fn clear(&self, conversation: ConversationId, user: UserId) {
    self.lock().remove(&(conversation, user));
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(ConversationId, UserId), i64>> {
    // A panic while holding the lock leaves a plain map behind; it stays usable.
    self
      .seen
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
  }
}

impl TypingAnnouncer {
  /// Tells the other members of `conversation` that the user types, on the open connection, unless
  /// that went out less than [`TYPING_INTERVAL_MS`] ago. Offline it is dropped, not queued.
  pub async fn announce<G: GatewayConnection>(
    &mut self,
    connection: &mut ConnectionSupervisor<G>,
    conversation: ConversationId,
    now_ms: i64,
  ) {
    if !connection.is_connected() || !self.take_turn(conversation, now_ms) {
      return;
    }
    let frame = GatewayClientMessage::Typing {
      conversation_id: conversation,
    };
    // A failed send replaces the connection; the user's next keystrokes announce typing again.
    if !connection.send(now_ms, &frame).await {
      tracing::debug!("typing not announced");
    }
  }

  /// Whether typing in `conversation` goes out at `now_ms`; notes it when it does. After the clock
  /// went back it goes out (the last one would otherwise block it for as long as the jump).
  fn take_turn(&mut self, conversation: ConversationId, now_ms: i64) -> bool {
    let is_recent = self.maybe_last.is_some_and(|(last, at)| {
      last == conversation && (0..TYPING_INTERVAL_MS).contains(&now_ms.saturating_sub(at))
    });
    if is_recent {
      return false;
    }
    self.maybe_last = Some((conversation, now_ms));
    true
  }
}

/// Whether an indicator refreshed at `at_ms` still shows at `now_ms`. One from the "future" (the
/// clock went back since) does not: the next `typing_start` brings it back (robustness.md, "Time").
fn is_shown(at_ms: i64, now_ms: i64) -> bool {
  (0..TYPING_TIMEOUT_MS).contains(&now_ms.saturating_sub(at_ms))
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  const ME: i64 = 1;
  const ALICE: i64 = 2;
  const BOB: i64 = 3;

  mod state {
    use super::*;

    #[test]
    fn indicators_show_for_the_timeout_and_never_for_oneself() {
      let typing = TypingState::default();
      typing.note(conversation(7), user(ALICE), 1_000);
      typing.note(conversation(7), user(ME), 1_000);
      typing.note(conversation(8), user(BOB), 1_000);
      assert_eq!(typing.users(conversation(7), user(ME), 1_000), vec![user(ALICE)]);
      assert_eq!(
        typing.users(conversation(7), user(ME), 1_000 + TYPING_TIMEOUT_MS - 1),
        vec![user(ALICE)]
      );
      assert!(
        typing
          .users(conversation(7), user(ME), 1_000 + TYPING_TIMEOUT_MS)
          .is_empty()
      );
    }

    #[test]
    fn the_next_change_is_when_the_first_indicator_runs_out() {
      let typing = TypingState::default();
      assert_eq!(typing.next_change_ms(0), None);
      typing.note(conversation(7), user(ALICE), 1_000);
      typing.note(conversation(7), user(BOB), 2_500);
      assert_eq!(typing.next_change_ms(3_000), Some(1_000 + TYPING_TIMEOUT_MS));
      // Run out but still held: due at once.
      assert_eq!(typing.next_change_ms(7_500), Some(7_500));
    }

    #[test]
    fn dropping_the_expired_ones_reports_whether_any_ran_out() {
      let typing = TypingState::default();
      typing.note(conversation(7), user(ALICE), 1_000);
      typing.note(conversation(7), user(BOB), 2_500);
      assert!(!typing.drop_expired(3_000));
      assert!(typing.drop_expired(1_000 + TYPING_TIMEOUT_MS));
      assert_eq!(typing.users(conversation(7), user(ME), 7_000), vec![user(BOB)]);
      assert_eq!(typing.next_change_ms(7_000), Some(2_500 + TYPING_TIMEOUT_MS));
      assert!(typing.drop_expired(2_500 + TYPING_TIMEOUT_MS));
      assert_eq!(typing.next_change_ms(9_000), None);
    }

    #[test]
    fn a_message_ends_the_indicator() {
      let typing = TypingState::default();
      typing.note(conversation(7), user(ALICE), 1_000);
      typing.clear(conversation(7), user(ALICE));
      assert!(typing.users(conversation(7), user(ME), 1_000).is_empty());
      assert_eq!(typing.next_change_ms(1_000), None);
    }

    #[test]
    fn an_indicator_from_before_the_clock_went_back_is_not_shown() {
      let typing = TypingState::default();
      typing.note(conversation(7), user(ALICE), 60_000);
      assert!(typing.users(conversation(7), user(ME), 1_000).is_empty());
      // Due at once, not when the clock has caught up a minute later.
      assert_eq!(typing.next_change_ms(1_000), Some(1_000));
      assert!(typing.drop_expired(1_000));
    }
  }

  mod announcer {
    use super::*;

    #[test]
    fn typing_goes_out_at_most_once_per_interval_and_conversation() {
      let mut announcer = TypingAnnouncer::default();
      assert!(announcer.take_turn(conversation(7), 1_000));
      assert!(!announcer.take_turn(conversation(7), 1_000 + TYPING_INTERVAL_MS - 1));
      assert!(announcer.take_turn(conversation(7), 1_000 + TYPING_INTERVAL_MS));
    }

    #[test]
    fn another_conversation_goes_out_at_once() {
      let mut announcer = TypingAnnouncer::default();
      assert!(announcer.take_turn(conversation(7), 1_000));
      assert!(announcer.take_turn(conversation(8), 1_500));
      assert!(announcer.take_turn(conversation(7), 2_000));
    }

    #[test]
    fn typing_goes_out_after_the_clock_went_back() {
      let mut announcer = TypingAnnouncer::default();
      assert!(announcer.take_turn(conversation(7), 60_000));
      assert!(announcer.take_turn(conversation(7), 1_000));
    }
  }

  fn conversation(raw: i64) -> ConversationId {
    ConversationId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn user(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
