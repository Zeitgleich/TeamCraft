//! Re-founding the notes (e2ee.md §4.6, "Re-founding the notes", issue #910): a device that isn't
//! in the group of the user's notes to self, while the user's own signed device list names no
//! other active device, has nobody left who could add it. It founds the group anew, alone, and
//! takes the server's queue over with its first commit. History stays readable from the key bag.

use std::collections::BTreeSet;

use rand_core::OsRng;
use teamcraft_crypto::content_key::ContentKey;
use teamcraft_crypto::mls::ConversationGroup;
use teamcraft_proto::{Base64Bytes, ConversationId, ListMlsMessagesQuery, PostMlsCommitRequest, UserId};

use super::Client;
use super::groups::rejoin_key;
use super::member_device::{current_key_wait_key, device_add_key};
use crate::api::ServerApi;
use crate::convert;
use crate::error::CoreError;
use crate::store::{LocalStore, Table};
use crate::trust::PinnedDirectory;

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Founds the group of the user's notes anew when this device established on its own that no
  /// device could add it: the conversation is the notes (`users`, the server's members and this
  /// user, hold nobody else), and the user's newest signed device list in `directory` names this
  /// device and no other active one. The server's word alone (members, an epoch, a revocation it
  /// reports) never gets here: without that list nothing is founded. A hostile server that hides
  /// a later list only splits off the user's own devices; the new group holds nobody but this
  /// device, so nothing is readable to anyone else. `Ok(true)` once this device is in the new
  /// group; `Ok(false)` when the rules don't allow it or a device of the old group committed first
  /// (it is alive and adds this one).
  pub(super) async fn refound_notes(
    &mut self,
    conversation: ConversationId,
    users: &BTreeSet<UserId>,
    directory: &PinnedDirectory,
  ) -> Result<bool, CoreError> {
    if !self.may_refound_notes(conversation, users, directory)? {
      return Ok(false);
    }
    let query = ListMlsMessagesQuery {
      maybe_after: None,
      maybe_limit: Some(1),
    };
    let epoch = self
      .api
      .list_mls_messages(conversation, &query)
      .await?
      .current_epoch;
    if epoch > self.settings.max_refound_epoch {
      // Every epoch costs a local commit: a server can't make this device spin on a made-up one.
      tracing::warn!(conversation_id = %conversation, epoch, "the notes' group is too far on to found anew");
      return Ok(false);
    }
    let (group, output) = ConversationGroup::refound(
      &self.provider,
      &self.account.device,
      &self.account.certificate,
      convert::conversation(conversation),
      epoch,
    )?;
    // Kept in memory only until the server accepts: a crash before leaves nothing behind.
    self.groups.insert(conversation, group);
    let maybe_group_info = output
      .maybe_group_info
      .filter(|info| info.len() <= self.settings.max_group_info_bytes)
      .map(Base64Bytes::new);
    let request = PostMlsCommitRequest {
      device_id: self.account.device_id,
      epoch,
      commit: Base64Bytes::new(output.commit),
      welcomes: Vec::new(),
      maybe_group_info,
    };
    match self.api.post_mls_commit(conversation, &request).await {
      Ok(response) => {
        if let Some(group) = self.groups.get_mut(&conversation) {
          group.confirm_commit(&self.provider)?;
        }
        self.atomically(|client| {
          client.forget_key_history(conversation)?;
          client.persist_group_at(conversation, &response.commit)?;
          client
            .store
            .delete(Table::AppliedChanges, &device_add_key(conversation))?;
          client
            .store
            .delete(Table::AppliedChanges, &rejoin_key(conversation))?;
          client
            .store
            .delete(Table::AppliedChanges, &current_key_wait_key(conversation))?;
          Ok(())
        })?;
        self.start_refounded_key(conversation)?;
        self.member_join_attempts.remove(&conversation);
        self.retry_undecrypted(conversation)?;
        tracing::info!(conversation_id = %conversation, epoch, "founded the notes' group anew");
        Ok(true)
      },
      Err(error) => {
        self.drop_group(conversation)?;
        if error.is_epoch_conflict() {
          // A device of the old group committed first: it is alive, and adds this one.
          tracing::info!(conversation_id = %conversation, "the notes' old group moved on; not founded anew");
          return Ok(false);
        }
        Err(error.into())
      },
    }
  }

  /// The checks of [`Self::refound_notes`] that need nothing from the server.
  fn may_refound_notes(
    &mut self,
    conversation: ConversationId,
    users: &BTreeSet<UserId>,
    directory: &PinnedDirectory,
  ) -> Result<bool, CoreError> {
    let me = self.account.user_id;
    let is_notes = self
      .cached_conversations()?
      .get(&conversation)
      .is_some_and(|response| response.is_notes_to_self(me));
    if !is_notes || users.iter().any(|user| *user != me) || self.has_group(conversation)? {
      return Ok(false);
    }
    Ok(self.is_only_listed_device(directory))
  }

  /// Whether the user's newest signed device list names this device, with its signing key, and
  /// no other active device. No list (never published, or the server withholds it) is no proof.
  fn is_only_listed_device(&self, directory: &PinnedDirectory) -> bool {
    let own = self.account.device.public_keys();
    let own_device = convert::device(self.account.device_id);
    self
      .stored_device_lists(directory, &[self.account.user_id])
      .first()
      .is_some_and(|list| {
        let devices = list.devices();
        devices
          .iter()
          .any(|entry| entry.device_id == own_device && entry.signing_key == own.signing_key)
          && devices.iter().all(|entry| entry.device_id == own_device)
      })
  }

  /// A fresh current content key for the new group: the old one may be known to the lost
  /// devices. The old keys stay in the ring (and the key bag) for the history.
  fn start_refounded_key(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let key = ContentKey::generate(&mut OsRng);
    let key_id = key.id();
    let ring = self.account.key_bag.ring_mut(convert::conversation(conversation));
    ring.insert(key)?;
    ring.set_current(key_id)?;
    self.persist_keys()?;
    let epoch = self.group_or_error(conversation)?.epoch();
    self.note_key_epochs(conversation, &[key_id], epoch)
  }
}
