//! Public identity keys and device certificates (e2ee.md §3). The server stores and serves them;
//! whether to trust them is decided by clients (signatures, OIDC attestation, pinning).
//!
//! A new identity key comes with its first key bag and both are stored in one transaction
//! (e2ee.md §3.2): an interrupted enrollment can't leave an identity nobody can recover.

use teamcraft_crypto::attestation::{attestation_claim, unverified_claims};
use teamcraft_crypto::identity::IdentityPublicKey;
use teamcraft_db::{IdentityKey, IdentityKeyOutcome, InitialKeyBag, NewIdentityKey};
use teamcraft_proto::{
  Base64Bytes, DeviceResponse, GatewayEvent, Id as _, IdentityKeyResponse, ListDevicesResponse,
  PutIdentityKeyAttestationRequest, UploadIdentityKeyRequest, UserEnrollmentResponse, UserId,
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
  // A new identity's first bag has no stored wraps to keep (e2ee.md §6.5.8).
  if super::key_bag::passphrase_rule(state, caller) == super::key_bag::PassphraseRule::OnlyStoredOnes {
    super::key_bag::check_passphrase_wraps(key_bag.wrapped_backup_keys.as_slice(), None)?;
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

/// Stores the caller's attestation for the identity key they already published (e2ee.md §3.1): keys
/// enrolled before their app attested them get one this way, without a reset. Like the key itself, the
/// server judges nothing; it keeps out junk and tokens that name somebody else or another key.
pub async fn put_identity_key_attestation(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: PutIdentityKeyAttestationRequest,
) -> Result<(), ApiError> {
  if request.token.chars().count() > MAX_ATTESTATION_CHARS {
    return Err(ApiError::InvalidRequest("attestation is too long".into()));
  }
  let not_a_token = || ApiError::InvalidRequest("this is not an identity provider token".into());
  let subject = super::role_evidence::token_subject(&request.token).ok_or_else(not_a_token)?;
  let claims = unverified_claims(&request.token).map_err(|_| not_a_token())?;
  let issuer = state.validator.issuer().to_owned();
  let maybe_owner = state
    .database
    .find_user_by_oidc_subject(&issuer, &subject)
    .await?;
  if maybe_owner != Some(caller.user.id) {
    return Err(ApiError::Forbidden("this token belongs to somebody else".into()));
  }
  let key = state
    .database
    .get_identity_key(caller.user.id)
    .await?
    .ok_or(ApiError::identity_key_not_found())?;
  if claims.maybe_key_claim.as_deref() != expected_claim(caller.user.id, &key.public_key).as_deref() {
    return Err(ApiError::InvalidRequest(
      "the token doesn't name the current identity key".into(),
    ));
  }
  let is_stored = state
    .database
    .set_identity_key_attestation(caller.user.id, &key.public_key, &request.token)
    .await?;
  if !is_stored {
    // The key was reset in between: this token names the old one.
    return Err(ApiError::Conflict("the identity key changed".into()));
  }
  Ok(())
}

/// The claim an attestation of `public_key` carries; `None` for a stored key of the wrong size.
fn expected_claim(user: UserId, public_key: &[u8]) -> Option<String> {
  let key = <[u8; 32]>::try_from(public_key).ok()?;
  let raw = u64::try_from(user.snowflake().get()).ok()?;
  Some(attestation_claim(
    teamcraft_crypto::ids::UserId::new(raw),
    &IdentityPublicKey::from_bytes(key),
  ))
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
  let is_own = user.id == caller.user.id;
  Ok(ListDevicesResponse {
    devices: devices
      .iter()
      .map(|device| {
        let response = device_response(device);
        if is_own { response } else { keys_only(response) }
      })
      .collect(),
  })
}

/// Another user's device as their colleagues' clients need it: its keys. The name may be personal
/// ("Anna's phone") and the push provider tells the platform, so both stay with the owner (and
/// admins, who have their own list).
fn keys_only(device: DeviceResponse) -> DeviceResponse {
  DeviceResponse {
    name: String::new(),
    maybe_push_provider: None,
    ..device
  }
}
