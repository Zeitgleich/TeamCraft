//! Whether a list has data yet (issue #297): a list that depends on a server round trip or on the
//! first sync can be "not there yet" or "could not be read" as well as empty, and only the last
//! one may say "nothing here".
//!
//! The status is kept apart from the list's data on purpose: `Loadable<Vec<T>>` could not say
//! "these are the cached rows and the refresh failed", which is exactly what an offline device has,
//! and it would change reads that pickers, mentions and notifications also use.

use serde::{Deserialize, Serialize};

use crate::failure::FailureKind;
use crate::model::ConversationId;

/// How long a list waits for a request's answer before it says it got none. Longer than a slow
/// mobile link needs for one page, short enough that nobody looks at placeholders forever.
pub const REQUEST_DEADLINE_MS: u64 = 20_000;
/// The same for a list that fills up during the first sync, which reads many conversations.
pub const FIRST_SYNC_DEADLINE_MS: u64 = 120_000;

/// A list that can be "not loaded yet" as well as empty.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "name", rename_all = "snake_case")]
pub enum LoadingList {
  CallHistory,
  /// The server's answer to one directory search. Build it with [`Self::directory`].
  Directory {
    query: String,
  },
  /// The members of a conversation.
  Members {
    conversation: ConversationId,
  },
  /// The activity feed and the threads inbox, both built from what the first sync brings.
  Activity,
  /// A list of a newer client: nothing waits for it.
  #[serde(other)]
  Unknown,
}

/// How far a list is, as its backend knows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ListLoad {
  Loading,
  /// The list has had an answer: what it holds is what there is.
  Ready,
  Failed {
    failure: LoadFailure,
  },
}

/// Why a list could not be read, as far as the UI words it differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoadFailure {
  /// The server could not be reached.
  Network,
  /// The server refused.
  Server,
  /// No answer before the list's deadline.
  TimedOut,
  /// Anything else, a cause of a newer core included.
  #[serde(other)]
  Other,
}

/// What a list shows instead of, or as, its rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListPhase {
  /// The rows it has: cached rows also while they are refreshed, or when that failed.
  Rows,
  Skeleton,
  /// Only ever after an answer: the empty state is a statement about the list.
  Empty,
  Failed(LoadFailure),
}

impl LoadingList {
  /// One directory search; the query is compared the way the directory searches.
  pub fn directory(query: &str) -> Self {
    Self::Directory {
      query: query.trim().to_lowercase(),
    }
  }
}

impl ListLoad {
  /// This status once the list's deadline has passed: a request that never answers ends as failed.
  /// A late answer still counts, since the status it brings is no longer `Loading`.
  #[must_use]
  pub fn after_deadline(self, is_overdue: bool) -> Self {
    match self {
      Self::Loading if is_overdue => Self::Failed {
        failure: LoadFailure::TimedOut,
      },
      other => other,
    }
  }
}

impl From<&FailureKind> for LoadFailure {
  fn from(kind: &FailureKind) -> Self {
    match kind {
      FailureKind::Network | FailureKind::CaptivePortal | FailureKind::UntrustedCertificate => Self::Network,
      FailureKind::Server(_) => Self::Server,
      _ => Self::Other,
    }
  }
}

impl ListPhase {
  pub fn of(load: ListLoad, is_empty: bool) -> Self {
    match (load, is_empty) {
      (_, false) => Self::Rows,
      (ListLoad::Loading, true) => Self::Skeleton,
      (ListLoad::Ready, true) => Self::Empty,
      (ListLoad::Failed { failure }, true) => Self::Failed(failure),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::id_from_raw;

  const FAILED: ListLoad = ListLoad::Failed {
    failure: LoadFailure::Network,
  };

  mod phase {
    use super::*;

    #[test]
    fn an_empty_list_shows_what_its_status_says() {
      assert_eq!(ListPhase::of(ListLoad::Loading, true), ListPhase::Skeleton);
      assert_eq!(ListPhase::of(ListLoad::Ready, true), ListPhase::Empty);
      assert_eq!(
        ListPhase::of(FAILED, true),
        ListPhase::Failed(LoadFailure::Network)
      );
    }

    #[test]
    fn cached_rows_show_whatever_the_status() {
      for load in [ListLoad::Loading, ListLoad::Ready, FAILED] {
        assert_eq!(ListPhase::of(load, false), ListPhase::Rows);
      }
    }
  }

  mod deadline {
    use super::*;

    #[test]
    fn only_a_list_still_loading_fails_at_its_deadline() {
      assert_eq!(
        ListLoad::Loading.after_deadline(true),
        ListLoad::Failed {
          failure: LoadFailure::TimedOut
        }
      );
      assert_eq!(ListLoad::Loading.after_deadline(false), ListLoad::Loading);
      assert_eq!(ListLoad::Ready.after_deadline(true), ListLoad::Ready);
      assert_eq!(FAILED.after_deadline(true), FAILED);
    }

    #[test]
    fn a_stuck_list_ends_with_a_retry_not_a_skeleton() {
      let overdue = ListLoad::Loading.after_deadline(true);
      assert_eq!(
        ListPhase::of(overdue, true),
        ListPhase::Failed(LoadFailure::TimedOut)
      );
    }
  }

  mod wire {
    use super::*;

    #[test]
    fn lists_have_fixed_names() {
      let conversation: ConversationId = id_from_raw(7);
      let cases = [
        (LoadingList::CallHistory, r#"{"name":"call_history"}"#.to_owned()),
        (
          LoadingList::directory(" Ann "),
          r#"{"name":"directory","query":"ann"}"#.to_owned(),
        ),
        (
          LoadingList::Members { conversation },
          format!(
            r#"{{"name":"members","conversation":{}}}"#,
            serde_json::to_string(&conversation).unwrap()
          ),
        ),
        (LoadingList::Activity, r#"{"name":"activity"}"#.to_owned()),
      ];
      for (list, json) in cases {
        assert_eq!(serde_json::to_string(&list).unwrap(), json);
        assert_eq!(serde_json::from_str::<LoadingList>(&json).unwrap(), list);
      }
    }

    #[test]
    fn a_list_of_a_newer_client_reads_as_unknown() {
      let list: LoadingList = serde_json::from_str(r#"{"name":"polls","poll":1}"#).unwrap();
      assert_eq!(list, LoadingList::Unknown);
    }

    #[test]
    fn statuses_have_fixed_names() {
      assert_eq!(
        serde_json::to_string(&ListLoad::Ready).unwrap(),
        r#"{"state":"ready"}"#
      );
      assert_eq!(
        serde_json::to_string(&FAILED).unwrap(),
        r#"{"state":"failed","failure":"network"}"#
      );
    }

    #[test]
    fn a_failure_of_a_newer_core_reads_as_other() {
      let load: ListLoad = serde_json::from_str(r#"{"state":"failed","failure":"quota"}"#).unwrap();
      assert_eq!(
        load,
        ListLoad::Failed {
          failure: LoadFailure::Other
        }
      );
    }
  }

  #[test]
  fn failures_map_to_what_the_list_says() {
    assert_eq!(LoadFailure::from(&FailureKind::Network), LoadFailure::Network);
    assert_eq!(
      LoadFailure::from(&FailureKind::CaptivePortal),
      LoadFailure::Network
    );
    assert_eq!(
      LoadFailure::from(&FailureKind::Server(teamcraft_proto::ErrorCode::Forbidden)),
      LoadFailure::Server
    );
    assert_eq!(LoadFailure::from(&FailureKind::Other), LoadFailure::Other);
  }
}
