//! Saved items, drafts and saved searches (account data inside the key bag, e2ee.md §6.4).

use teamcraft_crypto::key_bag::EncryptedKeyBag;

use super::Client;
use super::enroll::WrapsOnMerge;
use teamcraft_proto::{Base64Bytes, UserId};

use crate::account_data::{AccountData, PinnedKey};
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

  /// Writes the decoded account data next to the key bag for readers without the backup key (the
  /// UI), as the key bag holds it now: after enrollment, before anything changed it.
  pub(crate) fn mirror_account_data(&mut self) -> Result<(), CoreError> {
    let data = self.account_data();
    self.store_account_data(&data)
  }

  /// Records that the recovery passphrase was set now (issue #410): in the account data, so the
  /// user's other devices learn it with the next key bag upload. Leaves the stored key bag alone
  /// (enrollment stores it with the account); [`Self::update_account_data`] does that otherwise.
  pub(crate) fn note_passphrase_set(&mut self) -> Result<(), CoreError> {
    let mut data = self.account_data();
    data.maybe_recovery_passphrase_set_at = Some(self.now_ms());
    self.store_account_data(&data)?;
    self.sync.is_key_bag_dirty = true;
    self.persist_sync()
  }

  /// Whether local key bag changes (content keys or account data) wait for upload.
  pub fn is_key_bag_dirty(&self) -> bool {
    self.sync.is_key_bag_dirty
  }

  /// Brings the account data up to date with the user's other devices: merges a newer key bag
  /// from the server, and uploads local changes.
  pub async fn sync_account_data(&mut self) -> Result<(), CoreError> {
    let remote = match self.read_ahead.key_bag.take() {
      Some(remote) => remote?,
      None => self.api.key_bag().await?,
    };
    if remote.version > self.sync.key_bag_version {
      self.merge_key_bag(
        remote.encrypted_key_bag.as_slice(),
        remote.version,
        remote.wrapped_backup_keys.as_slice(),
        WrapsOnMerge::Adopt,
      )?;
    }
    self.share_trust_decisions()?;
    if self.sync.is_key_bag_dirty {
      self.upload_key_bag().await?;
    }
    Ok(())
  }

  /// Accepts a changed identity key the server reported for `user` (after the user compared it):
  /// this device and, through the account data, the user's other devices use it from now on.
  pub fn accept_identity_change(&mut self, user: UserId) -> Result<(), CoreError> {
    let mut pinned = records::pinned_identities(&self.store)?;
    let Some(entry) = pinned.get_mut(&user) else {
      return Ok(());
    };
    let Some(new_key) = entry.maybe_changed_to.take() else {
      return Ok(());
    };
    entry.public_key = new_key;
    records::pin_identity(&self.store, user, entry)?;
    self.update_account_data(|data, now| data.pin(user, new_key, now))
  }

  /// Brings this device's pins and the account data's into line: decisions from the user's other
  /// devices win (they are the user's own), this device's new pins are added for the others.
  fn share_trust_decisions(&mut self) -> Result<(), CoreError> {
    let shared = self.account_data();
    let mut local = records::pinned_identities(&self.store)?;
    for (user, entry) in &mut local {
      if let Some(key) = shared.pinned_key(*user)
        && key != entry.public_key
      {
        entry.public_key = key;
        entry.maybe_changed_to = None;
        records::pin_identity(&self.store, *user, entry)?;
      }
    }
    let workspace_root = self.workspace()?.maybe_root;
    self.update_account_data(|data, now| {
      let mut is_changed = false;
      for (user, entry) in &local {
        if data.pinned_key(*user).is_none() && entry.maybe_changed_to.is_none() {
          is_changed |= data.pin(*user, entry.public_key, now);
        }
      }
      if data.maybe_workspace_root.is_none()
        && let Some(root) = &workspace_root
      {
        data.maybe_workspace_root = Some(PinnedKey {
          user_id: root.user_id,
          public_key: Base64Bytes::new(root.identity_key.to_vec()),
        });
        is_changed = true;
      }
      is_changed
    })
  }

  /// Merges another device's key bag: content keys are added, account data entries take the newer
  /// side. A bag older than one this device saw is refused (security review B11).
  pub(crate) fn merge_key_bag(
    &mut self,
    encrypted: &[u8],
    version: u64,
    wrapped_keys: &[u8],
    wraps: WrapsOnMerge,
  ) -> Result<(), CoreError> {
    let encrypted = EncryptedKeyBag::from_bytes(encrypted)?;
    if encrypted.sequence() < self.sync.key_bag_sequence {
      return Err(CoreError::KeyBagRollback {
        seen: self.sync.key_bag_sequence,
        offered: encrypted.sequence(),
      });
    }
    let decrypted = encrypted.decrypt(
      convert::user(self.account.user_id),
      &self.account.backup_key(),
      wrapped_keys,
    )?;
    self.sync.key_bag_sequence = encrypted.sequence();
    if wraps == WrapsOnMerge::Adopt {
      self.store_wrapped_keys_blob(wrapped_keys)?;
    }
    for ring in decrypted.conversations() {
      // A device that joined by external commit takes the current key from a member, not from
      // a key bag that may predate the last key switch (#178).
      let conversation = convert::conversation_from_crypto(ring.conversation_id())?;
      let is_waiting = super::member_device::is_waiting_for_current_key(&self.store, conversation)?;
      let local = self.account.key_bag.ring_mut(ring.conversation_id());
      for key in ring.keys() {
        local.insert(key.duplicate())?;
      }
      if local.current().is_none()
        && !is_waiting
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
    // Removals another device of the user saw count here before the next message is decrypted.
    self.apply_shared_removals()?;
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
