use teamcraft_db::NewDevice;
use teamcraft_proto::{DeviceId, DeviceResponse, ListDevicesResponse, RegisterDeviceRequest};

use super::convert::device_response;
use super::validate_name;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

/// Upper bounds for the opaque key material; real keys are far smaller.
const MAX_KEY_BYTES: usize = 4096;
const MAX_PUSH_TOKEN_CHARS: usize = 4096;

pub async fn register_device(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: RegisterDeviceRequest,
) -> Result<DeviceResponse, ApiError> {
  let name = validate_name(
    &request.name,
    "device name",
    state.settings.limits.max_name_length,
  )?;
  if request.public_key.is_empty() || request.public_key.len() > MAX_KEY_BYTES {
    return Err(ApiError::InvalidRequest("public_key has an invalid size".into()));
  }
  if request.cross_signature.is_empty() || request.cross_signature.len() > MAX_KEY_BYTES {
    return Err(ApiError::InvalidRequest(
      "cross_signature has an invalid size".into(),
    ));
  }
  if request
    .maybe_push_token
    .as_ref()
    .is_some_and(|token| token.chars().count() > MAX_PUSH_TOKEN_CHARS)
  {
    return Err(ApiError::InvalidRequest("push_token is too long".into()));
  }

  let device = state
    .database
    .register_device(&NewDevice {
      id: state.next_id()?,
      user_id: caller.user.id,
      name: &name,
      public_key: request.public_key.as_slice(),
      cross_signature: request.cross_signature.as_slice(),
      maybe_push_token: request.maybe_push_token.as_deref(),
    })
    .await?;
  Ok(device_response(&device))
}

pub async fn list_devices(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<ListDevicesResponse, ApiError> {
  let devices = state.database.list_devices(caller.user.id).await?;
  Ok(ListDevicesResponse {
    devices: devices.iter().map(device_response).collect(),
  })
}

pub async fn revoke_device(
  state: &AppState,
  caller: &AuthenticatedUser,
  device: DeviceId,
) -> Result<(), ApiError> {
  if state.database.revoke_device(caller.user.id, device).await? {
    Ok(())
  } else {
    Err(ApiError::device_not_found())
  }
}
