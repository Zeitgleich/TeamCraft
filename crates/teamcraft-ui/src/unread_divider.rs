//! When the "New messages" divider of the open conversation moves or goes away (#321). It marks
//! the first message that was unread when the conversation opened and stays put while the user
//! reads, so they can see where the new messages began. It goes away when the user sends in the
//! conversation, marks it read (Esc), leaves it, or has stayed at the bottom of the focused window
//! for a while; new messages that arrive while the user looks elsewhere move it to the first of
//! them. Pure: the wiring lives in the message list, the composer and `AppState`.
//!
//! The read marker and the unread badge don't wait for the divider: whatever is unread while the
//! user watches the newest message is read at once ([`is_reading_latest`], [`ReadMarkerGate`]).

use std::time::Duration;

use crate::model::{ConversationId, MessageId, TimestampMs};
use crate::state::UnreadMarker;

/// What happened in a conversation, as far as its divider cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DividerEvent {
  /// The user sent a message there (or a thread reply).
  Sent,
  /// Esc or "mark as read".
  MarkedRead,
  /// The user left the conversation; the next open places the divider from the read marker.
  Left,
  /// The user stayed at the bottom of the focused window for the dwell time.
  DwelledAtBottom,
  /// Messages arrived while the window was blurred or hidden; `first_new` is the index of the
  /// first of them. `has_seen_divider` says whether the user had reached the bottom (so the old
  /// divider was read) before looking away.
  ArrivedWhileAway {
    first_new: usize,
    is_own: bool,
    has_seen_divider: bool,
  },
}

/// The divider after `event` in `conversation`. Events of another conversation leave it alone.
pub fn next_divider(
  maybe_marker: Option<UnreadMarker>,
  conversation: ConversationId,
  event: DividerEvent,
) -> Option<UnreadMarker> {
  let is_here = maybe_marker.is_some_and(|marker| marker.conversation == conversation);
  match event {
    DividerEvent::Sent | DividerEvent::MarkedRead | DividerEvent::Left | DividerEvent::DwelledAtBottom => {
      if is_here {
        None
      } else {
        maybe_marker
      }
    },
    // The user's own messages (from another device) are no news to them.
    DividerEvent::ArrivedWhileAway { is_own: true, .. } => maybe_marker,
    DividerEvent::ArrivedWhileAway {
      first_new,
      has_seen_divider,
      ..
    } => {
      // An unread divider the user hasn't reached yet still marks where the news begins.
      if is_here && !has_seen_divider {
        return maybe_marker;
      }
      Some(UnreadMarker {
        conversation,
        index: first_new,
      })
    },
  }
}

/// Whether the open conversation's unread messages are read right now: something is unread and the
/// user sees its newest message, at the bottom of the focused, visible window. The divider stays
/// until the dwell (it shows where the news began); the badge and the read marker don't wait.
pub fn is_reading_latest(has_unread: bool, is_at_bottom: bool, is_focused: bool) -> bool {
  has_unread && is_at_bottom && is_focused
}

/// The newest message the open conversation's read marker was sent for, so each one is sent once.
/// The server's `unread_update` can lag behind a read position it hasn't applied yet and raise the
/// count again; marking read again for the same message would only queue another request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadMarkerGate {
  maybe_sent: Option<MessageId>,
}

/// How long the user has sat at the bottom of the focused conversation with a divider shown,
/// against an injected clock (the message list feeds it `now` and arms one timer).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DwellClock {
  maybe_since: Option<TimestampMs>,
}

impl DwellClock {
  /// Whether the user is reading at the bottom right now; leaving the bottom starts over.
  pub fn update(&mut self, is_reading: bool, now: TimestampMs) {
    self.maybe_since = match (is_reading, self.maybe_since) {
      (false, _) => None,
      (true, Some(since)) => Some(since),
      (true, None) => Some(now),
    };
  }

  /// What is left of `dwell`; `None` while the user isn't reading at the bottom, zero once due.
  pub fn remaining(&self, now: TimestampMs, dwell: Duration) -> Option<Duration> {
    let since = self.maybe_since?;
    let elapsed = Duration::from_millis(u64::try_from(now.saturating_sub(since)).unwrap_or(0));
    Some(dwell.saturating_sub(elapsed))
  }
}

impl ReadMarkerGate {
  /// Whether to mark read now: something is unread through `maybe_unread_through` (the newest
  /// message the server stored, reactions included), the user reads at the bottom of the focused window ([`is_reading_latest`]),
  /// and that message is newer than the one sent last. Remembers it when so. With nothing confirmed
  /// to point at yet it waits for the message to arrive.
  pub fn should_mark(
    &mut self,
    maybe_unread_through: Option<MessageId>,
    is_at_bottom: bool,
    is_focused: bool,
  ) -> bool {
    let Some(newest) = maybe_unread_through else {
      return false;
    };
    let is_sent = self.maybe_sent.is_some_and(|sent| sent >= newest);
    if !is_reading_latest(true, is_at_bottom, is_focused) || is_sent {
      return false;
    }
    self.maybe_sent = Some(newest);
    true
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::id_from_raw;

  #[test]
  fn sending_reading_leaving_and_dwelling_remove_the_divider() {
    for event in [
      DividerEvent::Sent,
      DividerEvent::MarkedRead,
      DividerEvent::Left,
      DividerEvent::DwelledAtBottom,
    ] {
      assert_eq!(next_divider(Some(marker(1, 5)), here(), event), None, "{event:?}");
    }
  }

  #[test]
  fn events_elsewhere_leave_the_divider_alone() {
    let other = id_from_raw::<ConversationId>(2);
    assert_eq!(
      next_divider(Some(marker(1, 5)), other, DividerEvent::Sent),
      Some(marker(1, 5))
    );
    assert_eq!(next_divider(None, here(), DividerEvent::Left), None);
  }

  #[test]
  fn news_while_away_moves_the_divider_to_the_first_of_them() {
    let away = |has_seen_divider| DividerEvent::ArrivedWhileAway {
      first_new: 9,
      is_own: false,
      has_seen_divider,
    };
    assert_eq!(next_divider(None, here(), away(false)), Some(marker(1, 9)));
    assert_eq!(
      next_divider(Some(marker(1, 5)), here(), away(true)),
      Some(marker(1, 9))
    );
    // Not read up to the old divider yet: that one still marks the start of the news.
    assert_eq!(
      next_divider(Some(marker(1, 5)), here(), away(false)),
      Some(marker(1, 5))
    );
  }

  #[test]
  fn own_messages_from_another_device_place_no_divider() {
    let own = DividerEvent::ArrivedWhileAway {
      first_new: 9,
      is_own: true,
      has_seen_divider: true,
    };
    assert_eq!(next_divider(None, here(), own), None);
  }

  mod reading_latest {
    use super::*;

    #[test]
    fn unread_messages_in_view_of_the_focused_window_are_read() {
      assert!(is_reading_latest(true, true, true));
    }

    #[test]
    fn nothing_is_read_while_scrolled_up_or_looking_elsewhere() {
      assert!(!is_reading_latest(true, false, true), "scrolled up");
      assert!(!is_reading_latest(true, true, false), "window blurred or hidden");
      assert!(!is_reading_latest(true, false, false));
    }

    #[test]
    fn nothing_unread_sends_no_read_marker() {
      for (is_at_bottom, is_focused) in [(true, true), (true, false), (false, true), (false, false)] {
        assert!(!is_reading_latest(false, is_at_bottom, is_focused));
      }
    }
  }

  mod read_marker_gate {
    use super::*;

    #[test]
    fn a_message_that_arrives_while_reading_is_marked_once() {
      let mut feed = FakeFeed::default();
      feed.arrive(5);
      assert_eq!(feed.mark_read_calls, 1);
      assert_eq!(feed.unread_count, 0);
    }

    #[test]
    fn a_reaction_while_reading_is_marked_at_once() {
      let mut feed = FakeFeed::default();
      feed.arrive(5);
      // #641: no new row, but a newer message on the server that raises the count.
      feed.react(6);
      assert_eq!(feed.mark_read_calls, 2);
      assert_eq!(feed.unread_count, 0);
    }

    #[test]
    fn a_reaction_while_away_waits_for_the_focus() {
      let mut feed = FakeFeed::default();
      feed.arrive(5);
      feed.is_focused = false;
      feed.react(6);
      assert_eq!((feed.mark_read_calls, feed.unread_count), (1, 1));
      feed.is_focused = true;
      feed.run();
      assert_eq!((feed.mark_read_calls, feed.unread_count), (2, 0));
    }

    #[test]
    fn a_lagging_count_for_the_same_message_marks_nothing_again() {
      let mut feed = FakeFeed::default();
      feed.arrive(5);
      // `unread_update` sent before the server applied the position: the count rises again.
      feed.server_counts(1);
      feed.server_counts(1);
      assert_eq!(feed.mark_read_calls, 1);
      // The server's answer to the position; then real news.
      feed.server_counts(0);
      feed.arrive(6);
      assert_eq!(feed.mark_read_calls, 2);
    }

    #[test]
    fn news_while_away_is_marked_when_the_user_comes_back() {
      let mut feed = FakeFeed {
        is_focused: false,
        ..FakeFeed::default()
      };
      feed.arrive(5);
      feed.arrive(6);
      assert_eq!(feed.mark_read_calls, 0);
      feed.is_focused = true;
      feed.run();
      assert_eq!(feed.mark_read_calls, 1);
      assert_eq!(feed.unread_count, 0);
    }

    #[test]
    fn nothing_is_marked_while_scrolled_up_or_with_nothing_confirmed() {
      let mut feed = FakeFeed {
        is_at_bottom: false,
        ..FakeFeed::default()
      };
      feed.arrive(5);
      assert_eq!(feed.mark_read_calls, 0);
      let mut gate = ReadMarkerGate::default();
      assert!(!gate.should_mark(None, true, true));
    }

    #[test]
    fn an_older_message_never_moves_it_back() {
      let mut gate = ReadMarkerGate::default();
      assert!(gate.should_mark(Some(message(9)), true, true));
      assert!(!gate.should_mark(Some(message(7)), true, true));
      assert!(!gate.should_mark(Some(message(9)), true, true));
      assert!(gate.should_mark(Some(message(10)), true, true));
    }

    /// The message list's wiring against a fake backend: the unread count, the newest stored
    /// message (reactions included) and the `mark_read` calls, re-run on every change like the effect.
    struct FakeFeed {
      gate: ReadMarkerGate,
      unread_count: u32,
      maybe_newest: Option<MessageId>,
      is_at_bottom: bool,
      is_focused: bool,
      mark_read_calls: usize,
    }

    impl Default for FakeFeed {
      fn default() -> Self {
        Self {
          gate: ReadMarkerGate::default(),
          unread_count: 0,
          maybe_newest: None,
          is_at_bottom: true,
          is_focused: true,
          mark_read_calls: 0,
        }
      }
    }

    impl FakeFeed {
      fn arrive(&mut self, raw: u64) {
        self.maybe_newest = Some(message(raw));
        self.unread_count += 1;
        self.run();
      }

      /// A reaction to a message on screen: stored under a newer server ID, counted as unread.
      fn react(&mut self, raw: u64) {
        self.arrive(raw);
      }

      fn server_counts(&mut self, unread_count: u32) {
        self.unread_count = unread_count;
        self.run();
      }

      fn run(&mut self) {
        let maybe_through = self.maybe_newest.filter(|_| self.unread_count > 0);
        if self
          .gate
          .should_mark(maybe_through, self.is_at_bottom, self.is_focused)
        {
          self.mark_read_calls += 1;
          // The backend clears the count right away.
          self.unread_count = 0;
        }
      }
    }

    fn message(raw: u64) -> MessageId {
      id_from_raw(raw)
    }
  }

  mod dwell {
    use super::*;

    const DWELL: Duration = Duration::from_secs(10);

    #[test]
    fn counts_from_reaching_the_bottom_and_is_due_after_the_dwell() {
      let mut clock = DwellClock::default();
      assert_eq!(clock.remaining(0, DWELL), None);
      clock.update(true, 1_000);
      clock.update(true, 5_000);
      assert_eq!(clock.remaining(5_000, DWELL), Some(Duration::from_secs(6)));
      assert_eq!(clock.remaining(11_000, DWELL), Some(Duration::ZERO));
      assert_eq!(clock.remaining(60_000, DWELL), Some(Duration::ZERO));
    }

    #[test]
    fn leaving_the_bottom_or_the_window_starts_over() {
      let mut clock = DwellClock::default();
      clock.update(true, 1_000);
      clock.update(false, 8_000);
      assert_eq!(clock.remaining(9_000, DWELL), None);
      clock.update(true, 9_000);
      assert_eq!(clock.remaining(10_000, DWELL), Some(Duration::from_secs(9)));
    }

    #[test]
    fn a_clock_running_backwards_does_not_fire_early() {
      let mut clock = DwellClock::default();
      clock.update(true, 10_000);
      assert_eq!(clock.remaining(4_000, DWELL), Some(DWELL));
    }
  }

  fn here() -> ConversationId {
    id_from_raw(1)
  }

  fn marker(conversation: u64, index: usize) -> UnreadMarker {
    UnreadMarker {
      conversation: id_from_raw(conversation),
      index,
    }
  }
}
