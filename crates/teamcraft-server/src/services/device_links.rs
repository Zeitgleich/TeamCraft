//! Announcing a device link to the user's other devices (e2ee.md §5, "Approve prompt").

use std::time::Instant;

use teamcraft_proto::{
  CreateDeviceLinkRequest, DeviceLinkClosedReason, DeviceLinkRequest, GatewayEvent, ListDeviceLinksResponse,
};

use super::validate_name;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::rendezvous::AnnounceError;
use crate::rendezvous::connection::publish_closed;
use crate::state::AppState;

const ID_HEX_CHARS: usize = 32;
const KEY_BYTES: usize = 32;

/// The new device waits in the relay at `rendezvous_id`; its request goes to every connected
/// device of the caller. The key is the one the handshake needs (and the server sees it: the code
/// the user types on the approving device is what catches a server that injects itself).
pub async fn announce(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: CreateDeviceLinkRequest,
) -> Result<DeviceLinkRequest, ApiError> {
  if request.rendezvous_id.len() != ID_HEX_CHARS
    || !request.rendezvous_id.bytes().all(|b| b.is_ascii_hexdigit())
  {
    return Err(ApiError::InvalidRequest(
      "rendezvous_id must be 32 hex characters".into(),
    ));
  }
  if request.key.as_slice().len() != KEY_BYTES {
    return Err(ApiError::InvalidRequest("key must be 32 bytes".into()));
  }
  let name = validate_name(
    &request.device_name,
    "device name",
    state.settings.get().limits.max_name_length,
  )?;
  let announced = state
    .rendezvous
    .announce(
      &request.rendezvous_id,
      caller.user.id,
      request.key,
      name,
      request.platform,
      Instant::now(),
    )
    .map_err(|error| match error {
      AnnounceError::NotWaiting => ApiError::Conflict(error.to_string()),
      AnnounceError::AlreadyAnnounced => ApiError::Conflict(error.to_string()),
    })?;
  state.hub.publish(
    [caller.user.id],
    &GatewayEvent::DeviceLinkRequested {
      request: announced.clone(),
    },
  );
  Ok(announced)
}

/// What is open for the caller; devices read it when they connect.
pub fn list(state: &AppState, caller: &AuthenticatedUser) -> ListDeviceLinksResponse {
  ListDeviceLinksResponse {
    requests: state.rendezvous.pending_requests(caller.user.id, Instant::now()),
  }
}

/// A device of the user denies the request.
pub fn reject(state: &AppState, caller: &AuthenticatedUser, id: &str) -> Result<(), ApiError> {
  if !state.rendezvous.reject(id, caller.user.id) {
    return Err(ApiError::device_link_not_found());
  }
  publish_closed(state, caller.user.id, id, DeviceLinkClosedReason::Rejected);
  Ok(())
}

/// The new device gives up (or any device of the user dismisses the request without a decision on
/// the relay): the request goes, the prompts with it.
pub fn withdraw(state: &AppState, caller: &AuthenticatedUser, id: &str) -> Result<(), ApiError> {
  if !state.rendezvous.withdraw(id, caller.user.id) {
    return Err(ApiError::device_link_not_found());
  }
  publish_closed(state, caller.user.id, id, DeviceLinkClosedReason::Withdrawn);
  Ok(())
}
