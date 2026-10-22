//! Host controls of a running meeting (PLAN.md §9): the server checks that the caller hosts the
//! meeting and tells the SFU to act.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::UserId;

/// What the host wants done to the call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum MeetingControlAction {
  /// Everybody mutes themselves (and may unmute).
  MuteAll,
  /// Removes a participant, who cannot join again until the call ends.
  RemoveParticipant {
    user_id: UserId,
  },
  /// New participants are turned away (the host can still join).
  Lock,
  Unlock,
  /// Ends the meeting for everyone.
  End,
  #[serde(other)]
  Unknown,
}

/// `POST /api/v1/meetings/{id}/controls`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MeetingControlRequest {
  #[serde(flatten)]
  pub action: MeetingControlAction,
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn actions_are_tagged_and_unknown_ones_are_kept_apart() {
    let request: MeetingControlRequest = serde_json::from_str(r#"{"action":"mute_all"}"#).unwrap();
    assert_eq!(request.action, MeetingControlAction::MuteAll);
    let request: MeetingControlRequest =
      serde_json::from_str(r#"{"action":"remove_participant","user_id":"7"}"#).unwrap();
    assert!(matches!(
      request.action,
      MeetingControlAction::RemoveParticipant { .. }
    ));
    let request: MeetingControlRequest = serde_json::from_str(r#"{"action":"dance"}"#).unwrap();
    assert_eq!(request.action, MeetingControlAction::Unknown);
    assert_eq!(
      serde_json::to_string(&MeetingControlRequest {
        action: MeetingControlAction::End
      })
      .unwrap(),
      r#"{"action":"end"}"#
    );
  }
}
