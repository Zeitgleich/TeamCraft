//! How a sent message is shown while the client core still has it in its outbox. The core retries on
//! its own; the UI only tells the user when a message is taking longer than usual.

use crate::model::{MessageId, TimestampMs, is_pending};

/// After this long in the outbox, "Sending…" turns into "Not sent yet".
pub const DELAYED_AFTER_MS: i64 = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
  /// Confirmed by the server.
  Sent,
  /// In the outbox, sent moments ago.
  Sending,
  /// Still in the outbox after [`DELAYED_AFTER_MS`]: offline or the server is unreachable.
  Delayed,
}

impl Delivery {
  pub fn of(id: MessageId, sent_at: TimestampMs, now: TimestampMs) -> Self {
    if !is_pending(id) {
      Self::Sent
    } else if now.saturating_sub(sent_at) >= DELAYED_AFTER_MS {
      Self::Delayed
    } else {
      Self::Sending
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::{PENDING_ID_BIT, id_from_raw};

  fn pending() -> MessageId {
    id_from_raw(u64::try_from(PENDING_ID_BIT | 9).unwrap_or(0))
  }

  #[test]
  fn confirmed_messages_are_sent() {
    assert_eq!(Delivery::of(id_from_raw(9), 0, 1_000_000), Delivery::Sent);
  }

  #[test]
  fn pending_messages_turn_delayed_after_a_while() {
    assert_eq!(Delivery::of(pending(), 1_000, 1_000), Delivery::Sending);
    assert_eq!(
      Delivery::of(pending(), 1_000, 1_000 + DELAYED_AFTER_MS - 1),
      Delivery::Sending
    );
    assert_eq!(
      Delivery::of(pending(), 1_000, 1_000 + DELAYED_AFTER_MS),
      Delivery::Delayed
    );
  }

  #[test]
  fn a_clock_behind_the_message_counts_as_just_sent() {
    assert_eq!(Delivery::of(pending(), 5_000, 1_000), Delivery::Sending);
  }
}
