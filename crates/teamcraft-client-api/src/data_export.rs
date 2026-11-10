//! "Export my data" (issue #211, docs/data-export.md): what the UI asks of the core that writes the
//! archive (`core_data_export`, answered by the desktop shell and by the core in the browser page),
//! and where the export stands. The core decrypts and writes; the UI only starts, shows and stops.

use serde::{Deserialize, Serialize};

/// Which messages go into the archive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataExportScope {
  /// The messages the user wrote (with the reactions to them).
  #[default]
  Own,
  /// Every message this device can read in the user's conversations, other people's included.
  All,
}

/// What the user chose before starting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataExportOptions {
  pub scope: DataExportScope,
  /// The files the user sent go into the archive too (up to a size each, see the format).
  pub includes_files: bool,
}

/// The words of the archive's index page (`index.html`), translated by the UI: the core that writes
/// it has no translations. Kept with the export, so a resumed one writes the same language.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataExportLabels {
  /// The page's `lang`.
  pub language: String,
  pub title: String,
  /// What the archive is, above everything else.
  pub intro: String,
  pub account_heading: String,
  /// What the account folder holds.
  pub account_text: String,
  pub conversations_heading: String,
  pub conversation_column: String,
  pub messages_column: String,
  pub files_column: String,
  /// Next to a conversation whose history the server let this device read only in part.
  pub history_limited: String,
  /// What the archive can't hold (keys, deleted messages, other people's files).
  pub not_included: String,
  /// Where `manifest.json` and the checksums are.
  pub manifest_text: String,
  /// "Made on {date}": the UI leaves `{date}` in, the core puts the date there.
  pub made_on: String,
}

/// The one request of `core_data_export`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum DataExportRequest {
  /// Asks where to save (desktop) and starts. An export already running is not replaced.
  Start {
    options: DataExportOptions,
    /// Boxed: the other requests are small.
    labels: Box<DataExportLabels>,
  },
  /// Where the export stands.
  Status,
  /// Stops and deletes what was written so far.
  Cancel,
  /// Continues an export an earlier run of the app left unfinished.
  Resume,
  /// Deletes what an unfinished export left, without continuing.
  Discard,
  /// Forgets a finished, failed or cancelled export's result, so the card shows the start again.
  Dismiss,
}

/// The stage the export is in, in the order they run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataExportPhase {
  /// What the server and the account data hold about the user.
  Account,
  /// Reading the history of a conversation from the server down to its beginning.
  History,
  /// Writing a conversation's messages.
  Messages,
  /// Downloading and decrypting the user's files.
  Files,
  /// The index and the manifest.
  Finishing,
}

/// How far an export got. Counts only grow.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataExportProgress {
  #[serde(rename = "phase", default, skip_serializing_if = "Option::is_none")]
  pub maybe_phase: Option<DataExportPhase>,
  pub conversations_done: u32,
  pub conversations_total: u32,
  pub messages: u64,
  pub files: u32,
  /// Files left out: too large, or no longer on the server.
  pub files_skipped: u32,
  /// The archive's size so far.
  pub bytes: u64,
}

/// Why an export stopped without an archive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataExportFailure {
  /// The archive could not be written (disk full, folder gone, no permission).
  Storage,
  /// The server refused what the export needs.
  Server,
  /// The place an unfinished export wrote to is gone or changed: it can't continue.
  NotResumable,
  /// Anything else; the log has the details (never the content).
  Other,
}

/// Where the export stands, as the card shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DataExportStatus {
  /// Nothing running and nothing to show.
  #[default]
  Idle,
  Running {
    progress: DataExportProgress,
  },
  /// The network or the server failed; the export goes on by itself at `retry_at_ms` (or when the
  /// connection is back).
  Waiting {
    progress: DataExportProgress,
    retry_at_ms: i64,
  },
  /// An earlier run of the app stopped in the middle: it can continue or be discarded.
  Interrupted {
    progress: DataExportProgress,
  },
  /// The archive is complete. `file_name` is where it went (desktop) or what the browser saves.
  Done {
    progress: DataExportProgress,
    file_name: String,
  },
  Failed {
    reason: DataExportFailure,
    progress: DataExportProgress,
  },
  /// The user stopped it; what was written is deleted.
  Cancelled,
}

impl DataExportStatus {
  /// Whether an export is under way (running or waiting to go on), so another can't start.
  pub fn is_active(&self) -> bool {
    matches!(self, Self::Running { .. } | Self::Waiting { .. })
  }

  /// The progress to show, if there is one.
  pub fn progress(&self) -> Option<&DataExportProgress> {
    match self {
      Self::Running { progress }
      | Self::Waiting { progress, .. }
      | Self::Interrupted { progress }
      | Self::Done { progress, .. }
      | Self::Failed { progress, .. } => Some(progress),
      Self::Idle | Self::Cancelled => None,
    }
  }
}

impl DataExportProgress {
  /// How far the export got as a fraction for a progress bar: conversations dominate, the account
  /// part before them and the finish after them take a sliver each. `None` before the
  /// conversations are known.
  pub fn fraction(&self) -> Option<f64> {
    let total = self.conversations_total;
    match self.maybe_phase? {
      DataExportPhase::Account => Some(0.0),
      DataExportPhase::Finishing => Some(0.99),
      DataExportPhase::History | DataExportPhase::Messages | DataExportPhase::Files if total == 0 => {
        Some(0.98)
      },
      DataExportPhase::History | DataExportPhase::Messages | DataExportPhase::Files => {
        let done = f64::from(self.conversations_done.min(total)) / f64::from(total);
        Some(0.02 + 0.96 * done)
      },
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn requests_and_states_keep_their_wire_names() {
    let start = DataExportRequest::Start {
      options: DataExportOptions {
        scope: DataExportScope::All,
        includes_files: true,
      },
      labels: Box::default(),
    };
    let value = serde_json::to_value(start).unwrap();
    assert_eq!(value["op"], "start");
    assert_eq!(
      value["options"],
      serde_json::json!({"scope": "all", "includes_files": true})
    );
    let waiting = DataExportStatus::Waiting {
      progress: DataExportProgress::default(),
      retry_at_ms: 5,
    };
    assert_eq!(serde_json::to_value(&waiting).unwrap()["state"], "waiting");
    let back: DataExportStatus = serde_json::from_value(serde_json::to_value(&waiting).unwrap()).unwrap();
    assert_eq!(back, waiting);
  }

  #[test]
  fn the_bar_moves_with_the_conversations_and_never_claims_the_end_early() {
    let mut progress = DataExportProgress {
      maybe_phase: Some(DataExportPhase::Messages),
      conversations_done: 0,
      conversations_total: 4,
      ..DataExportProgress::default()
    };
    assert_eq!(progress.fraction(), Some(0.02));
    progress.conversations_done = 2;
    assert_eq!(progress.fraction(), Some(0.5));
    progress.maybe_phase = Some(DataExportPhase::Finishing);
    assert!(progress.fraction().unwrap() < 1.0);
    assert_eq!(DataExportProgress::default().fraction(), None);
  }

  #[test]
  fn only_a_running_or_waiting_export_blocks_a_new_one() {
    let progress = DataExportProgress::default();
    assert!(DataExportStatus::Running { progress }.is_active());
    assert!(
      DataExportStatus::Waiting {
        progress,
        retry_at_ms: 0
      }
      .is_active()
    );
    assert!(!DataExportStatus::Interrupted { progress }.is_active());
    assert!(!DataExportStatus::Cancelled.is_active());
  }
}
