//! Typing indicators, shared by every shell (the mobile runner, the web core and the desktop runtime):
//! who is typing where ([`TypingState`], filled from `typing_start` events) and the user's own
//! announcements ([`TypingAnnouncer`]).
//!
//! Every conversation is tracked, not only the open one: the chat list and the conversation header
//! show typing too (#882). A reply typed in a thread shows in that thread only; clients that don't
//! name the thread type in the conversation.
//!
//! Typing is metadata the server sees: who types in which conversation (and thread), never what
//! (e2ee.md §4.4).
//! It is ephemeral on both sides: the server neither numbers nor buffers it (gateway.md), and the
//! clients keep it in memory only. Whether the user shares their own typing is a local preference
//! the UI checks before it announces anything.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use teamcraft_proto::{ConversationId, GatewayClientMessage, ThreadId, UserId};

use crate::api::GatewayConnection;
use crate::connection::ConnectionSupervisor;

/// How long a "typing" indicator stays without a new `typing_start` (the server forwards one per
/// [`TYPING_INTERVAL_MS`], so a typing person refreshes it in time).
pub const TYPING_TIMEOUT_MS: i64 = 6_000;
/// How often the user's typing is announced per conversation: the server forwards no more than one
/// per 3 s and conversation (gateway.md), so sending more is wasted.
pub const TYPING_INTERVAL_MS: i64 = 3_000;

/// Where someone types: a conversation, or with a thread a reply in it.
pub type TypingPlace = (ConversationId, Option<ThreadId>);

/// Who is typing where, with the time the indicator was last refreshed. Ephemeral, not stored.
/// Cloning shares it: a UI adapter keeps a clone to read while the sync loop owns the client.
///
/// A person types in one place per conversation: the conversation itself or one of its threads, so
/// moving from one to the other moves the indicator.
#[derive(Clone, Debug, Default)]
pub struct TypingState {
  seen: Arc<Mutex<HashMap<(ConversationId, UserId), Typist>>>,
}

/// Where in a conversation someone types, and when that was last refreshed.
#[derive(Clone, Copy, Debug)]
struct Typist {
  maybe_thread: Option<ThreadId>,
  at_ms: i64,
}

/// Sends the user's own typing to the other members of a conversation: at most one frame per
/// [`TYPING_INTERVAL_MS`] and conversation, and none while offline (a late indicator would be wrong).
/// Each shell's loop keeps one.
#[derive(Debug, Default)]
pub struct TypingAnnouncer {
  /// Where and when typing last went out.
  maybe_last: Option<(TypingPlace, i64)>,
}

impl TypingState {
  /// People other than `me` typing in a conversation itself (not in its threads) at `now_ms`,
  /// sorted.
  pub fn users(&self, conversation: ConversationId, me: UserId, now_ms: i64) -> Vec<UserId> {
    self.users_at((conversation, None), me, now_ms)
  }

  /// People other than `me` typing a reply in a thread at `now_ms`, sorted.
  pub fn thread_users(
    &self,
    conversation: ConversationId,
    thread: ThreadId,
    me: UserId,
    now_ms: i64,
  ) -> Vec<UserId> {
    self.users_at((conversation, Some(thread)), me, now_ms)
  }

  /// Everywhere people other than `me` type at `now_ms`: each place once, with its people, sorted
  /// (the chat list reads all conversations at once instead of asking each).
  pub fn everywhere(&self, me: UserId, now_ms: i64) -> Vec<(TypingPlace, Vec<UserId>)> {
    let mut typists: Vec<(TypingPlace, UserId)> = self
      .lock()
      .iter()
      .filter(|((_, user), typist)| *user != me && is_shown(typist.at_ms, now_ms))
      .map(|((conversation, user), typist)| ((*conversation, typist.maybe_thread), *user))
      .collect();
    typists.sort();
    let mut places: Vec<(TypingPlace, Vec<UserId>)> = Vec::new();
    for (place, user) in typists {
      match places.last_mut() {
        Some((last, users)) if *last == place => users.push(user),
        _ => places.push((place, vec![user])),
      }
    }
    places
  }

  fn users_at(&self, place: TypingPlace, me: UserId, now_ms: i64) -> Vec<UserId> {
    let (conversation, maybe_thread) = place;
    let mut users: Vec<UserId> = self
      .lock()
      .iter()
      .filter(|((typed_in, user), typist)| {
        *typed_in == conversation
          && typist.maybe_thread == maybe_thread
          && *user != me
          && is_shown(typist.at_ms, now_ms)
      })
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
      .map(|typist| typist.at_ms)
      .map(|at| {
        if is_shown(at, now_ms) {
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
    seen.retain(|_, typist| is_shown(typist.at_ms, now_ms));
    seen.len() != before
  }

  /// `user` types in `conversation`, or with `maybe_thread` a reply in that thread.
  pub(crate) fn note(
    &self,
    conversation: ConversationId,
    maybe_thread: Option<ThreadId>,
    user: UserId,
    now_ms: i64,
  ) {
    let mut seen = self.lock();
    seen.insert(
      (conversation, user),
      Typist {
        maybe_thread,
        at_ms: now_ms,
      },
    );
    seen.retain(|_, typist| is_shown(typist.at_ms, now_ms));
  }

  /// A message from `user` ends their indicator in the conversation and its threads.
  pub(crate) fn clear(&self, conversation: ConversationId, user: UserId) {
    self.lock().remove(&(conversation, user));
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(ConversationId, UserId), Typist>> {
    // A panic while holding the lock leaves a plain map behind; it stays usable.
    self
      .seen
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
  }
}

impl TypingAnnouncer {
  /// Tells the other members of `conversation` that the user types (with `maybe_thread`, a reply in
  /// that thread), on the open connection, unless that went out less than [`TYPING_INTERVAL_MS`]
  /// ago. Offline it is dropped, not queued.
  pub async fn announce<G: GatewayConnection>(
    &mut self,
    connection: &mut ConnectionSupervisor<G>,
    conversation: ConversationId,
    maybe_thread: Option<ThreadId>,
    now_ms: i64,
  ) {
    if !connection.is_connected() || !self.take_turn((conversation, maybe_thread), now_ms) {
      return;
    }
    let frame = GatewayClientMessage::Typing {
      conversation_id: conversation,
      maybe_thread_id: maybe_thread,
    };
    // A failed send replaces the connection; the user's next keystrokes announce typing again.
    if !connection.send(now_ms, &frame).await {
      tracing::debug!("typing not announced");
    }
  }

  /// Whether typing in `place` goes out at `now_ms`; notes it when it does. After the clock went
  /// back it goes out (the last one would otherwise block it for as long as the jump).
  fn take_turn(&mut self, place: TypingPlace, now_ms: i64) -> bool {
    let is_recent = self.maybe_last.is_some_and(|(last, at)| {
      last == place && (0..TYPING_INTERVAL_MS).contains(&now_ms.saturating_sub(at))
    });
    if is_recent {
      return false;
    }
    self.maybe_last = Some((place, now_ms));
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
      typing.note(conversation(7), None, user(ALICE), 1_000);
      typing.note(conversation(7), None, user(ME), 1_000);
      typing.note(conversation(8), None, user(BOB), 1_000);
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
      typing.note(conversation(7), None, user(ALICE), 1_000);
      typing.note(conversation(7), None, user(BOB), 2_500);
      assert_eq!(typing.next_change_ms(3_000), Some(1_000 + TYPING_TIMEOUT_MS));
      // Run out but still held: due at once.
      assert_eq!(typing.next_change_ms(7_500), Some(7_500));
    }

    #[test]
    fn dropping_the_expired_ones_reports_whether_any_ran_out() {
      let typing = TypingState::default();
      typing.note(conversation(7), None, user(ALICE), 1_000);
      typing.note(conversation(7), None, user(BOB), 2_500);
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
      typing.note(conversation(7), None, user(ALICE), 1_000);
      typing.clear(conversation(7), user(ALICE));
      assert!(typing.users(conversation(7), user(ME), 1_000).is_empty());
      assert_eq!(typing.next_change_ms(1_000), None);
    }

    #[test]
    fn every_conversation_is_tracked_and_read_at_once() {
      let typing = TypingState::default();
      typing.note(conversation(8), None, user(BOB), 1_000);
      typing.note(conversation(7), None, user(BOB), 1_000);
      typing.note(conversation(7), None, user(ALICE), 1_500);
      typing.note(conversation(9), None, user(ME), 1_500);
      assert_eq!(
        typing.everywhere(user(ME), 2_000),
        vec![
          ((conversation(7), None), vec![user(ALICE), user(BOB)]),
          ((conversation(8), None), vec![user(BOB)]),
        ]
      );
      // Each runs out on its own.
      assert_eq!(
        typing.everywhere(user(ME), 1_000 + TYPING_TIMEOUT_MS),
        vec![((conversation(7), None), vec![user(ALICE)])]
      );
      assert!(typing.everywhere(user(ME), 1_500 + TYPING_TIMEOUT_MS).is_empty());
    }

    #[test]
    fn a_reply_in_a_thread_shows_in_that_thread_only() {
      let typing = TypingState::default();
      typing.note(conversation(7), Some(thread(70)), user(ALICE), 1_000);
      typing.note(conversation(7), None, user(BOB), 1_000);
      assert_eq!(typing.users(conversation(7), user(ME), 1_000), vec![user(BOB)]);
      assert_eq!(
        typing.thread_users(conversation(7), thread(70), user(ME), 1_000),
        vec![user(ALICE)]
      );
      assert!(
        typing
          .thread_users(conversation(7), thread(71), user(ME), 1_000)
          .is_empty()
      );
      assert_eq!(
        typing.everywhere(user(ME), 1_000),
        vec![
          ((conversation(7), None), vec![user(BOB)]),
          ((conversation(7), Some(thread(70))), vec![user(ALICE)]),
        ]
      );
    }

    #[test]
    fn typing_moves_between_the_conversation_and_its_threads() {
      let typing = TypingState::default();
      typing.note(conversation(7), None, user(ALICE), 1_000);
      typing.note(conversation(7), Some(thread(70)), user(ALICE), 1_500);
      assert!(typing.users(conversation(7), user(ME), 1_500).is_empty());
      assert_eq!(
        typing.thread_users(conversation(7), thread(70), user(ME), 1_500),
        vec![user(ALICE)]
      );
      // A message from her ends it wherever she typed.
      typing.clear(conversation(7), user(ALICE));
      assert!(typing.everywhere(user(ME), 1_500).is_empty());
    }

    #[test]
    fn own_typing_from_another_device_is_never_shown() {
      let typing = TypingState::default();
      typing.note(conversation(7), None, user(ME), 1_000);
      typing.note(conversation(7), Some(thread(70)), user(ME), 1_000);
      assert!(typing.users(conversation(7), user(ME), 1_000).is_empty());
      assert!(
        typing
          .thread_users(conversation(7), thread(70), user(ME), 1_000)
          .is_empty()
      );
      assert!(typing.everywhere(user(ME), 1_000).is_empty());
    }

    #[test]
    fn an_indicator_from_before_the_clock_went_back_is_not_shown() {
      let typing = TypingState::default();
      typing.note(conversation(7), None, user(ALICE), 60_000);
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
      assert!(announcer.take_turn((conversation(7), None), 1_000));
      assert!(!announcer.take_turn((conversation(7), None), 1_000 + TYPING_INTERVAL_MS - 1));
      assert!(announcer.take_turn((conversation(7), None), 1_000 + TYPING_INTERVAL_MS));
    }

    #[test]
    fn another_conversation_goes_out_at_once() {
      let mut announcer = TypingAnnouncer::default();
      assert!(announcer.take_turn((conversation(7), None), 1_000));
      assert!(announcer.take_turn((conversation(8), None), 1_500));
      assert!(announcer.take_turn((conversation(7), None), 2_000));
    }

    #[test]
    fn a_thread_reply_goes_out_beside_the_conversation() {
      let mut announcer = TypingAnnouncer::default();
      assert!(announcer.take_turn((conversation(7), None), 1_000));
      assert!(announcer.take_turn((conversation(7), Some(thread(70))), 1_500));
      assert!(!announcer.take_turn((conversation(7), Some(thread(70))), 2_000));
    }

    #[test]
    fn typing_goes_out_after_the_clock_went_back() {
      let mut announcer = TypingAnnouncer::default();
      assert!(announcer.take_turn((conversation(7), None), 60_000));
      assert!(announcer.take_turn((conversation(7), None), 1_000));
    }
  }

  fn conversation(raw: i64) -> ConversationId {
    ConversationId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn thread(raw: i64) -> ThreadId {
    ThreadId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn user(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
