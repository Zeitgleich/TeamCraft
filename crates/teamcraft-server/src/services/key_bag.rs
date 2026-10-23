//! Encrypted key bag storage (e2ee.md §6): opaque, versioned, one per user.

use teamcraft_db::KeyBagOutcome;
use teamcraft_proto::{Base64Bytes, GatewayEvent, KeyBagResponse, PutKeyBagRequest};

use super::convert::unix_millis;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

pub async fn put_key_bag(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: PutKeyBagRequest,
) -> Result<KeyBagResponse, ApiError> {
  check_size(state, &request.encrypted_key_bag, &request.wrapped_backup_keys)?;

  let outcome = state
    .database
    .put_key_bag(
      caller.user.id,
      request.expected_version,
      request.encrypted_key_bag.as_slice(),
      request.wrapped_backup_keys.as_slice(),
    )
    .await?;
  match outcome {
    KeyBagOutcome::Stored { version } => {
      // The user's other devices fetch the new bag.
      state
        .hub
        .publish([caller.user.id], &GatewayEvent::KeyBagUpdate { version });
      get_key_bag(state, caller).await
    },
    KeyBagOutcome::VersionConflict { current_version } => Err(ApiError::VersionConflict { current_version }),
  }
}

/// A key bag needs both parts and stays within `limits.max_key_bag_bytes`.
pub(crate) fn check_size(
  state: &AppState,
  encrypted_key_bag: &Base64Bytes,
  wrapped_backup_keys: &Base64Bytes,
) -> Result<(), ApiError> {
  if encrypted_key_bag.is_empty() || wrapped_backup_keys.is_empty() {
    return Err(ApiError::InvalidRequest(
      "key bag and wrapped backup keys must not be empty".into(),
    ));
  }
  let max = state.settings.get().limits.max_key_bag_bytes;
  if encrypted_key_bag.len() + wrapped_backup_keys.len() > max {
    return Err(ApiError::PayloadTooLarge(format!("key bag exceeds {max} bytes")));
  }
  Ok(())
}

pub async fn get_key_bag(state: &AppState, caller: &AuthenticatedUser) -> Result<KeyBagResponse, ApiError> {
  let bag = state
    .database
    .get_key_bag(caller.user.id)
    .await?
    .ok_or_else(ApiError::key_bag_not_found)?;
  Ok(KeyBagResponse {
    version: bag.version,
    encrypted_key_bag: Base64Bytes::new(bag.encrypted_key_bag),
    wrapped_backup_keys: Base64Bytes::new(bag.wrapped_backup_keys),
    updated_at: unix_millis(bag.updated_at),
  })
}
