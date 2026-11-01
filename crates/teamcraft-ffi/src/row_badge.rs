//! The unread badge of a chat list row for the mobile apps (#897): the rule is
//! `teamcraft_core::row_badge`, shared with desktop and web, so Kotlin and Swift only draw it.

use teamcraft_core::row_badge::{self as rule, RowBadge as RowBadgeRule};

/// The number next to a conversation's name.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct RowBadge {
  /// How many messages the badge counts, for the accessible name (a plural string).
  pub count: u32,
  /// What the badge shows: the count, or "99+".
  pub label: String,
  pub tone: RowBadgeTone,
}

/// How loud a [`RowBadge`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum RowBadgeTone {
  /// Red: unread messages in a conversation that notifies, or a mention in a muted one.
  Red,
  /// Grey: unread messages in a muted conversation.
  Quiet,
}

/// The badge a row shows, or none when nothing is unread.
#[uniffi::export]
pub fn row_badge(unread_count: u32, mention_count: u32, is_muted: bool) -> Option<RowBadge> {
  rule::row_badge(unread_count, mention_count, is_muted).map(RowBadge::from)
}

impl From<RowBadgeRule> for RowBadge {
  fn from(badge: RowBadgeRule) -> Self {
    Self {
      count: badge.count,
      label: badge.label(),
      tone: match badge.tone {
        rule::RowBadgeTone::Red => RowBadgeTone::Red,
        rule::RowBadgeTone::Quiet => RowBadgeTone::Quiet,
      },
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn exposes_count_label_and_tone() {
    assert_eq!(row_badge(0, 0, false), None);
    assert_eq!(
      row_badge(150, 0, false),
      Some(RowBadge {
        count: 150,
        label: "99+".into(),
        tone: RowBadgeTone::Red
      })
    );
    assert_eq!(
      row_badge(4, 0, true),
      Some(RowBadge {
        count: 4,
        label: "4".into(),
        tone: RowBadgeTone::Quiet
      })
    );
    assert_eq!(
      row_badge(4, 2, true),
      Some(RowBadge {
        count: 2,
        label: "2".into(),
        tone: RowBadgeTone::Red
      })
    );
  }
}
