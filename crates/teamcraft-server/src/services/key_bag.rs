//! Encrypted key bag storage (e2ee.md §6): opaque, versioned, one per user. The one thing the
//! server reads is which methods the wrap list holds, to apply the signed policy's
//! `recovery.is_passphrase_allowed` (e2ee.md §6.5.8).

use teamcraft_crypto::backup::{RawWrappedKey, WrapMethod, split_wrapped_key_list};
use teamcraft_db::KeyBagOutcome;
use teamcraft_proto::{Base64Bytes, GatewayEvent, KeyBagResponse, PutKeyBagRequest, UserKind};

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
  if passphrase_rule(state, caller) == PassphraseRule::OnlyStoredOnes {
    let maybe_stored = state.database.get_key_bag(caller.user.id).await?;
    check_passphrase_wraps(
      request.wrapped_backup_keys.as_slice(),
      maybe_stored
        .as_ref()
        .map(|bag| bag.wrapped_backup_keys.as_slice()),
    )?;
  }

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

/// Which passphrase wraps an upload of `caller` may hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PassphraseRule {
  Any,
  /// Only ones byte-identical to a wrap already stored (the user's unchanged legacy passphrase).
  OnlyStoredOnes,
}

/// The signed policy's rule for `caller`. Guests' bags are wrapped with a random secret nobody
/// keeps and bots' with the bridge's configured passphrase, so the policy is about members.
pub(crate) fn passphrase_rule(state: &AppState, caller: &AuthenticatedUser) -> PassphraseRule {
  let is_member = caller.user.kind == UserKind::Member;
  if is_member && !state.policy.get().recovery.is_passphrase_allowed {
    PassphraseRule::OnlyStoredOnes
  } else {
    PassphraseRule::Any
  }
}

/// Refuses a list with a passphrase wrap that `maybe_stored` doesn't hold byte for byte: a new or
/// changed passphrase. Under this rule a list that doesn't split is refused as malformed.
pub(crate) fn check_passphrase_wraps(uploaded: &[u8], maybe_stored: Option<&[u8]>) -> Result<(), ApiError> {
  let malformed = |_| ApiError::InvalidRequest("malformed wrapped backup keys".into());
  let uploaded = split_wrapped_key_list(uploaded).map_err(malformed)?;
  // A stored list that doesn't split (written before the policy) holds no wrap to keep.
  let stored: Vec<RawWrappedKey<'_>> = maybe_stored
    .and_then(|bytes| split_wrapped_key_list(bytes).ok())
    .unwrap_or_default();
  let is_kept = |wrap: &RawWrappedKey<'_>| stored.iter().any(|old| old.as_bytes() == wrap.as_bytes());
  if uploaded
    .iter()
    .any(|wrap| wrap.is_method(WrapMethod::Passphrase) && !is_kept(wrap))
  {
    return Err(ApiError::RecoveryPassphraseNotAllowed);
  }
  Ok(())
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

#[cfg(test)]
mod tests {
  use teamcraft_crypto::backup::{
    Argon2Settings, BackupKey, BackupKeyWrapper, PassphraseWrapper, RecoveryKeyWrapper, WrappedBackupKey,
    encode_wrapped_key_list,
  };
  use teamcraft_crypto::ids::UserId;
  use teamcraft_crypto::recovery_key::RecoveryKey;
  use teamcraft_crypto::secret::SecretBytes;

  use super::*;

  const ALICE: UserId = UserId::new(1);
  const CHEAP: Argon2Settings = Argon2Settings {
    memory_kib: 64,
    iterations: 1,
    parallelism: 1,
  };

  #[test]
  fn a_kit_alone_passes() {
    let (kit, _) = wraps();
    let list = encode_wrapped_key_list(&[kit]).unwrap();
    assert!(check_passphrase_wraps(&list, None).is_ok());
  }

  #[test]
  fn a_new_passphrase_is_refused() {
    let (kit, passphrase) = wraps();
    let first_bag = encode_wrapped_key_list(std::slice::from_ref(&passphrase)).unwrap();
    assert!(matches!(
      check_passphrase_wraps(&first_bag, None),
      Err(ApiError::RecoveryPassphraseNotAllowed)
    ));
    let stored = encode_wrapped_key_list(&[kit]).unwrap();
    let added = encode_wrapped_key_list(&[passphrase]).unwrap();
    assert!(matches!(
      check_passphrase_wraps(&added, Some(&stored)),
      Err(ApiError::RecoveryPassphraseNotAllowed)
    ));
  }

  #[test]
  fn the_unchanged_legacy_passphrase_passes_and_a_changed_one_does_not() {
    let (kit, passphrase) = wraps();
    let stored = encode_wrapped_key_list(std::slice::from_ref(&passphrase)).unwrap();
    // Another device uploads a content key change: same wraps.
    assert!(check_passphrase_wraps(&stored, Some(&stored)).is_ok());
    // Replaced by a kit: the passphrase wrap goes.
    let replaced = encode_wrapped_key_list(std::slice::from_ref(&kit)).unwrap();
    assert!(check_passphrase_wraps(&replaced, Some(&stored)).is_ok());
    // Kept next to a kit.
    let both = encode_wrapped_key_list(&[passphrase, kit]).unwrap();
    assert!(check_passphrase_wraps(&both, Some(&stored)).is_ok());
    // Changed: the same method, other bytes.
    let (_, changed) = wraps();
    let changed = encode_wrapped_key_list(&[changed]).unwrap();
    assert!(matches!(
      check_passphrase_wraps(&changed, Some(&stored)),
      Err(ApiError::RecoveryPassphraseNotAllowed)
    ));
  }

  #[test]
  fn a_list_that_does_not_split_is_refused() {
    assert!(matches!(
      check_passphrase_wraps(b"wrapped", None),
      Err(ApiError::InvalidRequest(_))
    ));
    // A stored list that doesn't split holds nothing to keep, but doesn't block a kit.
    let (kit, _) = wraps();
    let list = encode_wrapped_key_list(&[kit]).unwrap();
    assert!(check_passphrase_wraps(&list, Some(b"wrapped")).is_ok());
  }

  /// A recovery-key wrap and a passphrase wrap of the same backup key.
  fn wraps() -> (WrappedBackupKey, WrappedBackupKey) {
    let backup_key = BackupKey::generate(&mut rand_core::OsRng);
    let kit = RecoveryKeyWrapper::new(&RecoveryKey::generate(&mut rand_core::OsRng))
      .wrap_key(ALICE, &backup_key, &mut rand_core::OsRng)
      .unwrap();
    let passphrase = PassphraseWrapper::new(SecretBytes::new(b"correct horse battery".to_vec()), CHEAP)
      .unwrap()
      .wrap_key(ALICE, &backup_key, &mut rand_core::OsRng)
      .unwrap();
    (kit, passphrase)
  }
}
