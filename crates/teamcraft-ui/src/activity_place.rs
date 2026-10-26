//! Activity as a place of its own (#752): what is addressed to the user, in one list with filters.
//! Selecting it in the rail swaps the sidebar for this list, like Chats or a space; an entry opens
//! its conversation (or thread) beside it. Threads the user follows are one of its filters, not a
//! place of their own.

use teamcraft_client_api::activity::{ActivityItem, ActivityKind};

/// The filters of the activity list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ActivityFilter {
  /// Mentions, reactions and thread replies together.
  #[default]
  All,
  Mentions,
  /// The threads the user follows, with their new replies.
  Threads,
  /// Reactions to the user's messages.
  Reactions,
}

impl ActivityFilter {
  pub const ALL: [Self; 4] = [Self::All, Self::Mentions, Self::Threads, Self::Reactions];

  pub fn label_key(self) -> &'static str {
    match self {
      Self::All => "activity-all",
      Self::Mentions => "activity-mentions",
      Self::Threads => "activity-threads",
      Self::Reactions => "activity-reactions",
    }
  }

  /// What the list shows when nothing is there yet, per filter: what would show up.
  pub fn empty_keys(self) -> (&'static str, &'static str) {
    match self {
      Self::All => ("activity-empty-title", "activity-empty"),
      Self::Mentions => ("activity-empty-mentions-title", "activity-empty-mentions"),
      Self::Threads => ("activity-empty-threads-title", "activity-empty-threads"),
      Self::Reactions => ("activity-empty-reactions-title", "activity-empty-reactions"),
    }
  }

  /// Whether the filter lists the followed threads rather than activity entries.
  pub fn lists_threads(self) -> bool {
    self == Self::Threads
  }

  /// Whether an activity entry belongs under this filter.
  pub fn includes(self, item: &ActivityItem) -> bool {
    match self {
      Self::All => true,
      Self::Mentions => item.kind == ActivityKind::Mention,
      Self::Threads => item.kind == ActivityKind::ThreadReply,
      Self::Reactions => matches!(item.kind, ActivityKind::Reaction { .. }),
    }
  }
}

/// What an activity entry says happened (with `name`, `channel` and `emoji`).
pub fn summary_key(kind: &ActivityKind) -> &'static str {
  match kind {
    ActivityKind::Mention => "activity-mentioned",
    ActivityKind::Reaction { .. } => "activity-reacted",
    ActivityKind::ThreadReply => "activity-replied",
  }
}

/// What a followed thread's entry says: its new replies, or that it is quiet.
pub fn thread_summary_key(unread_replies: u32) -> &'static str {
  if unread_replies > 0 {
    "activity-thread-new"
  } else {
    "activity-thread-quiet"
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_client_api::activity::ActivityKind;

  use super::*;

  #[test]
  fn each_filter_keeps_its_kind_and_all_keeps_everything() {
    let mention = ActivityKind::Mention;
    let reaction = ActivityKind::Reaction { emoji: "+1".into() };
    let reply = ActivityKind::ThreadReply;
    let cases = [
      (ActivityFilter::All, [true, true, true]),
      (ActivityFilter::Mentions, [true, false, false]),
      (ActivityFilter::Reactions, [false, true, false]),
      (ActivityFilter::Threads, [false, false, true]),
    ];
    for (filter, expected) in cases {
      let got = [&mention, &reaction, &reply].map(|kind| filter.includes(&item(kind.clone())));
      assert_eq!(got, expected, "{filter:?}");
    }
  }

  #[test]
  fn only_the_threads_filter_lists_followed_threads() {
    let listing: Vec<_> = ActivityFilter::ALL
      .into_iter()
      .filter(|filter| filter.lists_threads())
      .collect();
    assert_eq!(listing, [ActivityFilter::Threads]);
  }

  #[test]
  fn every_filter_has_its_own_empty_state() {
    let mut keys: Vec<_> = ActivityFilter::ALL
      .into_iter()
      .map(ActivityFilter::empty_keys)
      .collect();
    keys.dedup();
    assert_eq!(keys.len(), ActivityFilter::ALL.len());
  }

  #[test]
  fn a_thread_entry_says_whether_replies_are_new() {
    assert_eq!(thread_summary_key(2), "activity-thread-new");
    assert_eq!(thread_summary_key(0), "activity-thread-quiet");
  }

  #[test]
  fn every_key_is_translated() {
    use crate::i18n::{Locale, translate};

    let mut keys: Vec<&str> = ActivityFilter::ALL
      .into_iter()
      .flat_map(|filter| {
        let (title, body) = filter.empty_keys();
        [filter.label_key(), title, body]
      })
      .collect();
    keys.extend([
      summary_key(&ActivityKind::Mention),
      summary_key(&ActivityKind::Reaction { emoji: String::new() }),
      summary_key(&ActivityKind::ThreadReply),
      thread_summary_key(1),
      thread_summary_key(0),
      "activity-place-title",
      "activity-place-body",
      "rail-activity-tip",
    ]);
    for locale in [Locale::En, Locale::De] {
      for key in &keys {
        assert_ne!(&translate(locale, key, None), key, "{locale:?} lacks {key}");
      }
    }
  }

  fn item(kind: ActivityKind) -> ActivityItem {
    use crate::model::id_from_raw;

    ActivityItem {
      id: "a1".into(),
      kind,
      conversation: id_from_raw(1),
      message: id_from_raw(2),
      maybe_thread: None,
      actor: id_from_raw(3),
      at: 0,
      excerpt: String::new(),
      maybe_read_through: None,
      is_unread: true,
    }
  }
}
