//! The sync engine: one [`Client`] per signed-in device.
//!
//! It owns the account keys, the MLS groups and the local store, talks to the server through a
//! [`ServerApi`] and keeps the store current. UIs read the store (see [`crate::view`]); every
//! network-facing operation is an `async` method here.

mod account;
mod actions;
mod attachments;
mod attested;
mod backgrounds;
mod calls;
mod devices;
mod enroll;
pub mod files;
mod gateway;
mod groups;
mod linking;
mod messaging;
mod notifications;
mod org_backgrounds;
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
pub use attested::{AttestationSettings, JwksSource};
pub use calls::{CallEventSink, CallKeySigner};
pub use enroll::{EnrollmentOptions, EnrollmentSecret, OrgRecoveryUnlock, PendingEnrollment};
pub use gateway::GatewayOutcome;
pub use linking::{LinkApproval, LinkOffer, LinkedSecrets, NewDeviceLink};
pub use messaging::FlushReport;
pub(crate) use messaging::decrypt_content;
#[cfg(feature = "ui")]
pub(crate) use messaging::{apply_control, bookmarks_key};
pub use notifications::TYPING_TIMEOUT_MS;
use openmls_rust_crypto::OpenMlsRustCrypto;
pub use org_recovery::{SealedShare, recovery_request};
#[cfg(not(target_arch = "wasm32"))]
pub use segments::SegmentPrefetch;
use teamcraft_crypto::membership::ChangeId;
use teamcraft_crypto::mls::{ChangeHistory, ConversationGroup};
use teamcraft_proto::{Base64Bytes, ConversationId, ConversationResponse, UserId};

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
  /// Where the app stages plaintext copies of files to upload (a phone copies picked documents
  /// out of other apps first); a copy there is deleted once its upload finished or failed for
  /// good. Files elsewhere (a desktop user's own documents) are never deleted.
  pub maybe_upload_staging_dir: Option<std::path::PathBuf>,
  pub link_previews: previews::LinkPreviewSettings,
  /// Checks before giving up on adding a new device that never uploads key packages.
  pub device_add_attempts: u32,
  /// Ranks beyond this commit together at `device_add_max_rank × device_add_stagger`: bounds how
  /// long a new device waits when the first-ranked members are away (presence isn't per device).
  pub device_add_max_rank: u32,
  /// How old the newest signed roster may get before admins lose their authority on this client
  /// (a server holding back a demotion gets at most this long, security review A6). Owners'
  /// clients re-sign the roster after half of it.
  pub workspace_max_age: std::time::Duration,
  /// The identity provider that attests identity keys; `None`: trust on first use.
  pub maybe_attestation: Option<AttestationSettings>,
  /// Show messages from before senders signed them (their sender is only "some member"). Off by
  /// default: a removed member could otherwise keep writing in anyone's name (security review A10).
  pub accept_unsigned_messages: bool,
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
  maybe_jwks_source: Option<Box<dyn JwksSource>>,
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
      maybe_upload_staging_dir: None,
      link_previews: previews::LinkPreviewSettings::default(),
      device_add_max_rank: 3,
      workspace_max_age: std::time::Duration::from_secs(30 * 24 * 60 * 60),
      maybe_attestation: None,
      accept_unsigned_messages: false,
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
    let maybe_jwks_source = attested::default_jwks_source(settings.maybe_attestation.as_ref());
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
      maybe_jwks_source,
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
      let history = match records::get_json::<Base64Bytes>(
        &self.store,
        Table::AppliedChanges,
        &history_key(conversation),
      )? {
        Some(bytes) => ChangeHistory::from_bytes(bytes.as_slice())?,
        None => ChangeHistory::default(),
      };
      let maybe_group = ConversationGroup::load(
        &self.provider,
        convert::conversation(conversation),
        applied.into_iter().map(ChangeId::from_bytes),
        history,
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
    self
      .store
      .delete(Table::AppliedChanges, &history_key(conversation))?;
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
      records::put_json(
        &self.store,
        Table::AppliedChanges,
        &history_key(conversation),
        &Base64Bytes::new(group.change_history().to_bytes()?),
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
    let users: Vec<UserId> = users.into_iter().collect();
    let mut pinned = records::pinned_identities(&self.store)?;
    // Pins another device of the user made travel in the account data (security review A2).
    let shared = self.account_data();
    for user in users.iter().copied() {
      if let std::collections::btree_map::Entry::Vacant(slot) = pinned.entry(user)
        && let Some(public_key) = shared.pinned_key(user)
      {
        let entry = PinnedIdentity {
          public_key,
          maybe_changed_to: None,
          // Checked (or not) by the device that pinned it.
          is_attested: false,
        };
        records::pin_identity(&self.store, user, &entry)?;
        slot.insert(entry);
      }
    }
    for user in users.iter().copied() {
      if user == self.account.user_id {
        if let std::collections::btree_map::Entry::Vacant(slot) = pinned.entry(user) {
          let own = PinnedIdentity {
            public_key: *self.account.identity_public_key().as_bytes(),
            maybe_changed_to: None,
            is_attested: true,
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
      let entry = match pinned.get(&user) {
        None => {
          // A first pin: checked against the identity provider's attestation (security review A1).
          let check = self
            .check_attestation(user, &public_key, response.maybe_attestation.as_deref())
            .await;
          if check == attested::PinCheck::Refused {
            continue;
          }
          PinnedIdentity {
            public_key,
            maybe_changed_to: None,
            is_attested: check == attested::PinCheck::Attested,
          }
        },
        Some(existing) if existing.public_key == public_key => PinnedIdentity {
          maybe_changed_to: None,
          ..existing.clone()
        },
        Some(existing) => {
          tracing::warn!(user_id = %user, "identity key changed on the server; keeping the pinned key");
          PinnedIdentity {
            maybe_changed_to: Some(public_key),
            ..existing.clone()
          }
        },
      };
      records::pin_identity(&self.store, user, &entry)?;
      pinned.insert(user, entry);
    }
    let directory = PinnedDirectory::new(&pinned, &self.workspace()?.roster_identities());
    let lists = self.stored_device_lists(&directory, &users);
    Ok(directory.with_device_lists(lists))
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

/// Where a group's change history (security review A3) is kept next to its applied change IDs.
fn history_key(conversation: ConversationId) -> String {
  format!("history/{conversation}")
}
