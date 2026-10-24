//! When the "New messages" divider of the open conversation moves or goes away (#321). It marks
//! the first message that was unread when the conversation opened and stays put while the user
//! reads, so they can see where the new messages began. It goes away when the user sends in the
//! conversation, marks it read (Esc), leaves it, or has stayed at the bottom of the focused window
//! for a while; new messages that arrive while the user looks elsewhere move it to the first of
//! them. Pure: the wiring lives in the message list, the composer and `AppState`.

use std::time::Duration;

use crate::model::{ConversationId, TimestampMs};
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
