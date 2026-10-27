use teamcraft_db::NewDevice;
use teamcraft_proto::ChangeWebPushSubscriptionRequest;
use teamcraft_proto::{
  DeviceId, DeviceResponse, GatewayEvent, ListDevicesResponse, PushProvider, RegisterDeviceRequest,
  RegisterPushRequest, SetDeviceCertificateRequest, UserId,
};

use crate::auth::AuthError;

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
    state.settings.get().limits.max_name_length,
  )?;
  if request.public_key.is_empty() || request.public_key.len() > MAX_KEY_BYTES {
    return Err(ApiError::InvalidRequest("public_key has an invalid size".into()));
  }
  if request.cross_signature.is_empty() || request.cross_signature.len() > MAX_KEY_BYTES {
    return Err(ApiError::InvalidRequest(
      "cross_signature has an invalid size".into(),
    ));
  }

  let limit = state.settings.get().limits.max_devices_per_user;
  if state.database.count_active_devices(caller.user.id).await? >= limit as u64 {
    return Err(ApiError::Conflict(format!(
      "at most {limit} devices per user; remove one first"
    )));
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

  // Everybody who shares a conversation with the user (and the user's other devices) learns about the
  // device, so one of them can add it to the MLS groups.
  let mut audience = state.database.users_sharing_conversation(caller.user.id).await?;
  audience.insert(caller.user.id);
  state.hub.publish(
    audience,
    &GatewayEvent::DeviceAdd {
      user_id: caller.user.id,
      device_id: device.id,
    },
  );
  Ok(device_response(&device))
}

/// Replaces the device's cross-signature with the final device certificate.
pub async fn set_device_certificate(
  state: &AppState,
  caller: &AuthenticatedUser,
  device: DeviceId,
  request: SetDeviceCertificateRequest,
) -> Result<DeviceResponse, ApiError> {
  if request.cross_signature.is_empty() || request.cross_signature.len() > MAX_KEY_BYTES {
    return Err(ApiError::InvalidRequest(
      "cross_signature has an invalid size".into(),
    ));
  }
  if !state
    .database
    .set_device_certificate(caller.user.id, device, request.cross_signature.as_slice())
    .await?
  {
    return Err(ApiError::device_not_found());
  }
  let devices = state.database.list_devices(caller.user.id).await?;
  devices
    .iter()
    .find(|d| d.id == device)
    .map(device_response)
    .ok_or_else(ApiError::device_not_found)
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
  check_bound_token(state, &request)?;
  if matches!(
    request.provider,
    PushProvider::WebPush | PushProvider::UnifiedPush
  ) {
    // The server will POST to the endpoint (UnifiedPush registrations use the Web Push subscription
    // format); check it now rather than at the first message.
    crate::push::WebPushSubscription::parse(
      &request.token,
      state.settings.get().push.allow_private_endpoints,
    )
    .map_err(|error| ApiError::InvalidRequest(format!("invalid push subscription: {error}")))?;
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

/// Tokens that the app sealed for the push gateway (`tcgw1:...`) only make sense where pushes go
/// through the gateway, and a gateway takes nothing else, unless raw tokens are allowed during a
/// transition. The apps ask `GET /api/v1/push/gateway` first.
fn check_bound_token(state: &AppState, request: &RegisterPushRequest) -> Result<(), ApiError> {
  let is_bound = request.token.starts_with(teamcraft_proto::BOUND_TOKEN_PREFIX);
  let is_gateway = state.push.is_gateway(&request.provider);
  if is_bound && !is_gateway {
    return Err(ApiError::InvalidRequest(
      "this server does not send this provider's pushes through a push gateway; register the device token itself".into(),
    ));
  }
  if is_gateway && !is_bound && state.push.requires_bound_token(&request.provider) {
    return Err(ApiError::InvalidRequest(
      "this server sends pushes through a push gateway; register a device token sealed for it (GET /api/v1/push/gateway)".into(),
    ));
  }
  Ok(())
}

/// The service worker replaces a rotated Web Push subscription while no page is open (e2ee.md §7).
/// There is no access token: the request proves knowledge of the device's push key. Every refusal
/// that could tell a guesser something (unknown device, no Web Push, wrong proof) is the same 401,
/// which the per-address limits count as a failed authentication.
pub async fn change_web_push_subscription(
  state: &AppState,
  request: ChangeWebPushSubscriptionRequest,
) -> Result<(), ApiError> {
  for token in [&request.previous_token, &request.token] {
    if token.is_empty() || token.chars().count() > MAX_PUSH_TOKEN_CHARS {
      return Err(ApiError::InvalidRequest("push token has an invalid size".into()));
    }
  }
  let provider = PushProvider::WebPush.as_str();
  let refused = || {
    ApiError::Auth(AuthError::InvalidToken(
      "subscription change not authenticated".into(),
    ))
  };
  let (stored_token, push_key) = state
    .database
    .push_registration_of(request.device_id, provider)
    .await?
    .ok_or_else(refused)?;
  crate::push::crypto::verify_subscription_change(
    &push_key,
    request.device_id,
    &request.previous_token,
    &request.token,
    request.mac.as_slice(),
  )
  .map_err(|_| refused())?;
  if stored_token == request.token {
    // A retry, or the page registered the same subscription first.
    return Ok(());
  }
  crate::push::WebPushSubscription::parse(&request.token, state.settings.get().push.allow_private_endpoints)
    .map_err(|error| ApiError::InvalidRequest(format!("invalid push subscription: {error}")))?;
  let is_replaced = state
    .database
    .replace_push_token(
      request.device_id,
      provider,
      &request.previous_token,
      &request.token,
    )
    .await?;
  if !is_replaced {
    return Err(ApiError::Conflict(
      "the device registered another push subscription meanwhile".into(),
    ));
  }
  tracing::info!(device_id = %request.device_id, "web push subscription replaced by the service worker");
  Ok(())
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
