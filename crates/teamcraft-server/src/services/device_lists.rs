//! Signed device lists (e2ee.md §3, security-review A4): opaque blobs the server stores for the
//! owner and hands to peers, who verify the signature themselves.

use teamcraft_db::DeviceListOutcome;
use teamcraft_proto::{Base64Bytes, DeviceListResponse, PutDeviceListRequest, UserId};

use super::directory::visible_user;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

pub async fn get_device_list(
  state: &AppState,
  caller: &AuthenticatedUser,
  user: UserId,
) -> Result<DeviceListResponse, ApiError> {
  let user = visible_user(state, caller, user).await?;
  let list = state
    .database
    .get_device_list(user.id)
    .await?
    .ok_or_else(ApiError::device_list_not_found)?;
  Ok(DeviceListResponse {
    sequence: list.sequence,
    signed_list: Base64Bytes::new(list.signed_list),
  })
}

pub async fn put_device_list(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: PutDeviceListRequest,
) -> Result<DeviceListResponse, ApiError> {
  let max = state.settings.limits.max_device_list_bytes;
  if request.signed_list.is_empty() {
    return Err(ApiError::InvalidRequest(
      "the device list must not be empty".into(),
    ));
  }
  if request.signed_list.len() > max {
    return Err(ApiError::PayloadTooLarge(format!(
      "device list exceeds {max} bytes"
    )));
  }
  // The sequence only ever goes up, so a stale list can't replace a newer one.
  if request.sequence <= request.expected_sequence {
    return Err(ApiError::InvalidRequest(
      "the sequence must be above the expected one".into(),
    ));
  }
  match state
    .database
    .put_device_list(
      caller.user.id,
      request.expected_sequence,
      request.sequence,
      request.signed_list.as_slice(),
    )
    .await?
  {
    DeviceListOutcome::Stored => Ok(DeviceListResponse {
      sequence: request.sequence,
      signed_list: request.signed_list,
    }),
    DeviceListOutcome::VersionConflict { current_sequence } => Err(ApiError::VersionConflict {
      current_version: current_sequence,
    }),
  }
}
