//! The sync engine: one [`Client`] per signed-in device.
//!
//! It owns the account keys, the MLS groups and the local store, talks to the server through a
//! [`ServerApi`] and keeps the store current. UIs read the store (see [`crate::view`]); every
//! network-facing operation is an `async` method here.

mod account;
mod actions;
mod attachments;
mod calls;
mod enroll;
pub mod files;
mod gateway;
mod groups;
mod linking;
mod messaging;
mod notifications;
mod org_recovery;
pub mod previews;
#[cfg(not(target_arch = "wasm32"))]
mod segments;
mod sync;
#[cfg(feature = "ui")]
mod ui_commands;
mod workspace;
pub mod workspace_admin;
pub(crate) use workspace::WORKSPACE_KEY;

use std::collections::{BTreeMap, HashMap};

pub use account::stored_account_data;
/// Where the engine records its upload limit for the UI.
pub const MAX_UPLOAD_KEY: &str = "max_upload_bytes";
pub use actions::{OutgoingText, REPLY_EXCERPT_CHARS, forward_content, reply_ref};
pub use attachments::OutgoingFile;
pub use calls::{CallEventSink, CallKeySigner};
pub use enroll::{EnrollmentOptions, OrgRecoveryUnlock, PendingEnrollment};
pub use gateway::GatewayOutcome;
pub use linking::{LinkApproval, LinkOffer, LinkedSecrets, NewDeviceLink};
pub use messaging::FlushReport;
#[cfg(feature = "ui")]
pub(crate) use messaging::apply_control;
pub(crate) use messaging::decrypt_content;
pub use notifications::TYPING_TIMEOUT_MS;
use openmls_rust_crypto::OpenMlsRustCrypto;
pub use org_recovery::SealedShare;
use teamcraft_crypto::membership::ChangeId;
use teamcraft_crypto::mls::ConversationGroup;
use teamcraft_proto::{ConversationId, ConversationResponse, UserId};

use crate::api::ServerApi;
use crate::clock::Clock;
use crate::convert;
use crate::error::CoreError;
use crate::records::{self, Account, PinnedIdentity, SyncState};
use crate::store::{LocalStore, Table};
use crate::trust::PinnedDirectory;

/// Tunables of the engine. Configurable, with defaults that suit a desktop client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientSettings {
  /// Regular key packages kept on the server.
  pub key_package_target: u32,
  /// Top up when fewer than this are left.
  pub key_package_low_water: u32,
  /// Messages per history page.
  pub history_page_size: u32,
  /// Attempts to land a commit when other members' commits win the epoch.
  pub max_commit_attempts: u32,
  /// Delay per rank before a member commits a colleague's new device, and between retries while
  /// the device has no key package yet.
  pub device_add_stagger: std::time::Duration,
  /// Largest file this client uploads (the server's default limit; it rejects larger ones too).
  pub max_upload_bytes: u64,
  /// Where downloaded files are written (native apps); `None` disables downloads to disk.
  pub maybe_download_dir: Option<std::path::PathBuf>,
  pub link_previews: previews::LinkPreviewSettings,
  /// Checks before giving up on adding a new device that never uploads key packages.
  pub device_add_attempts: u32,
  /// Ranks beyond this commit together at `device_add_max_rank × device_add_stagger`: bounds how
  /// long a new device waits when the first-ranked members are away (presence isn't per device).
  pub device_add_max_rank: u32,
}

pub struct Client<A, S> {
  api: A,
  store: S,
  clock: Box<dyn Clock>,
  settings: ClientSettings,
  provider: OpenMlsRustCrypto,
  account: Account,
  groups: HashMap<ConversationId, ConversationGroup>,
  sync: SyncState,
  typing: notifications::TypingState,
  maybe_call_sink: Option<CallEventSink>,
  maybe_preview_fetcher: Option<Box<dyn crate::link_preview::PreviewFetcher>>,
}

impl Default for ClientSettings {
  fn default() -> Self {
    Self {
      key_package_target: 30,
      key_package_low_water: 10,
      history_page_size: 50,
      max_commit_attempts: 3,
      device_add_stagger: std::time::Duration::from_secs(10),
      device_add_attempts: 6,
      max_upload_bytes: 100 * 1024 * 1024,
      maybe_download_dir: None,
      link_previews: previews::LinkPreviewSettings::default(),
      device_add_max_rank: 3,
    }
  }
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Opens the client of an enrolled device; `None` if the store holds no account yet (enroll).
  pub fn open(
    api: A,
    store: S,
    clock: Box<dyn Clock>,
    settings: ClientSettings,
  ) -> Result<Option<Self>, CoreError> {
    let Some(account) = records::load_account(&store)? else {
      return Ok(None);
    };
    let provider = records::load_mls_storage(&store)?;
    // The UI shows the upload limit before sending.
    records::put_json(&store, Table::Account, MAX_UPLOAD_KEY, &settings.max_upload_bytes)?;
    let sync = records::load_sync_state(&store)?;
    let maybe_preview_fetcher = previews::default_fetcher(&settings.link_previews);
    Ok(Some(Self {
      api,
      store,
      clock,
      settings,
      provider,
      account,
      groups: HashMap::new(),
      sync,
      typing: Default::default(),
      maybe_call_sink: None,
      maybe_preview_fetcher,
    }))
  }

  /// Where downloaded files go (native apps; e.g. the user's downloads folder).
  pub fn set_download_dir(&mut self, maybe_directory: Option<std::path::PathBuf>) {
    self.settings.maybe_download_dir = maybe_directory;
  }

  pub fn user_id(&self) -> UserId {
    self.account.user_id
  }

  pub fn device_id(&self) -> teamcraft_proto::DeviceId {
    self.account.device_id
  }

  pub fn store(&self) -> &S {
    &self.store
  }

  pub fn api(&self) -> &A {
    &self.api
  }

  /// Fingerprint of the user's identity key, for optional verification (e2ee.md §1).
  pub fn identity_fingerprint(&self) -> String {
    self.account.identity_public_key().fingerprint().to_string()
  }

  /// Whether this device holds an MLS group for the conversation (i.e. can read new messages).
  pub fn has_group(&mut self, conversation: ConversationId) -> Result<bool, CoreError> {
    Ok(self.group(conversation)?.is_some())
  }

  /// Users whose identity key changed since it was pinned (e2ee.md §1: "security key changed").
  pub fn changed_identities(&self) -> Result<Vec<UserId>, CoreError> {
    Ok(
      records::pinned_identities(&self.store)?
        .into_iter()
        .filter(|(_, pinned)| pinned.maybe_changed_to.is_some())
        .map(|(user, _)| user)
        .collect(),
    )
  }

  fn group(&mut self, conversation: ConversationId) -> Result<Option<&mut ConversationGroup>, CoreError> {
    if !self.groups.contains_key(&conversation) {
      let applied: Vec<[u8; 16]> =
        records::get_json(&self.store, Table::AppliedChanges, &conversation.to_string())?.unwrap_or_default();
      let maybe_group = ConversationGroup::load(
        &self.provider,
        convert::conversation(conversation),
        applied.into_iter().map(ChangeId::from_bytes),
      )?;
      match maybe_group {
        Some(group) => {
          self.groups.insert(conversation, group);
        },
        None => return Ok(None),
      }
    }
    Ok(self.groups.get_mut(&conversation))
  }

  pub(crate) fn group_or_error(
    &mut self,
    conversation: ConversationId,
  ) -> Result<&mut ConversationGroup, CoreError> {
    self.group(conversation)?.ok_or(CoreError::NoGroup(conversation))
  }

  /// Forgets the conversation's MLS group on this device (removed, or lost the founding race).
  fn drop_group(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    if self.group(conversation)?.is_some()
      && let Some(group) = self.groups.remove(&conversation)
    {
      group.delete(&self.provider)?;
    }
    self
      .store
      .delete(Table::AppliedChanges, &conversation.to_string())?;
    records::save_mls_storage(&self.store, &self.provider)
  }

  /// Writes MLS state (provider storage and applied change IDs) after a group operation.
  fn persist_group(&self, conversation: ConversationId) -> Result<(), CoreError> {
    records::save_mls_storage(&self.store, &self.provider)?;
    if let Some(group) = self.groups.get(&conversation) {
      let applied: Vec<[u8; 16]> = group.applied_changes().map(|id| *id.as_bytes()).collect();
      records::put_json(
        &self.store,
        Table::AppliedChanges,
        &conversation.to_string(),
        &applied,
      )?;
    }
    Ok(())
  }

  /// Writes the key bag after content keys changed, and marks it for upload.
  pub(crate) fn persist_keys(&mut self) -> Result<(), CoreError> {
    records::save_key_bag(&self.store, &self.account)?;
    self.sync.is_key_bag_dirty = true;
    self.persist_sync()
  }

  fn persist_sync(&self) -> Result<(), CoreError> {
    records::save_sync_state(&self.store, &self.sync)
  }

  /// Fetches and pins the identity keys of `users` that aren't pinned yet; records changed keys
  /// without trusting them. Returns the directory of pinned keys.
  async fn pin_identities(
    &self,
    users: impl IntoIterator<Item = UserId>,
  ) -> Result<PinnedDirectory, CoreError> {
    let mut pinned = records::pinned_identities(&self.store)?;
    for user in users {
      if user == self.account.user_id {
        if let std::collections::btree_map::Entry::Vacant(slot) = pinned.entry(user) {
          let own = PinnedIdentity {
            public_key: *self.account.identity_public_key().as_bytes(),
            maybe_changed_to: None,
          };
          records::pin_identity(&self.store, user, &own)?;
          slot.insert(own);
        }
        continue;
      }
      if pinned
        .get(&user)
        .is_some_and(|identity| identity.maybe_changed_to.is_none())
      {
        continue;
      }
      let response = match self.api.identity_key(user).await {
        Ok(response) => response,
        // Not enrolled yet: they can't be in any group, so there's nothing to pin.
        Err(error) if error.is_not_found() => continue,
        Err(error) => return Err(error.into()),
      };
      let public_key = <[u8; 32]>::try_from(response.public_key.as_slice())
        .map_err(|_| CoreError::Corrupt(format!("identity key of {user} has the wrong size")))?;
      // TODO(attestation): verify `response.maybe_attestation` (ID token with claim
      // tc_identity_key) against the identity provider's JWKS before pinning (e2ee.md §3.1).
      let entry = match pinned.get(&user) {
        None => PinnedIdentity {
          public_key,
          maybe_changed_to: None,
        },
        Some(existing) if existing.public_key == public_key => PinnedIdentity {
          public_key,
          maybe_changed_to: None,
        },
        Some(existing) => {
          tracing::warn!(user_id = %user, "identity key changed on the server; keeping the pinned key");
          PinnedIdentity {
            public_key: existing.public_key,
            maybe_changed_to: Some(public_key),
          }
        },
      };
      records::pin_identity(&self.store, user, &entry)?;
      pinned.insert(user, entry);
    }
    Ok(PinnedDirectory::new(
      &pinned,
      &self.workspace()?.roster_identities(),
    ))
  }

  fn cached_conversations(&self) -> Result<BTreeMap<ConversationId, ConversationResponse>, CoreError> {
    let list: Vec<ConversationResponse> = records::list_json(&self.store, Table::Conversations)?;
    Ok(
      list
        .into_iter()
        .map(|conversation| (conversation.id, conversation))
        .collect(),
    )
  }

  pub(crate) fn now_ms(&self) -> i64 {
    self.clock.now_ms()
  }
}

impl<A, S> std::fmt::Debug for Client<A, S> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Client")
      .field("account", &self.account)
      .finish_non_exhaustive()
  }
}
