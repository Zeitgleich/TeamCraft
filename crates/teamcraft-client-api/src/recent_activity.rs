//! Where something happened last (#1906): the mobile Home's "Recent activity" across DMs, group
//! conversations and channels, the newest message first. The rule lives here so Android and iOS
//! order and cut the list the same way; the apps only draw the rows they already have.

use std::collections::HashSet;

/// How many conversations Home shows before "Show all".
pub const HOME_ROWS: usize = 6;

/// A conversation of the chat list as the rule needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate<'a> {
  pub id: &'a str,
  /// What the list names it; orders conversations with the same newest message time.
  pub name: &'a str,
  /// When its newest message held on the device was sent; `None` without one.
  pub maybe_last_activity_ms: Option<i64>,
  /// Archived channels are read-only and out of the lists (#212).
  pub is_archived: bool,
}

/// The IDs of the conversations with activity, the newest message first (ties by name, any case,
/// then by ID), at most `maybe_limit` of them. Conversations without a message on the device and
/// archived ones are left out; muted ones stay (the apps dim them). A conversation listed twice
/// counts once.
pub fn recent_activity(candidates: &[Candidate<'_>], maybe_limit: Option<usize>) -> Vec<String> {
  let mut seen = HashSet::new();
  let mut active: Vec<(&Candidate<'_>, i64, String)> = candidates
    .iter()
    .filter(|candidate| seen.insert(candidate.id))
    .filter(|candidate| !candidate.is_archived)
    .filter_map(|candidate| {
      candidate
        .maybe_last_activity_ms
        .map(|at| (candidate, at, candidate.name.to_lowercase()))
    })
    .collect();
  active.sort_by(|(left, left_at, left_name), (right, right_at, right_name)| {
    right_at
      .cmp(left_at)
      .then_with(|| left_name.cmp(right_name))
      .then_with(|| left.id.cmp(right.id))
  });
  active
    .into_iter()
    .take(maybe_limit.unwrap_or(usize::MAX))
    .map(|(candidate, _, _)| candidate.id.to_owned())
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  fn candidate<'a>(id: &'a str, name: &'a str, at: Option<i64>) -> Candidate<'a> {
    Candidate {
      id,
      name,
      maybe_last_activity_ms: at,
      is_archived: false,
    }
  }

  #[test]
  fn newest_message_first_across_kinds() {
    let list = [
      candidate("dm", "Alice", Some(10)),
      candidate("channel", "general", Some(30)),
      candidate("group", "Alice, Bob", Some(20)),
    ];
    assert_eq!(recent_activity(&list, None), ["channel", "group", "dm"]);
  }

  #[test]
  fn leaves_out_quiet_and_archived_conversations() {
    let mut archived = candidate("old", "old", Some(50));
    archived.is_archived = true;
    let list = [
      candidate("empty", "empty", None),
      archived,
      candidate("live", "live", Some(1)),
    ];
    assert_eq!(recent_activity(&list, None), ["live"]);
  }

  #[test]
  fn ties_by_name_then_id_and_once_each() {
    let list = [
      candidate("b", "beta", Some(5)),
      candidate("a2", "Alpha", Some(5)),
      candidate("a1", "alpha", Some(5)),
      candidate("b", "beta", Some(5)),
    ];
    assert_eq!(recent_activity(&list, None), ["a1", "a2", "b"]);
  }

  #[test]
  fn cuts_at_the_limit() {
    let list: Vec<_> = (0..10)
      .map(|at| Candidate {
        id: ["c0", "c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8", "c9"][at],
        name: "x",
        maybe_last_activity_ms: Some(at as i64),
        is_archived: false,
      })
      .collect();
    assert_eq!(
      recent_activity(&list, Some(HOME_ROWS)),
      ["c9", "c8", "c7", "c6", "c5", "c4"]
    );
    assert!(recent_activity(&list, Some(0)).is_empty());
  }
}
