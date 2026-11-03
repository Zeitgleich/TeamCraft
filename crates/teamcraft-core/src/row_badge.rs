//! The unread number next to a conversation's name in the chat lists (#891, #897), decided once for
//! desktop, web, Android and iOS (docs/notifications.md).

/// The largest number a badge writes out; more reads as "99+" so the badge keeps a stable width.
pub const MAX_BADGE_COUNT: u32 = 99;

/// The number next to a conversation's name in a chat list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowBadge {
  pub count: u32,
  pub tone: RowBadgeTone,
}

/// How loud a [`RowBadge`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowBadgeTone {
  /// Red: unread messages in a conversation that notifies, or a mention in a muted one.
  Red,
  /// Grey: unread messages in a muted conversation.
  Quiet,
}

impl RowBadge {
  /// The text on the badge: the count, or "99+" above [`MAX_BADGE_COUNT`].
  pub fn label(&self) -> String {
    if self.count > MAX_BADGE_COUNT {
      format!("{MAX_BADGE_COUNT}+")
    } else {
      self.count.to_string()
    }
  }
}

/// What a chat list row shows next to its name: the number of unread messages, red. A muted
/// conversation never gets the red count of its plain unread messages: it shows them grey, and
/// only a mention makes it red again, counting the mentions (docs/notifications.md).
pub fn row_badge(unread_count: u32, mention_count: u32, is_muted: bool) -> Option<RowBadge> {
  let (count, tone) = if is_muted && mention_count > 0 {
    (mention_count, RowBadgeTone::Red)
  } else if is_muted {
    (unread_count, RowBadgeTone::Quiet)
  } else {
    // A mention is an unread message; a count that lags behind it doesn't hide it.
    (unread_count.max(mention_count), RowBadgeTone::Red)
  };
  (count > 0).then_some(RowBadge { count, tone })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_conversation_that_notifies_shows_its_unread_count_in_red() {
    assert_eq!(row_badge(0, 0, false), None);
    assert_eq!(row_badge(1, 0, false), red(1));
    assert_eq!(row_badge(99, 0, false), red(99));
    assert_eq!(
      row_badge(100, 0, false),
      red(100),
      "the badge caps the text, not the count"
    );
    assert_eq!(
      row_badge(5, 2, false),
      red(5),
      "mentions are part of the unread messages"
    );
    assert_eq!(
      row_badge(0, 2, false),
      red(2),
      "a mention is never hidden by a lagging count"
    );
  }

  #[test]
  fn a_muted_conversation_is_grey_until_somebody_mentions_you() {
    assert_eq!(row_badge(0, 0, true), None);
    assert_eq!(
      row_badge(7, 0, true),
      Some(RowBadge {
        count: 7,
        tone: RowBadgeTone::Quiet
      })
    );
    assert_eq!(
      row_badge(7, 1, true),
      red(1),
      "a muted conversation counts only its mentions in red"
    );
  }

  #[test]
  fn the_label_caps_at_99_plus() {
    assert_eq!(label(1), "1");
    assert_eq!(label(99), "99");
    assert_eq!(label(100), "99+");
    assert_eq!(label(1000), "99+");
  }

  fn red(count: u32) -> Option<RowBadge> {
    Some(RowBadge {
      count,
      tone: RowBadgeTone::Red,
    })
  }

  fn label(count: u32) -> String {
    RowBadge {
      count,
      tone: RowBadgeTone::Red,
    }
    .label()
  }
}
