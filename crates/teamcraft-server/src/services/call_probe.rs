//! Link probe tickets (#1959): a device measures its link to the SFU on the pre-join screen or in a
//! meeting's lobby, before it may join anything, so its call starts on the voice step and video
//! layers its link carries (#1893).
//!
//! A ticket grants padding up to the SFU's probe budget each way and nothing else: it names no call,
//! creates no call state, and tells nobody that someone is measuring. Members get one for any of
//! their active devices; a guest only while they wait in, or were let into, a meeting that isn't
//! over and hasn't removed them.

use std::time::Instant;

use rand_core::{OsRng, RngCore};
use teamcraft_proto::{
  CallProbeTicketResponse, CreateCallProbeTicketRequest, LobbyState, MeetingStatus, UserKind,
};

use super::calls::unix_now_secs;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

/// Bytes of randomness in a ticket's id.
const TICKET_ID_BYTES: usize = 16;

/// Issues a link probe ticket for one of the caller's active devices.
pub async fn create_probe_ticket(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: CreateCallProbeTicketRequest,
) -> Result<CallProbeTicketResponse, ApiError> {
  let calls = state
    .maybe_calls
    .as_ref()
    .ok_or_else(|| ApiError::Unavailable("calls are not configured".into()))?;
  if calls.probe_tickets_per_device_per_minute == 0 {
    return Err(ApiError::Unavailable("measuring before the join is off".into()));
  }
  if !state
    .database
    .is_active_device_of(caller.user.id, request.device_id)
    .await?
  {
    return Err(ApiError::device_not_found());
  }
  if caller.user.kind == UserKind::Guest {
    check_guest_in_lobby(state, caller, request.maybe_meeting_id).await?;
  }
  state.rate_limits.check_probe_ticket(
    request.device_id,
    calls.probe_tickets_per_device_per_minute,
    Instant::now(),
  )?;
  calls.probe_ticket(caller.user.id, request.device_id, ticket_id(), unix_now_secs()?)
}

/// A guest measures only while waiting in, or let into, a meeting that is on and that hasn't
/// removed them: a refused guest, or one of a meeting that ended, gets nothing.
async fn check_guest_in_lobby(
  state: &AppState,
  caller: &AuthenticatedUser,
  maybe_meeting_id: Option<teamcraft_proto::MeetingId>,
) -> Result<(), ApiError> {
  let meeting_id =
    maybe_meeting_id.ok_or_else(|| ApiError::Forbidden("a guest names their meeting".into()))?;
  let meeting = state
    .database
    .get_meeting(meeting_id)
    .await?
    .ok_or_else(ApiError::meeting_not_found)?;
  if meeting.status != MeetingStatus::Scheduled.as_str() {
    return Err(ApiError::Conflict("the meeting is over".into()));
  }
  let row = state
    .database
    .get_lobby_entry(meeting.id, caller.user.id)
    .await?
    .ok_or_else(ApiError::meeting_not_found)?;
  if !matches!(
    LobbyState::from(row.state),
    LobbyState::Waiting | LobbyState::Admitted
  ) {
    return Err(ApiError::Forbidden("the guest wasn't let in".into()));
  }
  let is_removed = state
    .maybe_calls
    .as_ref()
    .is_some_and(|calls| calls.is_removed(meeting.conversation, caller.user.id));
  if is_removed {
    return Err(ApiError::Forbidden(
      "the host removed you from the meeting".into(),
    ));
  }
  Ok(())
}

/// A fresh ticket id: the SFU takes each ticket once.
fn ticket_id() -> String {
  let mut bytes = [0u8; TICKET_ID_BYTES];
  OsRng.fill_bytes(&mut bytes);
  bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn ticket_ids_are_random_hex() {
    let (first, second) = (ticket_id(), ticket_id());
    assert_eq!(first.len(), TICKET_ID_BYTES * 2);
    assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(first, second);
  }
}
