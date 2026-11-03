//! MLS key packages (e2ee.md §4, §4.3): uploaded in batches by devices, consumed once by members who
//! add them. Each package's expiry is read from the package; only packages that stay valid for
//! `key_package_min_validity_secs` are handed out, the rest are deleted.

use std::time::Duration;

use teamcraft_crypto::mls::inspect_key_package;
use teamcraft_db::{AddPackagesOutcome, NewKeyPackage};
use teamcraft_proto::{
  Base64Bytes, ClaimKeyPackagesRequest, ClaimKeyPackagesResponse, ClaimedKeyPackage, DeviceId, Id,
  KeyPackageStatusResponse, Snowflake, UploadKeyPackagesRequest, UserId,
};

use super::access::load_conversation;
use super::directory::visible_user;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

pub async fn upload_key_packages(
  state: &AppState,
  caller: &AuthenticatedUser,
  device: DeviceId,
  request: UploadKeyPackagesRequest,
) -> Result<KeyPackageStatusResponse, ApiError> {
  if !state.database.is_active_device_of(caller.user.id, device).await? {
    return Err(ApiError::device_not_found());
  }
  let limits = &state.settings.get().limits;
  if request.key_packages.len() > limits.max_key_packages_per_upload {
    return Err(ApiError::InvalidRequest(format!(
      "at most {} key packages per upload",
      limits.max_key_packages_per_upload
    )));
  }
  let all = request
    .key_packages
    .iter()
    .chain(request.maybe_last_resort.as_ref());
  for package in all {
    if package.is_empty() || package.len() > limits.max_key_package_bytes {
      return Err(ApiError::InvalidRequest(
        "a key package has an invalid size".into(),
      ));
    }
  }

  let now = time::OffsetDateTime::now_utc();
  let valid_until = now + min_validity(state);
  let mut regular = Vec::with_capacity(request.key_packages.len());
  for package in &request.key_packages {
    regular.push(new_package(state, package, false, valid_until)?);
  }
  let last_resort = match &request.maybe_last_resort {
    Some(package) => Some(new_package(state, package, true, valid_until)?),
    None => None,
  };
  let outcome = state
    .database
    .add_key_packages(
      device,
      &regular,
      last_resort,
      limits.max_key_packages_per_device,
      now,
    )
    .await?;
  if outcome == AddPackagesOutcome::OverLimit {
    return Err(ApiError::Conflict(format!(
      "a device keeps at most {} unused key packages",
      limits.max_key_packages_per_device
    )));
  }
  key_package_status(state, caller, device).await
}

pub async fn key_package_status(
  state: &AppState,
  caller: &AuthenticatedUser,
  device: DeviceId,
) -> Result<KeyPackageStatusResponse, ApiError> {
  if !state.database.is_active_device_of(caller.user.id, device).await? {
    return Err(ApiError::device_not_found());
  }
  let now = time::OffsetDateTime::now_utc();
  let renew_before = now + renewal_window(state);
  let counts = state
    .database
    .key_package_counts(device, now, renew_before)
    .await?;
  Ok(KeyPackageStatusResponse {
    available: u32::try_from(counts.available).unwrap_or(u32::MAX),
    expiring_soon: u32::try_from(counts.expiring_soon).unwrap_or(u32::MAX),
    has_last_resort: counts.has_last_resort,
  })
}

/// Takes one key package per device of `user`, to add them to a conversation both belong to.
/// Members of a conversation can do this for fellow members; clients decide (with the signed change
/// rules of e2ee.md §4.6) whether the add is allowed. Draining is limited to members and softened by
/// the last-resort package.
pub async fn claim_key_packages(
  state: &AppState,
  caller: &AuthenticatedUser,
  user: UserId,
  request: ClaimKeyPackagesRequest,
) -> Result<ClaimKeyPackagesResponse, ApiError> {
  let access = load_conversation(state, caller, request.conversation_id).await?;
  if !access.is_member() {
    return Err(ApiError::Forbidden(
      "only members can add devices to the group".into(),
    ));
  }
  let target = visible_user(state, caller, user).await?;
  let members = state
    .database
    .conversation_member_ids(request.conversation_id)
    .await?;
  if !members.contains(&target.id) {
    return Err(ApiError::InvalidRequest(
      "the user is not a member of the conversation".into(),
    ));
  }

  let candidates: Vec<_> = state
    .database
    .list_devices(target.id)
    .await?
    .into_iter()
    .filter(|device| {
      request
        .maybe_only_device_ids
        .as_ref()
        .is_none_or(|only| only.contains(&device.id))
    })
    .filter(|device| !request.exclude_device_ids.contains(&device.id))
    .collect();
  // The filters left nothing to claim: no device is missing from the group.
  if candidates.is_empty() {
    return Ok(ClaimKeyPackagesResponse { packages: Vec::new() });
  }

  // A package that expires soon would be refused by members who process the add late.
  let valid_until = time::OffsetDateTime::now_utc() + min_validity(state);
  let mut packages = Vec::new();
  for device in candidates {
    if let Some(claimed) = state.database.claim_key_package(device.id, valid_until).await? {
      packages.push(ClaimedKeyPackage {
        device_id: device.id,
        key_package: Base64Bytes::new(claimed.key_package),
        is_last_resort: claimed.is_last_resort,
      });
    }
  }
  if packages.is_empty() {
    return Err(ApiError::KeyPackageExhausted);
  }
  Ok(ClaimKeyPackagesResponse { packages })
}

/// Deletes key packages that can't be handed out any more. Run periodically.
pub async fn sweep_expired(state: &AppState, now: time::OffsetDateTime) -> Result<u64, ApiError> {
  Ok(
    state
      .database
      .delete_expiring_key_packages(now + min_validity(state))
      .await?,
  )
}

/// Checks a package and reads its expiry; one that expires before it could be handed out is
/// refused (the uploader's clock is far behind).
fn new_package<'a>(
  state: &AppState,
  package: &'a Base64Bytes,
  is_last_resort: bool,
  valid_until: time::OffsetDateTime,
) -> Result<NewKeyPackage<'a>, ApiError> {
  let info = inspect_key_package(package.as_slice())
    .map_err(|error| ApiError::InvalidKeyPackage(error.to_string()))?;
  if info.is_last_resort != is_last_resort {
    return Err(ApiError::InvalidKeyPackage(
      "the last_resort extension does not match the upload field".into(),
    ));
  }
  let expires_at = i64::try_from(info.not_after_unix_secs)
    .ok()
    .and_then(|secs| time::OffsetDateTime::from_unix_timestamp(secs).ok())
    .ok_or_else(|| ApiError::InvalidKeyPackage("expiry out of range".into()))?;
  if expires_at <= valid_until {
    return Err(ApiError::InvalidKeyPackage(
      "the key package expires too soon".into(),
    ));
  }
  Ok(NewKeyPackage {
    id: next_snowflake(state)?,
    bytes: package.as_slice(),
    expires_at,
  })
}

fn min_validity(state: &AppState) -> Duration {
  let limits = &state.settings.get().limits;
  // Never more than the renewal window, or a fresh package could count as unusable.
  Duration::from_secs(
    limits
      .key_package_min_validity_secs
      .min(limits.key_package_renewal_window_secs),
  )
}

fn renewal_window(state: &AppState) -> Duration {
  Duration::from_secs(state.settings.get().limits.key_package_renewal_window_secs)
}

fn next_snowflake(state: &AppState) -> Result<Snowflake, ApiError> {
  state
    .ids
    .next_id::<DeviceId>()
    .map(|id| id.snowflake())
    .map_err(|e| ApiError::Internal(e.to_string()))
}
