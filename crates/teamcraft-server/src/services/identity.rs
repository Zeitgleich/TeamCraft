//! Public identity keys and device certificates (e2ee.md §3). The server stores and serves them;
//! whether to trust them is decided by clients (signatures, OIDC attestation, pinning).
//!
//! A new identity key comes with its first key bag and both are stored in one transaction
//! (e2ee.md §3.2): an interrupted enrollment can't leave an identity nobody can recover.

use teamcraft_db::{IdentityKey, IdentityKeyOutcome, InitialKeyBag, NewIdentityKey};
use teamcraft_proto::{
  Base64Bytes, GatewayEvent, IdentityKeyResponse, ListDevicesResponse, UploadIdentityKeyRequest,
  UserEnrollmentResponse, UserId,
};

use super::convert::{device_response, unix_millis};
use super::device_lists::device_list_response;
use super::directory::visible_user;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

const MAX_KEY_BYTES: usize = 4096;
const MAX_ATTESTATION_CHARS: usize = 16 * 1024;

pub async fn put_identity_key(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: UploadIdentityKeyRequest,
) -> Result<IdentityKeyResponse, ApiError> {
  for (what, bytes) in [
    ("public_key", &request.public_key),
    ("signature", &request.signature),
  ] {
    if bytes.is_empty() || bytes.len() > MAX_KEY_BYTES {
      return Err(ApiError::InvalidRequest(format!("{what} has an invalid size")));
    }
  }
  if request
    .maybe_attestation
    .as_ref()
    .is_some_and(|token| token.chars().count() > MAX_ATTESTATION_CHARS)
  {
    return Err(ApiError::InvalidRequest("attestation is too long".into()));
  }
  // A guest's identity key is pinned by the members on first use; resetting it would make a
  // second key look like the same guest (e2ee.md §4.6, "Meeting guests").
  if request.replace && caller.user.kind == teamcraft_proto::UserKind::Guest {
    return Err(ApiError::Forbidden(
      "guests can't replace their identity key".into(),
    ));
  }
  let key_bag = request
    .maybe_key_bag
    .as_ref()
    .ok_or_else(|| ApiError::InvalidRequest("a new identity key needs its key bag".into()))?;
  super::key_bag::check_size(state, &key_bag.encrypted_key_bag, &key_bag.wrapped_backup_keys)?;

  let outcome = state
    .database
    .put_identity_key(&NewIdentityKey {
      id: state
        .ids
        .next_snowflake()
        .map_err(|e| ApiError::Internal(e.to_string()))?,
      user_id: caller.user.id,
      public_key: request.public_key.as_slice(),
      signature: request.signature.as_slice(),
      maybe_attestation: request.maybe_attestation.as_deref(),
      replace: request.replace,
      key_bag: InitialKeyBag {
        encrypted_key_bag: key_bag.encrypted_key_bag.as_slice(),
        wrapped_backup_keys: key_bag.wrapped_backup_keys.as_slice(),
      },
    })
    .await?;
  let IdentityKeyOutcome::Stored { key_bag_version } = outcome else {
    return Err(ApiError::Conflict(
      "an identity key exists; set replace to reset it".into(),
    ));
  };
  // The user's other devices fetch the new bag (after a reset).
  state.hub.publish(
    [caller.user.id],
    &GatewayEvent::KeyBagUpdate {
      version: key_bag_version,
    },
  );
  let mut response = get_identity_key(state, caller, caller.user.id).await?;
  response.maybe_key_bag_version = Some(key_bag_version);
  Ok(response)
}

pub async fn get_identity_key(
  state: &AppState,
  caller: &AuthenticatedUser,
  user: UserId,
) -> Result<IdentityKeyResponse, ApiError> {
  let user = visible_user(state, caller, user).await?;
  let key = state
    .database
    .get_identity_key(user.id)
    .await?
    .ok_or(ApiError::identity_key_not_found())?;
  Ok(identity_key_response(key))
}

/// The identity key and device list a user has published; neither before their first device is
/// enrolled. Clients probe this instead of the single endpoints, whose 404 browsers log as errors.
pub async fn get_user_enrollment(
  state: &AppState,
  caller: &AuthenticatedUser,
  user: UserId,
) -> Result<UserEnrollmentResponse, ApiError> {
  let user = visible_user(state, caller, user).await?;
  let maybe_identity_key = state.database.get_identity_key(user.id).await?;
  let maybe_device_list = state.database.get_device_list(user.id).await?;
  Ok(UserEnrollmentResponse {
    maybe_identity_key: maybe_identity_key.map(identity_key_response),
    maybe_device_list: maybe_device_list.map(device_list_response),
  })
}

fn identity_key_response(key: IdentityKey) -> IdentityKeyResponse {
  IdentityKeyResponse {
    user_id: key.user_id,
    public_key: Base64Bytes::new(key.public_key),
    signature: Base64Bytes::new(key.signature),
    maybe_attestation: key.maybe_attestation,
    created_at: unix_millis(key.created_at),
    maybe_key_bag_version: None,
  }
}

/// The device keys of a user with their cross-signatures, so peers can build MLS groups.
pub async fn list_user_devices(
  state: &AppState,
  caller: &AuthenticatedUser,
  user: UserId,
) -> Result<ListDevicesResponse, ApiError> {
  let user = visible_user(state, caller, user).await?;
  let devices = state.database.list_devices(user.id).await?;
  Ok(ListDevicesResponse {
    devices: devices.iter().map(device_response).collect(),
  })
}
