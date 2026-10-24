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
pub mod devices;
mod enroll;
pub mod files;
mod gateway;
mod group_health;
mod groups;
mod linking;
mod member_device;
mod messaging;
mod notifications;
mod org_backgrounds;
mod org_recovery;
mod pending_add;
pub mod people;
pub mod previews;
mod removal;
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
pub use attachments::{MAX_VIEWER_BYTES, OutgoingFile};
pub use attested::{AttestationSettings, JwksSource};
pub use calls::{CallEventSink, CallKeySigner};
pub use enroll::{EnrollmentOptions, EnrollmentSecret, OrgRecoveryUnlock, PendingEnrollment};
pub use gateway::{DEFAULT_HANDSHAKE_TIMEOUT, GatewayOutcome, GatewayStart};
pub use group_health::{
  RemovalStatus, group_leftovers, is_queue_refused, pending_removals, unconfirmed_removals,
};
pub use linking::{LinkApproval, LinkOffer, LinkedSecrets, NewDeviceLink};
pub use member_device::{GroupAccess, is_waiting_for_device};
pub use messaging::FlushReport;
pub(crate) use messaging::{REMOVED_SENDER_GRACE_MS, decrypt_content};
#[cfg(feature = "ui")]
pub(crate) use messaging::{apply_control, bookmarks_key};
pub use notifications::{TYPING_TIMEOUT_MS, TypingState};
use openmls_rust_crypto::OpenMlsRustCrypto;
pub use org_recovery::{SealedShare, recovery_request};
pub use pending_add::{UserAddWait, pending_user_additions};
#[cfg(not(target_arch = "wasm32"))]
pub use segments::SegmentPrefetch;
use teamcraft_crypto::membership::ChangeId;
use teamcraft_crypto::mls::{ChangeHistory, ConversationGroup, PendingCommit};
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
  /// Top up when fewer than this are left (packages expiring soon don't count).
  pub key_package_low_water: u32,
  /// How often a running client asks the server whether its key packages need renewing.
  pub key_package_check_interval: std::time::Duration,
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
  /// The push gateway this app was built for (a publisher's store app): device tokens are sealed for
  /// it. `None`: use the gateway the server names (self-built apps).
  pub maybe_push_gateway_url: Option<String>,
  /// A sealed device token is sealed again after this long (it picks up a rotated gateway key and
  /// the old one is revoked), well before it expires.
  pub push_reseal_after: std::time::Duration,
  /// Where the app stages plaintext copies of files to upload (a phone copies picked documents
  /// out of other apps first); a copy there is deleted once its upload finished or failed for
  /// good. Files elsewhere (a desktop user's own documents) are never deleted.
  pub maybe_upload_staging_dir: Option<std::path::PathBuf>,
  pub link_previews: previews::LinkPreviewSettings,
  /// Checks before giving up on adding a new device that never uploads key packages (also for
  /// adding back a device that fell out of a group).
  pub device_add_attempts: u32,
  /// A device that fell out of a group asks again to be added back after this long without a
  /// welcome (e2ee.md §4.6).
  pub rejoin_retry: std::time::Duration,
  /// A device without a group (a welcome it couldn't join) waits twice as long after each rejoin
  /// request, up to this.
  pub rejoin_retry_max: std::time::Duration,
  /// A removal whose server side the server refused after the MLS side was done (the remover lost
  /// the right meanwhile) is tried again after this long (e2ee.md §4.6, removal order).
  pub removal_retry: std::time::Duration,
  /// The delay before the next try of a refused removal doubles each time, up to this.
  pub removal_retry_max: std::time::Duration,
  /// Refusals before a removal shows as failed and waits for the user's "retry"; also how often
  /// an admin outside the group signs its removal again at most.
  pub removal_attempts: u32,
  /// How long an admin outside the group keeps checking that its posted removal doesn't get too
  /// old to be committed (it can't see the members' commit).
  pub removal_watch: std::time::Duration,
  /// Larger MLS group information is not sent with a commit (it would push the commit over the
  /// server's payload limit); a further device then waits for a member instead.
  pub max_group_info_bytes: usize,
  /// Ranks beyond this commit together at `device_add_max_rank × device_add_stagger`: bounds how
  /// long a new device waits when the first-ranked members are away (presence isn't per device).
  pub device_add_max_rank: u32,
  /// How old the newest signed roster may get before admins lose their authority on this client
  /// (a server holding back a demotion gets at most this long, security review A6). Owners'
  /// clients re-sign the roster after half of it.
  pub workspace_max_age: std::time::Duration,
  /// How long before `workspace_max_age` runs out the admin console warns that no owner signed.
  pub workspace_expiry_warning: std::time::Duration,
  /// The identity provider that attests identity keys; `None`: trust on first use.
  pub maybe_attestation: Option<AttestationSettings>,
  /// Show messages from before senders signed them (their sender is only "some member"). Off by
  /// default: a removed member could otherwise keep writing in anyone's name (security review A10).
  pub accept_unsigned_messages: bool,
  /// How many people's announced presence the core keeps for the UI (the newest announcements);
  /// a UI that fell further behind reads presence from the server again.
  pub max_announced_presence: usize,
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
  /// People who went offline whose last seen is still to be read (batched, see
  /// [`Client::refresh_stale_last_seen`]).
  stale_last_seen: std::collections::BTreeSet<UserId>,
  maybe_call_sink: Option<CallEventSink>,
  maybe_preview_fetcher: Option<Box<dyn crate::link_preview::PreviewFetcher>>,
  maybe_jwks_source: Option<Box<dyn JwksSource>>,
  /// When the key packages were last checked ([`Client::renew_key_packages_if_due`]).
  last_key_package_check_ms: i64,
  /// When this device last tried to join each conversation's group as a further device (backs
  /// off while waiting for a member).
  member_join_attempts: HashMap<ConversationId, i64>,
  /// When people waiting for a first device were last checked after a full sync
  /// ([`Client::recheck_waiting_additions`]).
  maybe_waiting_checked_at_ms: Option<i64>,
  /// Conversations whose MLS queue was caught up since the gateway (re)connected; the outbox
  /// sends into the others only after catching up (e2ee.md §4.6).
  mls_current: std::collections::BTreeSet<ConversationId>,
  /// Events of a resume replay still to come: the counters are read once it has drained, because
  /// `GET /unread` already counts the missed messages the replay delivers (issue #183).
  maybe_replay_left: Option<u64>,
}

impl Default for ClientSettings {
  fn default() -> Self {
    Self {
      key_package_target: 30,
      key_package_low_water: 10,
      key_package_check_interval: std::time::Duration::from_secs(6 * 60 * 60),
      history_page_size: 50,
      max_commit_attempts: 3,
      device_add_stagger: std::time::Duration::from_secs(10),
      device_add_attempts: 6,
      rejoin_retry: std::time::Duration::from_secs(10 * 60),
      rejoin_retry_max: std::time::Duration::from_secs(24 * 60 * 60),
      removal_retry: std::time::Duration::from_secs(60 * 60),
      removal_retry_max: std::time::Duration::from_secs(24 * 60 * 60),
      removal_attempts: 6,
      removal_watch: std::time::Duration::from_secs(30 * 24 * 60 * 60),
      max_group_info_bytes: 256 * 1024,
      max_upload_bytes: 100 * 1024 * 1024,
      maybe_download_dir: None,
      maybe_push_gateway_url: None,
      push_reseal_after: std::time::Duration::from_secs(7 * 24 * 60 * 60),
      maybe_upload_staging_dir: None,
      link_previews: previews::LinkPreviewSettings::default(),
      device_add_max_rank: 3,
      workspace_max_age: std::time::Duration::from_secs(30 * 24 * 60 * 60),
      workspace_expiry_warning: std::time::Duration::from_secs(7 * 24 * 60 * 60),
      maybe_attestation: None,
      accept_unsigned_messages: false,
      max_announced_presence: 500,
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
    let mut client = Self {
      api,
      store,
      clock,
      settings,
      provider,
      account,
      groups: HashMap::new(),
      sync,
      typing: Default::default(),
      stale_last_seen: Default::default(),
      maybe_call_sink: None,
      maybe_preview_fetcher,
      maybe_jwks_source,
      last_key_package_check_ms: i64::MIN,
      mls_current: Default::default(),
      maybe_replay_left: None,
      member_join_attempts: HashMap::new(),
      maybe_waiting_checked_at_ms: None,
    };
    // Startup: removal records of the user's devices count from the start, and this device's own
    // (also from older versions) are shared.
    client.apply_shared_removals()?;
    client.share_removed_senders()?;
    Ok(Some(client))
  }

  /// A sealed device token is sealed again after this long (tests set it to zero to force it).
  pub fn set_push_reseal_after(&mut self, after: std::time::Duration) {
    self.settings.push_reseal_after = after;
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

  /// The epoch of the conversation's MLS group on this device; `None` without a group.
  pub fn mls_epoch(&mut self, conversation: ConversationId) -> Result<Option<u64>, CoreError> {
    Ok(self.group(conversation)?.map(|group| group.epoch()))
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
      let maybe_pending = records::get_json::<Base64Bytes>(
        &self.store,
        Table::AppliedChanges,
        &pending_commit_key(conversation),
      )?
      .map(|bytes| PendingCommit::from_bytes(bytes.as_slice()))
      .transpose()?;
      let maybe_group = ConversationGroup::load(
        &self.provider,
        convert::conversation(conversation),
        applied.into_iter().map(ChangeId::from_bytes),
        history,
        maybe_pending,
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
    self.forget_group_records(conversation)
  }

  /// The part of [`Self::drop_group`] outside the MLS state: what this device noted about the
  /// group next to it. Alone for a group whose MLS state a joined group took over.
  fn forget_group_records(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    self.groups.remove(&conversation);
    self
      .store
      .delete(Table::AppliedChanges, &conversation.to_string())?;
    self
      .store
      .delete(Table::AppliedChanges, &history_key(conversation))?;
    self
      .store
      .delete(Table::AppliedChanges, &pending_commit_key(conversation))?;
    self
      .store
      .delete(Table::AppliedChanges, &pending_post_key(conversation))?;
    self
      .store
      .delete(Table::AppliedChanges, &groups::rejoin_key(conversation))?;
    self
      .store
      .delete(Table::AppliedChanges, &group_health::leftovers_key(conversation))?;
    self.store.delete(
      Table::AppliedChanges,
      &group_health::queue_refused_key(conversation),
    )?;
    self.mls_current.remove(&conversation);
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
      match group.pending_commit() {
        Some(pending) => records::put_json(
          &self.store,
          Table::AppliedChanges,
          &pending_commit_key(conversation),
          &Base64Bytes::new(pending.to_bytes()?),
        )?,
        None => self
          .store
          .delete(Table::AppliedChanges, &pending_commit_key(conversation))?,
      }
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

/// Where this device's commit that waits for the server's answer is kept (e2ee.md §4.6).
fn pending_commit_key(conversation: ConversationId) -> String {
  format!("pending_commit/{conversation}")
}

/// The request that posted the pending commit, to send it again if the server never got it.
pub(crate) fn pending_post_key(conversation: ConversationId) -> String {
  format!("pending_post/{conversation}")
}
