//! Public identity keys and device certificates (e2ee.md §3). The server stores and serves them;
//! whether to trust them is decided by clients (signatures, OIDC attestation, pinning).

use teamcraft_db::{IdentityKeyOutcome, NewIdentityKey};
use teamcraft_proto::{
  Base64Bytes, IdentityKeyResponse, ListDevicesResponse, UploadIdentityKeyRequest, UserId,
};

use super::convert::{device_response, unix_millis};
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
    })
    .await?;
  if outcome == IdentityKeyOutcome::AlreadyExists {
    return Err(ApiError::Conflict(
      "an identity key exists; set replace to reset it".into(),
    ));
  }
  get_identity_key(state, caller, caller.user.id).await
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
  Ok(IdentityKeyResponse {
    user_id: key.user_id,
    public_key: Base64Bytes::new(key.public_key),
    signature: Base64Bytes::new(key.signature),
    maybe_attestation: key.maybe_attestation,
    created_at: unix_millis(key.created_at),
  })
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
