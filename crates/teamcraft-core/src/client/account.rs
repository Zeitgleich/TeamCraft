//! Saved items, drafts and saved searches (account data inside the key bag, e2ee.md §6.4).

use teamcraft_crypto::key_bag::EncryptedKeyBag;

use super::Client;
use crate::account_data::AccountData;
use crate::api::ServerApi;
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};

/// The decoded account data, kept next to the key bag for readers without the backup key (the UI).
pub(crate) const ACCOUNT_DATA_KEY: &str = "account_data";

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// The user's saved items, drafts and saved searches as this device knows them.
  pub fn account_data(&self) -> AccountData {
    AccountData::from_bytes(self.account.key_bag.account_data()).unwrap_or_else(|error| {
      tracing::error!(%error, "account data in the key bag is unreadable");
      AccountData::default()
    })
  }

  /// Changes the account data; `change` returns whether anything changed. Uploaded with the key bag
  /// on the next sync round.
  pub fn update_account_data(
    &mut self,
    change: impl FnOnce(&mut AccountData, i64) -> bool,
  ) -> Result<(), CoreError> {
    let mut data = self.account_data();
    if !change(&mut data, self.now_ms()) {
      return Ok(());
    }
    self.store_account_data(&data)?;
    self.persist_keys()
  }

  /// Whether local key bag changes (content keys or account data) wait for upload.
  pub fn is_key_bag_dirty(&self) -> bool {
    self.sync.is_key_bag_dirty
  }

  /// Brings the account data up to date with the user's other devices: merges a newer key bag
  /// from the server, and uploads local changes.
  pub async fn sync_account_data(&mut self) -> Result<(), CoreError> {
    let remote = self.api.key_bag().await?;
    if remote.version > self.sync.key_bag_version {
      self.merge_key_bag(&remote.encrypted_key_bag.into_vec(), remote.version)?;
    }
    if self.sync.is_key_bag_dirty {
      self.upload_key_bag().await?;
    }
    Ok(())
  }

  /// Merges another device's key bag: content keys are added, account data entries take the newer
  /// side.
  pub(crate) fn merge_key_bag(&mut self, encrypted: &[u8], version: u64) -> Result<(), CoreError> {
    let decrypted = EncryptedKeyBag::from_bytes(encrypted)?
      .decrypt(convert::user(self.account.user_id), &self.account.backup_key())?;
    for ring in decrypted.conversations() {
      let local = self.account.key_bag.ring_mut(ring.conversation_id());
      for key in ring.keys() {
        local.insert(key.duplicate())?;
      }
      if local.current().is_none()
        && let Some(current) = ring.current()
      {
        local.set_current(current.id())?;
      }
    }
    let remote_data = AccountData::from_bytes(decrypted.account_data())
      .map_err(|error| CoreError::Corrupt(error.to_string()))?;
    let mut data = self.account_data();
    data.merge(&remote_data, self.now_ms());
    self.store_account_data(&data)?;
    // Entries the other device didn't have yet still need an upload.
    if data != remote_data {
      self.sync.is_key_bag_dirty = true;
    }
    self.sync.key_bag_version = version;
    records::save_key_bag(&self.store, &self.account)?;
    self.persist_sync()
  }

  fn store_account_data(&mut self, data: &AccountData) -> Result<(), CoreError> {
    let bytes = data
      .to_bytes()
      .map_err(|error| CoreError::Corrupt(error.to_string()))?;
    self.account.key_bag.set_account_data(bytes);
    records::put_json(&self.store, Table::Account, ACCOUNT_DATA_KEY, data)
  }
}

/// The account data as last stored by the engine (for readers without the key bag).
pub fn stored_account_data(store: &impl LocalStore) -> Result<AccountData, CoreError> {
  Ok(records::get_json(store, Table::Account, ACCOUNT_DATA_KEY)?.unwrap_or_default())
}
