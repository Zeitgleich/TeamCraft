//! Home's "Recent activity" for the mobile apps (#1906): the rule is
//! `teamcraft_client_api::recent_activity`, so Kotlin and Swift only draw the rows.

use teamcraft_client_api::recent_activity::{self as rule, Candidate};

/// A chat list conversation as the order needs it: the apps have all of it at hand (the list and
/// its previews), so nothing is read from the store.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct RecentActivityCandidate {
  pub id: String,
  pub name: String,
  /// When the newest message on the device was sent (the preview's time); `None` without one.
  pub maybe_last_activity_ms: Option<i64>,
  pub is_archived: bool,
}

/// The IDs of the conversations with activity, the newest message first; at most `maybe_limit`.
#[uniffi::export]
pub fn recent_activity(candidates: Vec<RecentActivityCandidate>, maybe_limit: Option<u32>) -> Vec<String> {
  let borrowed: Vec<_> = candidates
    .iter()
    .map(|candidate| Candidate {
      id: &candidate.id,
      name: &candidate.name,
      maybe_last_activity_ms: candidate.maybe_last_activity_ms,
      is_archived: candidate.is_archived,
    })
    .collect();
  rule::recent_activity(&borrowed, maybe_limit.map(|limit| limit as usize))
}

/// How many conversations Home's "Recent activity" shows before "Show all".
#[uniffi::export]
pub fn recent_activity_home_rows() -> u32 {
  u32::try_from(rule::HOME_ROWS).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn orders_and_cuts() {
    let candidate = |id: &str, at| RecentActivityCandidate {
      id: id.into(),
      name: id.into(),
      maybe_last_activity_ms: at,
      is_archived: false,
    };
    let list = vec![
      candidate("a", Some(1)),
      candidate("b", None),
      candidate("c", Some(3)),
    ];
    assert_eq!(recent_activity(list.clone(), None), ["c", "a"]);
    assert_eq!(recent_activity(list, Some(1)), ["c"]);
    assert_eq!(recent_activity_home_rows(), 6);
  }
}
