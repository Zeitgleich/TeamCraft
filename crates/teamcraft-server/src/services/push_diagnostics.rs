//! Notification diagnostics (notifications.md, "Diagnostics API"): test pushes users send to their
//! own devices, and the delivery counts admins see per user (counts only, no content).

use teamcraft_proto::{
  DeviceId, DevicePushHealth, Permission, PushHealthResponse, PushProvider, Target, TestPushResponse, UserId,
};

use super::access::require;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::push::routing;
use crate::state::AppState;

/// Sends a sealed `test` wake-up to one of the caller's devices, at most once per
/// `push.test_interval_secs`.
pub async fn send_test_push(
  state: &AppState,
  caller: &AuthenticatedUser,
  device: DeviceId,
) -> Result<TestPushResponse, ApiError> {
  if !state.database.is_active_device_of(caller.user.id, device).await? {
    return Err(ApiError::device_not_found());
  }
  let interval = state.settings.get().push.test_interval;
  if !state.push_tracker.try_start_test(device, interval) {
    return Err(ApiError::RateLimited {
      retry_after_secs: interval.as_secs().max(1),
    });
  }
  Ok(routing::send_test(state, caller.user.id, device).await?)
}

/// Delivery counts of every device of `user` since the server started.
pub async fn push_health(
  state: &AppState,
  caller: &AuthenticatedUser,
  user: UserId,
) -> Result<PushHealthResponse, ApiError> {
  require(state, caller, Permission::ManageUsers, &Target::Instance)?;
  if state.database.get_user(user).await?.is_none() {
    return Err(ApiError::user_not_found());
  }
  health_of(state, user).await
}

/// The same for the caller's own devices (Settings > Devices, issue #409): which push service
/// each one is registered with and when the last push went out.
pub async fn own_push_health(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<PushHealthResponse, ApiError> {
  health_of(state, caller.user.id).await
}

async fn health_of(state: &AppState, user: UserId) -> Result<PushHealthResponse, ApiError> {
  let mut counts = state.push_tracker.counts_of(user);
  let devices = state
    .database
    .list_devices(user)
    .await?
    .into_iter()
    .map(|device| DevicePushHealth {
      device_id: device.id,
      maybe_provider: device.maybe_push_provider.map(PushProvider::from),
      counts: counts.remove(&device.id).unwrap_or_default(),
    })
    .collect();
  Ok(PushHealthResponse { devices })
}
