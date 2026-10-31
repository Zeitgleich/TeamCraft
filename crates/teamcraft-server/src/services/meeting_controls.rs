//! Host controls of a running meeting (PLAN.md §9): the host mutes everybody, removes a person,
//! locks the call or ends it. The server decides whether the caller hosts the meeting and keeps
//! people out afterwards; the SFU acts on the media connections.

use serde_json::json;
use teamcraft_proto::{
  MeetingControlAction, MeetingControlRequest, MeetingId, MeetingStatus, SfuControlAction, SfuControlRequest,
};

use super::audit::{self, Target as AuditTarget};
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

pub async fn control_meeting(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: MeetingId,
  request: MeetingControlRequest,
) -> Result<(), ApiError> {
  let calls = state
    .maybe_calls
    .as_ref()
    .ok_or_else(|| ApiError::Unavailable("calls are not configured".into()))?;
  let meeting = state
    .database
    .get_meeting(id)
    .await?
    .ok_or_else(ApiError::meeting_not_found)?;
  let members = state
    .database
    .conversation_member_ids(meeting.conversation)
    .await?;
  if !members.contains(&caller.user.id) {
    return Err(ApiError::meeting_not_found());
  }
  let hosts = state.database.meeting_host_ids(&meeting).await?;
  if !hosts.contains(&caller.user.id) {
    return Err(ApiError::Forbidden("only hosts can do that".into()));
  }
  if meeting.status != MeetingStatus::Scheduled.as_str() {
    return Err(ApiError::Conflict("the meeting is over".into()));
  }
  if !calls.has_controls() {
    return Err(ApiError::Unavailable("host controls are not configured".into()));
  }
  let call_id = calls
    .running_call(meeting.conversation)
    .ok_or_else(ApiError::call_not_found)?;

  let (sfu_action, name) = match request.action {
    MeetingControlAction::MuteAll => (SfuControlAction::MuteAll, "mute_all"),
    MeetingControlAction::Lock => (SfuControlAction::Lock, "lock"),
    MeetingControlAction::Unlock => (SfuControlAction::Unlock, "unlock"),
    MeetingControlAction::End => (SfuControlAction::End, "end"),
    MeetingControlAction::RemoveParticipant { user_id } => {
      if hosts.contains(&user_id) {
        return Err(ApiError::InvalidRequest("hosts cannot be removed".into()));
      }
      (
        SfuControlAction::RemoveParticipant { user_id },
        "remove_participant",
      )
    },
    MeetingControlAction::Unknown => return Err(ApiError::InvalidRequest("unknown control".into())),
  };

  // The server's own state first: even if the SFU is slow, nobody gets back in.
  match &sfu_action {
    SfuControlAction::Lock => calls.set_locked(meeting.conversation, true),
    SfuControlAction::Unlock => calls.set_locked(meeting.conversation, false),
    SfuControlAction::RemoveParticipant { user_id } => calls.mark_removed(meeting.conversation, *user_id),
    // Ending a series' call ends today's meeting, not the series (issue #1006): only a meeting
    // that doesn't repeat is over for good.
    SfuControlAction::End if meeting.maybe_recurrence_rule.is_none() => {
      state.database.end_meeting(meeting.id).await?;
    },
    SfuControlAction::End => {},
    // Unknown is only what a newer peer might send; the server never builds it. Leaving on all
    // devices is not a host control (`calls::leave_call_everywhere`).
    SfuControlAction::MuteAll | SfuControlAction::DisconnectUser { .. } | SfuControlAction::Unknown => {},
  }
  audit::record(
    state,
    Some(caller.user.id),
    "meeting_control",
    Some(AuditTarget::new("meeting", meeting.id)),
    &json!({"action": name}),
  )
  .await?;
  calls
    .send_control(&SfuControlRequest {
      call_id,
      by_user_id: caller.user.id,
      action: sfu_action,
    })
    .await
}
