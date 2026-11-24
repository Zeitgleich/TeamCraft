//! Passkeys as a recovery factor (e2ee.md §6.2.1, issue #1965).
//!
//! The UI runs the WebAuthn ceremony (create the credential with the PRF extension, then evaluate
//! it with the PRF input [`PasskeyCredential::generate`] drew); the core only sees the credential
//! ID, the PRF input and the 32-byte PRF output, wraps the backup key with it and uploads the key
//! bag. The output is wiped once the wrap is made.

use rand_core::OsRng;
use teamcraft_crypto::backup::{
  self, BackupKeyWrapper, PasskeyCredential, PasskeyPrfWrapper, WrapMethod, WrappedBackupKey,
};
use teamcraft_crypto::secret::SecretBytes;

use super::Client;
use super::enroll::WrapsOnMerge;
use crate::api::ServerApi;
use crate::convert;
use crate::error::CoreError;
use crate::store::LocalStore;

/// Most passkeys one account's backup is wrapped with: each is a wrap in the list the key bag
/// binds, and a handful covers every authenticator a person uses.
pub const MAX_PASSKEYS: usize = 10;

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// The passkeys that currently open the backup, in the order they were added.
  pub async fn passkeys(&mut self) -> Result<Vec<PasskeyCredential>, CoreError> {
    Ok(backup::passkey_credentials(&self.current_wrapped_keys().await?))
  }

  /// Wraps the backup key with a passkey's PRF output and uploads the key bag with the new wrap
  /// (replacing an earlier wrap of the same credential). The other factors stay.
  pub async fn add_passkey(
    &mut self,
    credential: PasskeyCredential,
    prf_output: &SecretBytes,
  ) -> Result<(), CoreError> {
    let wrapper = PasskeyPrfWrapper::new(credential, prf_output)?;
    let wrapped = wrapper.wrap_key(
      convert::user(self.account.user_id),
      &self.account.backup_key(),
      &mut OsRng,
    )?;
    let mut keys = self
      .wraps_without_passkey(wrapper.credential().credential_id())
      .await?;
    let passkeys = keys
      .iter()
      .filter(|existing| existing.method() == WrapMethod::PasskeyPrf)
      .count();
    if passkeys >= MAX_PASSKEYS {
      return Err(CoreError::TooManyPasskeys { max: MAX_PASSKEYS });
    }
    keys.push(wrapped);
    self.store_wrapped_keys(&keys)?;
    self.upload_key_bag_keeping(WrapsOnMerge::KeepOwn).await
  }

  /// Stops a passkey from opening the backup: its wrap leaves the list. A server that kept the old
  /// list can still use it with the passkey, as for any replaced factor; a rotation (e2ee.md §6.6)
  /// ends that.
  pub async fn remove_passkey(&mut self, credential_id: &[u8]) -> Result<(), CoreError> {
    let keys = self.wraps_without_passkey(credential_id).await?;
    self.store_wrapped_keys(&keys)?;
    self.upload_key_bag_keeping(WrapsOnMerge::KeepOwn).await
  }

  async fn wraps_without_passkey(
    &mut self,
    credential_id: &[u8],
  ) -> Result<Vec<WrappedBackupKey>, CoreError> {
    Ok(
      self
        .current_wrapped_keys()
        .await?
        .into_iter()
        .filter(|existing| {
          existing.method() != WrapMethod::PasskeyPrf
            || PasskeyCredential::of_wrap(existing).is_ok_and(|named| named.credential_id() != credential_id)
        })
        .collect(),
    )
  }
}
