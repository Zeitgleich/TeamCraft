use teamcraft_db::NewDevice;
use teamcraft_proto::{
  DeviceId, DeviceResponse, GatewayEvent, ListDevicesResponse, PushProvider, RegisterDeviceRequest,
  RegisterPushRequest, UserId,
};

use super::audit::{self, Target as AuditTarget};
use super::convert::device_response;
use super::validate_name;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

/// Upper bounds for the opaque key material; real keys are far smaller.
const MAX_KEY_BYTES: usize = 4096;

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

  let device = state
    .database
    .register_device(&NewDevice {
      id: state.next_id()?,
      user_id: caller.user.id,
      name: &name,
      public_key: request.public_key.as_slice(),
      cross_signature: request.cross_signature.as_slice(),
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
  revoke_and_announce(state, caller.user.id, caller.user.id, device, "self").await
}

/// Revokes a device, ends its sessions, tells everyone who shares a conversation with the owner (so
/// their clients commit the removal to the MLS groups) and logs it.
pub async fn revoke_and_announce(
  state: &AppState,
  actor: UserId,
  owner: UserId,
  device: DeviceId,
  by: &str,
) -> Result<(), ApiError> {
  if !state.database.revoke_device(owner, device).await? {
    return Err(ApiError::device_not_found());
  }
  state.hub.revoke_device(owner, device);
  let mut audience = state.database.users_sharing_conversation(owner).await?;
  audience.insert(owner);
  state.hub.publish(
    audience,
    &GatewayEvent::DeviceRemove {
      user_id: owner,
      device_id: device,
    },
  );
  audit::record(
    state,
    Some(actor),
    "device_revoked",
    Some(AuditTarget::new("user", owner)),
    &serde_json::json!({"device_id": device.to_string(), "by": by}),
  )
  .await
}

const PUSH_KEY_BYTES: usize = 32;
const MAX_PUSH_TOKEN_CHARS: usize = 4096;

/// Registers the push channel of one of the caller's devices (token plus the shared push key).
pub async fn register_push(
  state: &AppState,
  caller: &AuthenticatedUser,
  device: DeviceId,
  request: RegisterPushRequest,
) -> Result<(), ApiError> {
  if matches!(request.provider, PushProvider::Unknown(_)) {
    return Err(ApiError::InvalidRequest("unknown push provider".into()));
  }
  if request.token.is_empty() || request.token.chars().count() > MAX_PUSH_TOKEN_CHARS {
    return Err(ApiError::InvalidRequest("push token has an invalid size".into()));
  }
  if request.push_key.len() != PUSH_KEY_BYTES {
    return Err(ApiError::InvalidRequest(format!(
      "push_key must be {PUSH_KEY_BYTES} bytes"
    )));
  }
  if request.provider == PushProvider::WebPush {
    // The server will POST to this URL; check it now rather than at the first message.
    crate::push::WebPushSubscription::parse(&request.token, state.settings.push.allow_private_endpoints)
      .map_err(|error| ApiError::InvalidRequest(format!("invalid Web Push subscription: {error}")))?;
  }
  let stored = state
    .database
    .set_push_registration(
      caller.user.id,
      device,
      request.provider.as_str(),
      &request.token,
      request.push_key.as_slice(),
    )
    .await?;
  if stored {
    Ok(())
  } else {
    Err(ApiError::device_not_found())
  }
}

pub async fn unregister_push(
  state: &AppState,
  caller: &AuthenticatedUser,
  device: DeviceId,
) -> Result<(), ApiError> {
  if state
    .database
    .clear_push_registration(caller.user.id, device)
    .await?
  {
    Ok(())
  } else {
    Err(ApiError::device_not_found())
  }
}
