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
mod catch_up;
mod custom_status;
pub mod devices;
mod enroll;
pub mod files;
mod gateway;
mod group_health;
mod groups;
mod guest;
mod heads;
mod key_history;
mod key_packages;
mod linking;
mod member_device;
mod messaging;
mod notifications;
mod org_backgrounds;
mod org_recovery;
mod pending_actions;
mod pending_add;
pub mod people;
pub mod previews;
mod read_ahead;
mod removal;
#[cfg(not(target_arch = "wasm32"))]
mod segments;
mod sources;
mod sync;
mod sync_trace;
#[cfg(feature = "ui")]
mod ui_commands;
pub mod upload_parts;
mod workspace;
pub mod workspace_admin;
pub(crate) use account::ACCOUNT_DATA_KEY;
pub(crate) use workspace::WORKSPACE_KEY;

use std::collections::{BTreeMap, HashMap};

pub use account::stored_account_data;
/// Where the engine records its upload limit for the UI.
pub const MAX_UPLOAD_KEY: &str = "max_upload_bytes";
pub use actions::{OutgoingText, REPLY_EXCERPT_CHARS, forward_content, reply_ref};
pub use attachments::{BlobDownload, MAX_VIEWER_BYTES, OutgoingFile};
pub use attested::{AttestationSettings, JwksSource};
pub use calls::{CallEventSink, CallKeySigner, CallMoveRecord, CallPreparation, call_move_announcement};
pub(crate) use catch_up::CATCHING_UP_PREFIX;
pub use catch_up::{CatchUpFocus, is_catching_up};
pub use enroll::{EnrollmentOptions, EnrollmentSecret, OrgRecoveryUnlock, PendingEnrollment};
pub use gateway::{
  DEFAULT_HANDSHAKE_TIMEOUT, GatewayOutcome, GatewayStart, GatewayTiming, HeldFrames, HeldLimits, KeptAlive,
  keep_alive,
};
pub use group_health::{
  RemovalStatus, all_group_leftovers, group_leftovers, is_queue_refused, pending_removals,
  unconfirmed_removals,
};
pub use groups::{DmSetupStatus, dm_setup_status};
pub use guest::{GuestAdmission, GuestLinkMaterial, guest_link_fragment};
pub use linking::{
  LinkAnnouncement, LinkApproval, LinkOffer, LinkRequestInfo, LinkRequestList, LinkedSecrets, NewDeviceLink,
};
pub use member_device::{GroupAccess, is_waiting_for_device};
pub use messaging::FlushReport;
pub(crate) use messaging::{REMOVED_SENDER_GRACE_MS, decrypt_content};
#[cfg(feature = "ui")]
pub(crate) use messaging::{apply_control, bookmarks_key};
pub(crate) use notifications::registered_push_token;
pub use notifications::{TYPING_TIMEOUT_MS, TypingState};
use openmls_rust_crypto::OpenMlsRustCrypto;
pub use org_recovery::{SealedShare, recovery_request};
pub use pending_actions::{PendingAction, PendingActionSettings, StatusChange};
pub use pending_add::{UserAddWait, pending_user_additions};
#[cfg(not(target_arch = "wasm32"))]
pub use segments::SegmentPrefetch;
pub use sources::HistoryScan;
pub use sync_trace::SyncPhase;
use teamcraft_crypto::membership::ChangeId;
use teamcraft_crypto::mls::{ChangeHistory, ConversationGroup, PendingCommit};
use teamcraft_proto::{Base64Bytes, ConversationId, ConversationResponse, DeviceActivity, UserId};

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
  /// How long the private part of one of this device's key packages stays after the package
  /// expired or, for a last-resort package, after a newer one replaced it on the server: welcomes
  /// for it may still be on the way (e2ee.md §4.3.1). Then it is deleted.
  pub key_package_retention: std::time::Duration,
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
  /// Somebody added to a conversation who has no device for this long is shown as having never set
  /// up the app, and the wait for their first device ends (their first device still gets them in,
  /// should it ever appear).
  pub pending_addition_expiry: std::time::Duration,
  /// Members ignore a device's rejoin request for this long after it was added back (server
  /// times): each answer costs a commit, and a broken or malicious device could ask far more often
  /// than `rejoin_retry` (issue #149).
  pub rejoin_cooldown: std::time::Duration,
  /// Removal records whose absence ended before the workspace's retention limit are dropped
  /// (issue #186); this much later, to spare messages still on the way.
  pub removed_sender_retention_leeway: std::time::Duration,
  /// A DM device that waits for the welcome of a group someone else founded shows "setting up
  /// encryption" this long, then that it failed, with a retry (#278).
  pub dm_welcome_timeout: std::time::Duration,
  /// A removal whose server side the server refused after the MLS side was done (the remover lost
  /// the right meanwhile) is tried again after this long (e2ee.md §4.6, removal order).
  pub removal_retry: std::time::Duration,
  /// The delay before the next try of a refused removal doubles each time, up to this.
  pub removal_retry_max: std::time::Duration,
  /// Refusals before a removal shows as failed and waits for the user's "retry"; also how often
  /// an admin outside the group signs its removal again at most.
  pub removal_attempts: u32,
  /// After the server reports another device of this user revoked, this device waits this long
  /// before it reads the signed device list and, if that names the device revoked, finishes its
  /// removal from the groups (the revoking device normally did by then). Later checks back off
  /// like refused removals, `removal_attempts` in all (#232).
  pub revocation_check_delay: std::time::Duration,
  /// Revocation checks waiting at most; a server reporting more devices revoked can't make this
  /// device fetch its device list without bound (#232).
  pub max_pending_revocation_checks: usize,
  /// How long an admin outside the group keeps checking that its posted removal doesn't get too
  /// old to be committed (it can't see the members' commit).
  pub removal_watch: std::time::Duration,
  /// Larger MLS group information is not sent with a commit (it would push the commit over the
  /// server's payload limit); a further device then waits for a member instead.
  pub max_group_info_bytes: usize,
  /// At most this many content keys are sent to a device added back, the newest first (e2ee.md
  /// §4.6): bounds the size of that one message.
  pub max_missed_keys: usize,
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
  /// How many people's announced presence the core keeps for the UI (the newest announcements, one
  /// record each). The core announces everyone it knows after each connection, so the bound only
  /// bites in larger workspaces; there a UI keeps the older presence of people dropped before it
  /// read them until their next announcement.
  pub max_announced_presence: usize,
  /// Limits of the queue of read positions, acknowledgements, pins and presence (issue #159).
  pub pending_actions: PendingActionSettings,
  /// Backoff and cap of file uploads that failed on the network (issue #316).
  pub upload_retry: files::UploadRetrySettings,
  /// How many conversations a catch-up reads from the server at once (issue #247): member lists
  /// and the first page of each queue and history. The MLS work itself stays one at a time.
  pub catch_up_concurrency: usize,
  /// Make the user's notes to self (a DM with themself) once a sync finds there is none. Off by
  /// default; the apps turn it on with [`ClientSettings::with_notes_to_self`].
  pub creates_notes_to_self: bool,
}

impl ClientSettings {
  /// The settings of an app: the defaults, and every user gets notes to self.
  pub fn with_notes_to_self(mut self) -> Self {
    self.creates_notes_to_self = true;
    self
  }
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
  /// Refusals of earlier user actions found by a background flush, for the host to show
  /// ([`Client::take_background_failures`]).
  background_failures: Vec<CoreError>,
  /// File transfers in flight, with their progress; the UI adapter reads a clone.
  transfers: crate::transfers::Transfers,
  /// The progress of a large file between two of its parts, so it stays listed.
  held_upload_progress: Option<crate::transfers::TransferGuard>,
  /// The browser's picked files may include some no upload needs any more (at start, after an
  /// upload finished).
  is_picked_sweep_due: bool,
  maybe_call_sink: Option<CallEventSink>,
  /// New devices of the user that ask to be linked ([`Client::link_requests`]); in memory only,
  /// the server lists the open ones again on every gateway connection.
  link_requests: linking::LinkRequestList,
  /// The call this device last prepared to join per conversation: a call move is followed only
  /// when announced for that very call (docs/meetings.md, "A call becomes a meeting").
  call_ids: HashMap<ConversationId, teamcraft_proto::CallId>,
  maybe_preview_fetcher: Option<Box<dyn crate::link_preview::PreviewFetcher>>,
  maybe_jwks_source: Option<Box<dyn JwksSource>>,
  /// When the key packages were last checked ([`Client::renew_key_packages_if_due`]).
  last_key_package_check_ms: i64,
  /// When this device last tried to join each conversation's group as a further device (backs
  /// off while waiting for a member).
  member_join_attempts: HashMap<ConversationId, i64>,
  /// When this device last handled a further device's request to be added, per conversation and
  /// device: repeats within `rejoin_retry` cost no requests (issue #187).
  device_add_requests_handled: HashMap<(ConversationId, teamcraft_proto::DeviceId), i64>,
  /// Whether all welcomes were read since the last reset: only then may spent key packages go, so a
  /// welcome that waited for one is opened first (e2ee.md §4.3.1). Reset when a gateway connection
  /// starts (fresh or resumed), when a full sync starts and when a welcome sync starts; set when a
  /// welcome sync read the last page.
  are_welcomes_synced: bool,
  /// When people waiting for a first device were last checked after a full sync
  /// ([`Client::recheck_waiting_additions`]).
  maybe_waiting_checked_at_ms: Option<i64>,
  /// Conversations whose MLS queue was caught up since the gateway (re)connected; the outbox
  /// sends into the others only after catching up (e2ee.md §4.6).
  mls_current: std::collections::BTreeSet<ConversationId>,
  /// Events of a resume replay still to come: the counters are read once it has drained, because
  /// `GET /unread` already counts the missed messages the replay delivers (issue #183).
  maybe_replay_left: Option<u64>,
  /// What the device last said about its activity; a new gateway session identifies with it.
  device_activity: DeviceActivity,
  /// The conversation the user looks at; a catch-up takes it first (issue #247).
  catch_up_focus: CatchUpFocus,
  /// Set while a catch-up joins the focused conversation's welcomes ahead of the others: the
  /// welcome cursor then stays where it is, so the welcomes before them are still read in order.
  /// Cleared right after each such join; a catch-up future dropped in between leaves it set until
  /// the next catch-up, which clears it before anything else (only welcomes are affected).
  is_welcome_cursor_held: bool,
  /// Where the time of the last gateway connection went (issue #714).
  sync_trace: sync_trace::SyncTrace,
  /// The heads of the full sync in progress, to skip what didn't change (issue #714).
  maybe_heads: Option<heads::HeadsInSync>,
  /// The first reads of a gateway connection, read at once and waiting for their steps (#714).
  read_ahead: read_ahead::ReadAhead,
}

impl Default for ClientSettings {
  fn default() -> Self {
    Self {
      key_package_target: 30,
      key_package_low_water: 10,
      key_package_check_interval: std::time::Duration::from_secs(6 * 60 * 60),
      key_package_retention: std::time::Duration::from_secs(30 * 24 * 60 * 60),
      history_page_size: 50,
      max_commit_attempts: 3,
      device_add_stagger: std::time::Duration::from_secs(10),
      device_add_attempts: 6,
      rejoin_retry: std::time::Duration::from_secs(10 * 60),
      rejoin_retry_max: std::time::Duration::from_secs(24 * 60 * 60),
      pending_addition_expiry: std::time::Duration::from_secs(90 * 24 * 60 * 60),
      rejoin_cooldown: std::time::Duration::from_secs(30 * 60),
      removed_sender_retention_leeway: std::time::Duration::from_secs(24 * 60 * 60),
      dm_welcome_timeout: std::time::Duration::from_secs(2 * 60),
      removal_retry: std::time::Duration::from_secs(60 * 60),
      removal_retry_max: std::time::Duration::from_secs(24 * 60 * 60),
      removal_attempts: 6,
      revocation_check_delay: std::time::Duration::from_secs(5 * 60),
      max_pending_revocation_checks: 16,
      removal_watch: std::time::Duration::from_secs(30 * 24 * 60 * 60),
      max_group_info_bytes: 256 * 1024,
      max_missed_keys: 512,
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
      max_announced_presence: 5_000,
      pending_actions: PendingActionSettings::default(),
      upload_retry: files::UploadRetrySettings::default(),
      catch_up_concurrency: 6,
      creates_notes_to_self: false,
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
      background_failures: Vec::new(),
      transfers: crate::transfers::Transfers::new(),
      held_upload_progress: None,
      is_picked_sweep_due: true,
      maybe_call_sink: None,
      link_requests: Default::default(),
      call_ids: HashMap::new(),
      maybe_preview_fetcher,
      maybe_jwks_source,
      last_key_package_check_ms: i64::MIN,
      mls_current: Default::default(),
      maybe_replay_left: None,
      device_activity: DeviceActivity::Active,
      member_join_attempts: HashMap::new(),
      device_add_requests_handled: HashMap::new(),
      are_welcomes_synced: false,
      maybe_waiting_checked_at_ms: None,
      catch_up_focus: CatchUpFocus::default(),
      is_welcome_cursor_held: false,
      sync_trace: Default::default(),
      maybe_heads: None,
      read_ahead: Default::default(),
    };
    // A catch-up the app was closed during left its marks; the next one sets them again.
    catch_up::clear_marks(&client.store)?;
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

  /// The file transfers in flight; a clone stays current (the UI adapter reads their progress).
  pub fn transfers(&self) -> crate::transfers::Transfers {
    self.transfers.clone()
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

  /// Whether this device holds the content key a message names (`content_key_id`). For tests.
  #[doc(hidden)]
  pub fn has_content_key(&self, conversation: ConversationId, content_key_id: &[u8]) -> bool {
    let Ok(id) = <[u8; 16]>::try_from(content_key_id) else {
      return false;
    };
    self
      .account
      .key_bag
      .ring(convert::conversation(conversation))
      .is_some_and(|ring| {
        ring
          .get(teamcraft_crypto::content_key::ContentKeyId::from_bytes(id))
          .is_some()
      })
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
    self.atomically(|client| client.delete_group_records(conversation))
  }

  fn delete_group_records(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
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
      .delete(Table::AppliedChanges, &key_history::key_history_key(conversation))?;
    self.store.delete(
      Table::AppliedChanges,
      &member_device::current_key_wait_key(conversation),
    )?;
    self
      .store
      .delete(Table::AppliedChanges, &group_health::leftovers_key(conversation))?;
    self.store.delete(
      Table::AppliedChanges,
      &group_health::queue_refused_key(conversation),
    )?;
    self
      .store
      .delete(Table::SyncState, &groups::group_info_published_key(conversation))?;
    self.mls_current.remove(&conversation);
    records::save_mls_storage(&self.store, &self.provider)
  }

  /// Writes MLS state (provider storage and applied change IDs) after a group operation, as one
  /// group of writes.
  fn persist_group(&self, conversation: ConversationId) -> Result<(), CoreError> {
    crate::store::write_group(&self.store, || self.write_group_state(conversation))
  }

  /// [`Self::persist_group`] after an MLS entry was processed: the state and the cursor past the
  /// entry go to the store together, so a restart never processes it again on a state that
  /// already moved on.
  pub(crate) fn persist_group_at(
    &mut self,
    conversation: ConversationId,
    message: &teamcraft_proto::MlsMessageResponse,
  ) -> Result<(), CoreError> {
    self.atomically(|client| {
      client.write_group_state(conversation)?;
      client.advance_cursor(conversation, message)
    })
  }

  fn write_group_state(&self, conversation: ConversationId) -> Result<(), CoreError> {
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

  /// Writes the key bag after content keys changed, and marks it for upload (together, so a bag
  /// saved with new keys is never left unmarked).
  pub(crate) fn persist_keys(&mut self) -> Result<(), CoreError> {
    self.sync.is_key_bag_dirty = true;
    crate::store::write_group(&self.store, || {
      records::save_key_bag(&self.store, &self.account)?;
      records::save_sync_state(&self.store, &self.sync)
    })
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
      // Not enrolled yet: they can't be in any group, so there's nothing to pin.
      let Some(response) = self.api.identity_key(user).await? else {
        continue;
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
    let directory = PinnedDirectory::new(&pinned, &self.workspace()?.roster_identities())
      .with_guests(self.guest_users(users.iter().copied())?);
    let lists = self.stored_device_lists(&directory, &users);
    Ok(directory.with_device_lists(lists))
  }

  /// The users among `users` this device treats as meeting guests: those the server's user kind
  /// says are guests or this device itself let in as guests, unless the signed roster vouches for
  /// them or they organize a meeting (a hostile server can't make a host a guest, e2ee.md §4.6).
  /// Guests manage nobody's membership but their own.
  pub(crate) fn guest_users(
    &self,
    users: impl IntoIterator<Item = UserId>,
  ) -> Result<Vec<UserId>, CoreError> {
    let admitted = self.admitted_guests()?;
    let organizers: Vec<UserId> = self
      .cached_conversations()?
      .values()
      .filter_map(|conversation| conversation.maybe_organizer_user_id)
      .collect();
    let workspace = self.workspace()?;
    let mut guests = Vec::new();
    for user in users {
      let maybe_summary =
        records::get_json::<teamcraft_proto::UserSummary>(&self.store, Table::Users, &user.to_string())?;
      let is_listed_as_guest =
        maybe_summary.is_some_and(|summary| summary.kind == teamcraft_proto::UserKind::Guest);
      let is_vouched = workspace
        .roster
        .members
        .iter()
        .any(|member| member.user_id == user)
        || organizers.contains(&user)
        || user == self.account.user_id && !self.is_guest_device()?;
      if (is_listed_as_guest || admitted.contains(&user)) && !is_vouched {
        guests.push(user);
      }
    }
    Ok(guests)
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

  /// Runs `writes` as one group of store writes (issue #160): after a crash or a failed write, the
  /// store holds all of them or none. For steps whose records must agree (MLS state, cursor, keys);
  /// synchronous, so a group never stays open across a network request.
  pub(crate) fn atomically<T>(
    &mut self,
    writes: impl FnOnce(&mut Self) -> Result<T, CoreError>,
  ) -> Result<T, CoreError> {
    self.store.begin_writes()?;
    // A panic must not leave the group open: nothing would be committed again.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| writes(self)));
    let ended = self.store.end_writes();
    match result {
      Ok(result) => {
        let value = result?;
        ended?;
        Ok(value)
      },
      Err(panic) => std::panic::resume_unwind(panic),
    }
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
