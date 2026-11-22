//! The details of audit entries the console words itself instead of showing the server's JSON
//! (the export keeps the JSON).

use serde::Deserialize;

use crate::admin::AuditAction;

/// What the detail column shows for an entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditDetail {
  /// The server's detail as it came.
  Raw(String),
  /// A call recording started with this many devices in the call (issue #2117).
  RecordingStarted { participants: u32 },
  /// A call recording ended after `duration_secs`, with at most `participants` devices in the call
  /// while it ran; `has_left`: its device left the call without a stop.
  RecordingStopped {
    participants: u32,
    duration_secs: u64,
    has_left: bool,
  },
}

/// The recording entries' detail as the server writes it (`services::call_recordings`).
#[derive(Deserialize)]
struct RecordingJson {
  participants: u32,
  #[serde(default)]
  duration_secs: u64,
  #[serde(default)]
  ended_by: String,
}

impl AuditDetail {
  /// Words the details of the actions the console knows; anything else, or a detail it can't read,
  /// stays as it came.
  pub fn of(action: AuditAction, detail: &str) -> Self {
    let raw = || Self::Raw(detail.to_owned());
    match action {
      AuditAction::CallRecordingStarted => serde_json::from_str::<RecordingJson>(detail).map_or_else(
        |_| raw(),
        |json| Self::RecordingStarted {
          participants: json.participants,
        },
      ),
      AuditAction::CallRecordingStopped => serde_json::from_str::<RecordingJson>(detail).map_or_else(
        |_| raw(),
        |json| Self::RecordingStopped {
          participants: json.participants,
          duration_secs: json.duration_secs,
          has_left: json.ended_by == "left",
        },
      ),
      _ => raw(),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn recording_entries_are_worded_from_their_detail() {
    assert_eq!(
      AuditDetail::of(
        AuditAction::CallRecordingStarted,
        r#"{"call_id":"1","device_id":"2","participants":3}"#
      ),
      AuditDetail::RecordingStarted { participants: 3 }
    );
    assert_eq!(
      AuditDetail::of(
        AuditAction::CallRecordingStopped,
        r#"{"call_id":"1","device_id":"2","participants":4,"duration_secs":754,"ended_by":"left"}"#
      ),
      AuditDetail::RecordingStopped {
        participants: 4,
        duration_secs: 754,
        has_left: true,
      }
    );
  }

  #[test]
  fn other_entries_and_unreadable_details_stay_as_they_came() {
    assert_eq!(
      AuditDetail::of(AuditAction::PolicyChanged, r#"{"version":3}"#),
      AuditDetail::Raw(r#"{"version":3}"#.to_owned())
    );
    assert_eq!(
      AuditDetail::of(AuditAction::CallRecordingStopped, "{}"),
      AuditDetail::Raw("{}".to_owned())
    );
  }
}
