//! Host controls of a running meeting (PLAN.md §9): the host mutes everybody, removes a person,
//! locks the call or ends the meeting. The server decides whether the caller hosts the meeting and
//! keeps people out afterwards; the SFU acts on the media connections.

use serde_json::json;
use teamcraft_db::StoredMeeting;
use teamcraft_proto::{
  ErrorCode, MeetingControlAction, MeetingControlRequest, MeetingId, MeetingStatus, SfuControlAction,
  SfuControlRequest,
};
use tracing::warn;

use super::audit::{self, Target as AuditTarget};
use super::calls::CallService;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

pub async fn control_meeting(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: MeetingId,
  request: MeetingControlRequest,
) -> Result<(), ApiError> {
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
  if request.action == MeetingControlAction::End {
    return end_meeting(state, state.maybe_calls.as_deref(), caller, &meeting).await;
  }
  let calls = state
    .maybe_calls
    .as_ref()
    .ok_or_else(|| ApiError::Unavailable("calls are not configured".into()))?;
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
    // Handled above: ending works without a running call.
    MeetingControlAction::End => return end_meeting(state, Some(calls), caller, &meeting).await,
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
    // End is `end_meeting`'s. Unknown is only what a newer peer might send; the server never
    // builds it. Leaving on all devices is not a host control (`calls::leave_call_everywhere`).
    SfuControlAction::End
    | SfuControlAction::MuteAll
    | SfuControlAction::DisconnectUser { .. }
    | SfuControlAction::Unknown => {},
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

/// "End for everyone" (#1931): closes the meeting whether or not a call runs and whether or not the
/// host is in it. A running call is stopped on the SFU; the meeting's guests lose their access at
/// once instead of at the next sweep; the invited users' lists show it ended. Without calls
/// configured no call can run, and the meeting still ends.
async fn end_meeting(
  state: &AppState,
  maybe_calls: Option<&CallService>,
  caller: &AuthenticatedUser,
  meeting: &StoredMeeting,
) -> Result<(), ApiError> {
  let maybe_running = maybe_calls.and_then(|calls| {
    calls
      .running_call(meeting.conversation)
      .map(|call_id| (calls, call_id))
  });
  // Checked before anything changes: a meeting is never marked ended while its call goes on.
  if maybe_running.is_some_and(|(calls, _)| !calls.has_controls()) {
    return Err(ApiError::Unavailable("host controls are not configured".into()));
  }
  // Ending a series' call ends today's meeting, not the series (issue #1006): only a meeting that
  // doesn't repeat is over for good.
  if meeting.maybe_recurrence_rule.is_none() && state.database.end_meeting(meeting.id).await? {
    super::meetings::announce(state, meeting).await?;
    // The periodic sweep would end the guests too; a failure here only delays that.
    if let Err(error) = super::meeting_guests::sweep_expired_guests(state).await {
      warn!(%error, meeting_id = %meeting.id, "ending the guests of an ended meeting failed");
    }
  }
  audit::record(
    state,
    Some(caller.user.id),
    "meeting_control",
    Some(AuditTarget::new("meeting", meeting.id)),
    &json!({"action": "end"}),
  )
  .await?;
  let Some((calls, call_id)) = maybe_running else {
    return Ok(());
  };
  let sent = calls
    .send_control(&SfuControlRequest {
      call_id,
      by_user_id: caller.user.id,
      action: SfuControlAction::End,
    })
    .await;
  match sent {
    // The call ended on its own meanwhile: nothing is left to stop.
    Err(error) if error.error_code() == ErrorCode::CallNotFound => Ok(()),
    other => other,
  }
}
