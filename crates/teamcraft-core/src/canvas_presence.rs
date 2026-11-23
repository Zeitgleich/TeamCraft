//! Who has a page of a channel's notes open, and where their caret is (issue #1456, e2ee.md
//! §4.11, "Who has the page open"), shared by the shells like [`crate::typing`]: the other devices'
//! words ([`CanvasPresenceState`], filled from `canvas_presence` events once opened), the
//! numbering of this device's own ([`CanvasPresenceAnnouncer`]), and when a shell that shows a page
//! without a caret (the phones) says it has the page open ([`CanvasPresencePacer`]).
//!
//! Ephemeral on both sides: the server relays the sealed words without numbering or buffering
//! them, and the clients keep them in memory only. A device that stops saying so (closed the app,
//! lost its connection) runs out after [`CANVAS_PRESENCE_TIMEOUT_MS`] by itself.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use teamcraft_proto::{
  CANVAS_PRESENCE_INTERVAL_MS, CANVAS_PRESENCE_REFRESH_MS, CANVAS_PRESENCE_TIMEOUT_MS, CanvasCaret,
  CanvasPageId, CanvasPageState, CanvasPeer, CanvasPresence, CanvasPresenceNote, ConversationId, DeviceId,
  UserId,
};

use crate::api::{GatewayConnection, ServerApi};
use crate::client::Client;
use crate::connection::ConnectionSupervisor;
use crate::store::LocalStore;

/// The most other devices remembered at once over all conversations; beyond, the one heard from
/// longest ago goes first. Far above any page's audience, and a member can't make it grow.
const MAX_REMEMBERED_DEVICES: usize = 512;

/// Which device said something about which channel's notes.
type PeerKey = (ConversationId, UserId, DeviceId);

/// The other devices' last words, with the local time each arrived. Cloning shares it: a UI adapter
/// keeps a clone to read while the sync loop owns the client.
#[derive(Clone, Debug, Default)]
pub struct CanvasPresenceState {
  seen: Arc<Mutex<HashMap<PeerKey, Heard>>>,
}

/// A device's last word about a channel's notes.
#[derive(Clone, Debug)]
struct Heard {
  seq: u64,
  maybe_page: Option<CanvasPageId>,
  is_open: bool,
  maybe_caret: Option<CanvasCaret>,
  /// The local time it arrived: the sender's clock is never asked.
  at_ms: i64,
}

/// Numbers this device's words, so the others can drop a replayed older one. Each shell's loop
/// keeps one.
#[derive(Debug, Default)]
pub struct CanvasPresenceAnnouncer {
  last_seq: u64,
}

/// When a device that shows a page without a caret of its own (the phones, read-only) says it has
/// the page open: the editor's rules without the caret. It says so when the page opens, every
/// [`CANVAS_PRESENCE_REFRESH_MS`] while it is shown, and as soon as another device appears (so that
/// one learns of it without waiting for the refresh), never twice within
/// [`CANVAS_PRESENCE_INTERVAL_MS`]. The shell asks on its own tick and whenever the core changed,
/// only while the page is shown, and says "closed" itself when it leaves the page.
#[derive(Debug, Default)]
pub struct CanvasPresencePacer {
  maybe_said_at_ms: Option<i64>,
  /// The other devices that had the page open when this one last said so.
  known: BTreeSet<(UserId, DeviceId)>,
}

impl CanvasPresenceState {
  /// The devices of users other than `me` with `maybe_page` of `conversation` open at `now_ms`,
  /// sorted by user and device.
  pub fn peers(
    &self,
    conversation: ConversationId,
    maybe_page: Option<&CanvasPageId>,
    me: UserId,
    now_ms: i64,
  ) -> Vec<CanvasPeer> {
    let mut peers: Vec<CanvasPeer> = self
      .lock()
      .iter()
      .filter(|((heard_in, user, _), heard)| {
        *heard_in == conversation
          && *user != me
          && heard.is_open
          && heard.maybe_page.as_ref() == maybe_page
          && is_current(heard.at_ms, now_ms)
      })
      .map(|((_, user, device), heard)| CanvasPeer {
        user: *user,
        device: *device,
        maybe_caret: heard.maybe_caret.clone(),
        heard_at_ms: heard.at_ms,
      })
      .collect();
    peers.sort_by_key(|peer| (peer.user, peer.device));
    peers
  }

  /// Notes what `device` of `user` said about `conversation`'s notes at local time `now_ms`;
  /// whether what [`Self::peers`] answers may have changed. A word that breaks the rules, is of a
  /// state this version doesn't know, or isn't newer than the device's last one is dropped. An
  /// entry older than the timeout no longer blocks a lower number: the sender's counter starts
  /// from its clock, which may have gone back since.
  pub(crate) fn note(
    &self,
    conversation: ConversationId,
    user: UserId,
    device: DeviceId,
    word: CanvasPresence,
    now_ms: i64,
  ) -> bool {
    if !word.is_valid() {
      return false;
    }
    let is_open = match word.state {
      CanvasPageState::Open => true,
      CanvasPageState::Closed => false,
      CanvasPageState::Unknown => return false,
    };
    let mut seen = self.lock();
    let key = (conversation, user, device);
    if seen
      .get(&key)
      .is_some_and(|heard| heard.seq >= word.seq && is_current(heard.at_ms, now_ms))
    {
      return false;
    }
    seen.insert(
      key,
      Heard {
        seq: word.seq,
        maybe_page: word.maybe_page,
        is_open,
        maybe_caret: word.maybe_caret,
        at_ms: now_ms,
      },
    );
    seen.retain(|_, heard| is_current(heard.at_ms, now_ms));
    while seen.len() > MAX_REMEMBERED_DEVICES {
      let Some(oldest) = seen
        .iter()
        .min_by_key(|(_, heard)| heard.at_ms)
        .map(|(key, _)| *key)
      else {
        break;
      };
      seen.remove(&oldest);
    }
    true
  }

  /// A member left the conversation (or a device was removed): their words go at once.
  pub(crate) fn forget_user(&self, conversation: ConversationId, user: UserId) {
    self
      .lock()
      .retain(|(heard_in, heard_user, _), _| !(*heard_in == conversation && *heard_user == user));
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<PeerKey, Heard>> {
    // A panic while holding the lock leaves a plain map behind; it stays usable.
    self
      .seen
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
  }
}

impl CanvasPresenceAnnouncer {
  /// Seals `note` about `conversation`'s notes and sends it on the open connection. Offline it is
  /// dropped, not queued: the editor says it again on its next refresh.
  pub async fn announce<A: ServerApi, S: LocalStore, G: GatewayConnection>(
    &mut self,
    client: &Client<A, S>,
    connection: &mut ConnectionSupervisor<G>,
    conversation: ConversationId,
    note: CanvasPresenceNote,
    now_ms: i64,
  ) {
    if !connection.is_connected() {
      return;
    }
    let word = self.word(note, now_ms);
    let frame = match client.canvas_presence_frame(conversation, &word) {
      Ok(frame) => frame,
      Err(error) => {
        tracing::debug!(conversation_id = %conversation, %error, "canvas presence not sealed");
        return;
      },
    };
    // A failed send replaces the connection; the editor's next refresh says it again.
    if !connection.send(now_ms, &frame).await {
      tracing::debug!("canvas presence not sent");
    }
  }

  /// The word to seal for `note` at `now_ms`: numbered above every earlier one of this device, and
  /// from the clock where it can be, so a restarted app continues above its last session.
  pub fn word(&mut self, note: CanvasPresenceNote, now_ms: i64) -> CanvasPresence {
    let from_clock = u64::try_from(now_ms).unwrap_or(0);
    self.last_seq = from_clock.max(self.last_seq.saturating_add(1));
    CanvasPresence {
      seq: self.last_seq,
      maybe_page: note.maybe_page,
      state: note.state,
      maybe_caret: note.maybe_caret,
    }
  }
}

impl CanvasPresencePacer {
  /// Whether to say "open" now, with `peers` the other devices that have the page open at
  /// `now_ms`; noted as said when it is. After the clock went back it is said again (the last word
  /// would otherwise hold it back for as long as the jump).
  pub fn is_due(&mut self, peers: &[CanvasPeer], now_ms: i64) -> bool {
    let devices: BTreeSet<(UserId, DeviceId)> = peers.iter().map(|peer| (peer.user, peer.device)).collect();
    let has_new = devices.iter().any(|device| !self.known.contains(device));
    let is_due = match self.maybe_said_at_ms.map(|at| now_ms.saturating_sub(at)) {
      None => true,
      Some(elapsed) if elapsed < 0 => true,
      Some(elapsed) => {
        elapsed >= CANVAS_PRESENCE_REFRESH_MS || (has_new && elapsed >= CANVAS_PRESENCE_INTERVAL_MS)
      },
    };
    // A device that appeared within the interval is still new on the next ask.
    if is_due {
      self.maybe_said_at_ms = Some(now_ms);
      self.known = devices;
    }
    is_due
  }
}

/// Whether a word heard at `at_ms` still counts at `now_ms`. One from the "future" (the local clock
/// went back since) does not: the device's next refresh brings it back.
fn is_current(at_ms: i64, now_ms: i64) -> bool {
  (0..CANVAS_PRESENCE_TIMEOUT_MS).contains(&now_ms.saturating_sub(at_ms))
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
    fn an_open_page_shows_for_the_timeout_and_never_oneself() {
      let presence = CanvasPresenceState::default();
      assert!(presence.note(conversation(7), user(ALICE), device(20), open(1, None), 1_000));
      assert!(presence.note(conversation(7), user(ME), device(10), open(1, None), 1_000));
      let peers = presence.peers(conversation(7), None, user(ME), 2_000);
      assert_eq!(peers.len(), 1);
      assert_eq!((peers[0].user, peers[0].device), (user(ALICE), device(20)));
      assert!(
        presence
          .peers(
            conversation(7),
            None,
            user(ME),
            1_000 + CANVAS_PRESENCE_TIMEOUT_MS
          )
          .is_empty()
      );
    }

    #[test]
    fn peers_are_per_page_and_conversation() {
      let presence = CanvasPresenceState::default();
      let page = CanvasPageId::parse("ab-1").unwrap();
      presence.note(
        conversation(7),
        user(ALICE),
        device(20),
        open(1, Some(page.clone())),
        0,
      );
      presence.note(conversation(7), user(BOB), device(30), open(1, None), 0);
      presence.note(conversation(8), user(BOB), device(31), open(1, None), 0);
      let first = presence.peers(conversation(7), None, user(ME), 10);
      assert_eq!(
        first.iter().map(|peer| peer.user).collect::<Vec<_>>(),
        [user(BOB)]
      );
      let further = presence.peers(conversation(7), Some(&page), user(ME), 10);
      assert_eq!(
        further.iter().map(|peer| peer.user).collect::<Vec<_>>(),
        [user(ALICE)]
      );
    }

    #[test]
    fn closing_removes_at_once_and_a_replayed_open_doesnt_bring_it_back() {
      let presence = CanvasPresenceState::default();
      presence.note(conversation(7), user(ALICE), device(20), open(5, None), 0);
      let closed = CanvasPresence {
        state: CanvasPageState::Closed,
        ..open(6, None)
      };
      assert!(presence.note(conversation(7), user(ALICE), device(20), closed, 10));
      assert!(presence.peers(conversation(7), None, user(ME), 20).is_empty());
      assert!(!presence.note(conversation(7), user(ALICE), device(20), open(5, None), 30));
      assert!(presence.peers(conversation(7), None, user(ME), 40).is_empty());
    }

    #[test]
    fn a_caret_moves_only_forward_in_the_devices_numbering() {
      let presence = CanvasPresenceState::default();
      presence.note(
        conversation(7),
        user(ALICE),
        device(20),
        with_caret(2, "b1", 4),
        0,
      );
      assert!(!presence.note(
        conversation(7),
        user(ALICE),
        device(20),
        with_caret(1, "b1", 0),
        10
      ));
      let peers = presence.peers(conversation(7), None, user(ME), 20);
      assert_eq!(peers[0].maybe_caret.as_ref().map(|caret| caret.start), Some(4));
      // Once the entry ran out, a lower number counts again (the sender's clock went back).
      assert!(presence.note(
        conversation(7),
        user(ALICE),
        device(20),
        with_caret(1, "b2", 1),
        CANVAS_PRESENCE_TIMEOUT_MS + 10
      ));
    }

    #[test]
    fn words_breaking_the_rules_or_of_an_unknown_state_are_dropped() {
      let presence = CanvasPresenceState::default();
      assert!(!presence.note(
        conversation(7),
        user(ALICE),
        device(20),
        with_caret(1, "../x", 0),
        0
      ));
      let unknown = CanvasPresence {
        state: CanvasPageState::Unknown,
        ..open(2, None)
      };
      assert!(!presence.note(conversation(7), user(ALICE), device(20), unknown, 0));
      assert!(presence.peers(conversation(7), None, user(ME), 0).is_empty());
    }

    #[test]
    fn a_word_from_the_future_of_a_clock_gone_back_doesnt_show() {
      let presence = CanvasPresenceState::default();
      presence.note(conversation(7), user(ALICE), device(20), open(1, None), 10_000);
      assert!(presence.peers(conversation(7), None, user(ME), 5_000).is_empty());
    }

    #[test]
    fn memory_stays_bounded_and_the_oldest_go_first() {
      let presence = CanvasPresenceState::default();
      for index in 0..(MAX_REMEMBERED_DEVICES as i64 + 10) {
        presence.note(
          conversation(7),
          user(100 + index),
          device(1),
          open(1, None),
          index,
        );
      }
      assert_eq!(presence.lock().len(), MAX_REMEMBERED_DEVICES);
      let peers = presence.peers(conversation(7), None, user(ME), 1_000);
      assert!(!peers.iter().any(|peer| peer.user == user(100)));
    }

    #[test]
    fn a_member_who_left_goes_at_once() {
      let presence = CanvasPresenceState::default();
      presence.note(conversation(7), user(ALICE), device(20), open(1, None), 0);
      presence.forget_user(conversation(7), user(ALICE));
      assert!(presence.peers(conversation(7), None, user(ME), 10).is_empty());
    }
  }

  mod announcer {
    use super::*;

    #[test]
    fn words_are_numbered_upwards_even_when_the_clock_goes_back() {
      let mut announcer = CanvasPresenceAnnouncer::default();
      let first = announcer.word(note(), 5_000).seq;
      let second = announcer.word(note(), 5_000).seq;
      let after_jump = announcer.word(note(), 1_000).seq;
      assert_eq!(first, 5_000);
      assert!(second > first && after_jump > second);
    }

    fn note() -> CanvasPresenceNote {
      CanvasPresenceNote {
        maybe_page: None,
        state: CanvasPageState::Open,
        maybe_caret: None,
      }
    }
  }

  mod pacer {
    use super::*;

    #[test]
    fn says_open_at_once_then_on_the_refresh() {
      let mut pacer = CanvasPresencePacer::default();
      assert!(pacer.is_due(&[], 1_000));
      assert!(!pacer.is_due(&[], 11_000));
      assert!(!pacer.is_due(&[], 1_000 + CANVAS_PRESENCE_REFRESH_MS - 1));
      assert!(pacer.is_due(&[], 1_000 + CANVAS_PRESENCE_REFRESH_MS));
    }

    #[test]
    fn a_new_device_hears_of_it_at_once_but_not_within_the_interval() {
      let mut pacer = CanvasPresencePacer::default();
      assert!(pacer.is_due(&[], 0));
      let alice = [peer(ALICE, 20)];
      assert!(!pacer.is_due(&alice, CANVAS_PRESENCE_INTERVAL_MS - 1));
      // Still new on the next ask, once the interval allows.
      assert!(pacer.is_due(&alice, CANVAS_PRESENCE_INTERVAL_MS));
      assert!(!pacer.is_due(&alice, 5_000));
      // A further device of the same user is new too.
      assert!(pacer.is_due(&[peer(ALICE, 20), peer(ALICE, 21)], 6_000));
    }

    #[test]
    fn a_device_that_left_is_no_reason_to_speak() {
      let mut pacer = CanvasPresencePacer::default();
      assert!(pacer.is_due(&[peer(ALICE, 20), peer(BOB, 30)], 0));
      assert!(!pacer.is_due(&[peer(BOB, 30)], 5_000));
    }

    #[test]
    fn the_clock_going_back_says_it_again() {
      let mut pacer = CanvasPresencePacer::default();
      assert!(pacer.is_due(&[], 50_000));
      assert!(pacer.is_due(&[], 10_000));
      assert!(!pacer.is_due(&[], 12_000));
    }

    fn peer(user_id: i64, device_id: i64) -> CanvasPeer {
      CanvasPeer {
        user: user(user_id),
        device: device(device_id),
        maybe_caret: None,
        heard_at_ms: 0,
      }
    }
  }

  fn open(seq: u64, maybe_page: Option<CanvasPageId>) -> CanvasPresence {
    CanvasPresence {
      seq,
      maybe_page,
      state: CanvasPageState::Open,
      maybe_caret: None,
    }
  }

  fn with_caret(seq: u64, block: &str, at: u32) -> CanvasPresence {
    CanvasPresence {
      maybe_caret: Some(CanvasCaret {
        block: block.to_owned(),
        start: at,
        end: at,
      }),
      ..open(seq, None)
    }
  }

  fn conversation(id: i64) -> ConversationId {
    ConversationId::from_snowflake(Snowflake::new(id).unwrap())
  }

  fn user(id: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(id).unwrap())
  }

  fn device(id: i64) -> DeviceId {
    DeviceId::from_snowflake(Snowflake::new(id).unwrap())
  }
}
