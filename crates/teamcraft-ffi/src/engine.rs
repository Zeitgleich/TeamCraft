//! The engine thread: owns the store, the client and the sync loop, and runs the jobs the
//! [`crate::TeamCraft`] handle posts, one at a time, on a current-thread runtime.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use teamcraft_client_api::backend::{ChatBackend, SearchBackend};
use teamcraft_client_api::entry_rules::{channel_leave, space_leave};
use teamcraft_client_api::model::{
  Conversation as ConversationView, ConversationKind as ConversationKindView, Message as MessageView,
  SpaceRole, SpaceRoster, UserId, is_pending,
};
use teamcraft_client_api::notify::{MentionRules, is_mentioned};
use teamcraft_client_api::search::parse_query;
use teamcraft_core::api::{AccessTokenSource, ApiError, HttpRendezvous, HttpServerApi, ServerApi};
use teamcraft_core::client::NewDeviceLink;
use teamcraft_core::client::files::sweep_staged_copies;
use teamcraft_core::client::{
  CatchUpFocus, LinkAnnouncement, LinkApproval, LinkRequestList, ReceiptCancel, SIGN_OUT_REVOKE_PATIENCE,
};
use teamcraft_core::clock::{Clock as _, SystemClock};
use teamcraft_core::connection::{ConnectionStatus, NetworkChanges};
use teamcraft_core::notified;
use teamcraft_core::notify_rules::{load_mention_preferences, save_mention_preferences};
use teamcraft_core::pending_work::PendingWork;
use teamcraft_core::prefetch::{load_segment_prefetch, save_segment_prefetch};
use teamcraft_core::push_guard;
use teamcraft_core::push_preview::{
  PreviewGap, PushPreview, device_push_content, push_content_mode, push_preview, save_device_push_content,
};
use teamcraft_core::runner::{RunnerCommand, RunnerHandle, RunnerSettings, SyncRunner};
use teamcraft_core::store::{
  LocalStore as _, SealedSqliteStore, SealedStoreLimits, StaticKeySource, TrackedStore,
};
use teamcraft_core::ui_backend::{CoreChatBackend, CoreCommand, FeedAnchor};
use teamcraft_core::{
  Client, ClientSettings, CoreError, EnrollmentOptions, EnrollmentSecret, LinkCodeKind, LinkOffer,
  PendingEnrollment,
};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_crypto::secret::{SecretBytes, SymmetricKey};
use teamcraft_proto::{CallJoinTokenResponse, ConversationId, DeviceActivity, Id, ThreadId};
use tokio::sync::{mpsc, oneshot};
use tokio::task::{JoinHandle, LocalSet};

use crate::calls::{CallHub, CallPreparation};
use crate::error::TeamCraftError;
use crate::types::{
  AccountState, AppConfig, ConversationItem, DirectPeer, KeyCatchUpItem, LeaveChoice, LinkPlatform,
  LinkRequestItem, MentionSettings, MessageItem, MessagePage, NotificationContent,
  NotificationContentSettings, NotifiedConversation, Presence, PushDiagnosticsInfo, PushNotification,
  PushService, SearchHitItem, SegmentPrefetchMode, SegmentPrefetchResult, SpaceMenuItem, StoreProblem,
  TestPushResult, share_targets,
};
use crate::{EventListener, TokenProvider};

/// Decrypted downloads, one folder per account below it.
/// Wrong short codes allowed before a link approval is dropped.
const MAX_APPROVAL_MISSES: u8 = 3;
const DOWNLOADS_DIR: &str = "downloads";
/// The longest a wake-up is worked on before its notification is shown with what is known by then.
/// FCM gives a high-priority message about 10 s; the app hands over to expedited work at 8 s.
const PUSH_PREVIEW_BUDGET: Duration = Duration::from_secs(7);
/// The part of [`PUSH_PREVIEW_BUDGET`] for fetching and decrypting the message alone; the rest is
/// for catching the conversation up if that fails.
const PUSH_FETCH_BUDGET: Duration = Duration::from_secs(4);
/// What must be left of [`PUSH_PREVIEW_BUDGET`] to start following the groups, which can't be cut
/// short once begun.
const PUSH_FOLLOW_MIN_LEFT: Duration = Duration::from_secs(2);
/// The decrypted downloads an account keeps: the disk cache the apps reopen pictures and files from
/// without downloading them again. Above it the least recently used go at start (a deleted file is
/// fetched again when opened: its transfer shows as remote).
const MAX_DOWNLOADS_BYTES: u64 = 1024 * 1024 * 1024;
/// Plaintext copies of files waiting to be uploaded, one folder per account below it.
const UPLOAD_STAGING_DIR: &str = "outgoing";
/// Decoded messages of the conversations read last, kept so the open conversation isn't decrypted
/// again on every change (issue #155; as many as the desktop keeps). A
/// memory-constrained process (the iOS notification extension) keeps none.
const MESSAGE_CACHE_MESSAGES: usize = 60_000;

/// A boxed future that stays on the engine thread.
pub(crate) type LocalFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;
/// Work for the engine thread.
pub(crate) type Job = Box<dyn for<'a> FnOnce(&'a mut Engine) -> LocalFuture<'a, ()> + Send>;

pub(crate) type Api = HttpServerApi<ForeignTokens>;
pub(crate) type Store = TrackedStore<Arc<SealedSqliteStore>>;
pub(crate) type CoreClient = Client<Api, Store>;

pub(crate) struct Engine {
  pub(crate) server_url: String,
  /// Where downloaded files are decrypted to (inside the app's private storage).
  download_dir: PathBuf,
  maybe_push_gateway_url: Option<String>,
  /// Where the app copies files to upload; the core deletes each copy once it went up.
  upload_staging_dir: PathBuf,
  tokens: ForeignTokens,
  listener: Arc<dyn EventListener>,
  pub(crate) store: Store,
  pub(crate) client: ClientSlot,
  maybe_backend: Option<CoreChatBackend<Store>>,
  /// The enrolled identity's fingerprint (empty before enrollment).
  pub(crate) identity_fingerprint: String,
  pub(crate) maybe_recovery: Option<crate::bridge::PendingRecovery>,
  maybe_link_offer: Option<Rc<LinkOffer>>,
  /// What the new device tells the user's other devices about itself while it waits.
  maybe_link_announcement: Option<LinkAnnouncement>,
  /// The open link requests of other devices, readable while the sync loop owns the client.
  link_requests: LinkRequestList,
  /// The new device's wait in the relay, which runs beside the jobs (issue #999).
  new_link: NewLinkWait,
  maybe_approval: Option<PendingApproval>,
  /// Stops the wait for the receipt of the keys this device sent, which runs beside the jobs
  /// (issue #415).
  maybe_receipt: Option<ReceiptCancel>,
  calls: CallHub,
  /// Applied to decrypted push texts (keywords, first name).
  mentions: MentionSettings,
  pub(crate) signals: SyncSignals,
  /// Whether the app asked for syncing (foreground) and hasn't stopped it since.
  is_syncing_for_app: bool,
  /// Background drains under way (issue #158). The sync loop runs while the app syncs or any drain
  /// waits; the app stopping hands it over to them, the last one ends it.
  active_drains: usize,
  /// The engine thread's own queue, for a drain to end itself without the caller (who may be
  /// cancelled by the OS meanwhile). Weak, so the thread still ends once the app drops its handle.
  maybe_jobs: Option<mpsc::WeakUnboundedSender<Job>>,
}

/// What the app's handle shares with every sync loop the engine starts.
#[derive(Clone)]
pub(crate) struct SyncSignals {
  pub(crate) network: NetworkChanges,
  pub(crate) connection: ConnectionStatus,
  /// The conversation the user looks at: a catch-up reads it ahead of the others (issue #247). Set
  /// by every read of a conversation's messages, as on the desktop; until it was, a conversation
  /// opened from a notification after a long absence waited for every other one (issue #972).
  pub(crate) catch_up_focus: CatchUpFocus,
}

/// The new device's wait in the relay ([`Engine::wait_for_link_peer`]): a task on the engine thread
/// beside the jobs, and the link it leaves once the other device answered. Dropping it ends the wait.
#[derive(Default)]
struct NewLinkWait {
  maybe_task: Option<JoinHandle<()>>,
  link: Rc<RefCell<Option<NewDeviceLink<HttpRendezvous>>>>,
}

/// What opening the engine needs, checked once. The handle keeps it while the local store is
/// unreadable, to open the engine again after a reset (issue #414); the key zeroizes on drop.
pub(crate) struct EngineStart {
  server_url: String,
  maybe_push_gateway_url: Option<String>,
  /// The app's private files directory.
  data_dir: PathBuf,
  account: String,
  key: SymmetricKey,
  tokens: Arc<dyn TokenProvider>,
  listener: Arc<dyn EventListener>,
  signals: SyncSignals,
  /// The process has little memory (the iOS notification extension): small store caches.
  is_memory_constrained: bool,
}

/// Why the engine didn't open.
#[derive(Debug)]
pub(crate) enum OpenError {
  /// The local store can't be read by this version, now or after a restart (issue #414): the user
  /// decides what happens to it.
  Unreadable(StoreProblem),
  Failed(TeamCraftError),
}

/// Where the client is: not enrolled yet, held by the engine, or running in the sync loop.
pub(crate) enum ClientSlot {
  Missing,
  Idle(Box<CoreClient>),
  Running {
    handle: RunnerHandle,
    task: JoinHandle<CoreClient>,
  },
}

/// An approval on this device waiting for the user's confirmation; the sync loop pauses meanwhile.
struct PendingApproval {
  approval: LinkApproval<HttpRendezvous>,
  was_running: bool,
  /// Wrong codes typed so far; the attempt ends after [`MAX_APPROVAL_MISSES`].
  misses: u8,
}

/// Whether a push payload is opened as it arrives or again later to update its notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PushOpening {
  /// The push service delivered it now.
  Received,
  /// The app opens it again because the first try could not read the message in time.
  Retry,
}

/// Who opens a push: the app, whose client may catch the conversation up, or a notification
/// extension next to the app (iOS), which must not advance the account's state: two processes
/// would each advance their own copy of the MLS state (e2ee.md §7.2, issue #424 review H2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PushReader {
  App,
  Extension,
}

impl PushReader {
  /// Whether the client catches the push's conversation up (welcomes, MLS backlog, messages)
  /// when the preview couldn't read the message: only the app's own, and only while it isn't
  /// syncing (the sync loop brings the message itself).
  fn catches_up(self, needs_catch_up: bool, is_client_idle: bool) -> bool {
    self == Self::App && needs_catch_up && is_client_idle
  }
}

/// The app's [`TokenProvider`] as the core's token source.
#[derive(Clone)]
pub(crate) struct ForeignTokens(Arc<dyn TokenProvider>);

/// [`CoreClient::catch_up_for_push`] within `deadline`, cutting only the cancel-safe steps short.
/// Following the groups (welcomes, MLS) is never dropped halfway: it starts only with
/// [`PUSH_FOLLOW_MIN_LEFT`] to spare and then runs to completion, bounded by its requests' own
/// timeouts; a wake-up that outlasts the receiver's budget continues as expedited work.
async fn catch_up_for_push(
  client: &mut CoreClient,
  conversation: ConversationId,
  deadline: tokio::time::Instant,
) -> Result<(), TeamCraftError> {
  let timed_out = || TeamCraftError::Core("push catch-up timed out".into());
  tokio::time::timeout_at(deadline, client.push_refresh_conversation(conversation))
    .await
    .map_err(|_| timed_out())??;
  if deadline.saturating_duration_since(tokio::time::Instant::now()) < PUSH_FOLLOW_MIN_LEFT {
    return Err(timed_out());
  }
  client.push_follow_groups(conversation).await?;
  tokio::time::timeout_at(deadline, client.push_catch_up_messages(conversation))
    .await
    .map_err(|_| timed_out())??;
  Ok(())
}

/// Starts the engine thread for `start` and waits until the store is open.
pub(crate) fn spawn(start: &EngineStart) -> Result<mpsc::UnboundedSender<Job>, OpenError> {
  let start = start.duplicate();
  let (ready, opened) = std::sync::mpsc::channel();
  let (jobs, mut queue) = mpsc::unbounded_channel::<Job>();
  let own_jobs = jobs.downgrade();
  std::thread::Builder::new()
    .name("teamcraft-engine".into())
    .spawn(move || {
      let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
          // The constructor waits for this; if it has gone, nobody needs the answer.
          let _ = ready.send(Err(OpenError::Failed(TeamCraftError::Core(error.to_string()))));
          return;
        },
      };
      LocalSet::new().block_on(&runtime, async move {
        // A panic while opening would otherwise only show as `EngineStopped`; Rust's panic output
        // goes nowhere on Android, so the error carries the message and location instead.
        let mut engine = match open_reporting_panics(start) {
          Ok(engine) => engine,
          Err(error) => {
            let _ = ready.send(Err(error));
            return;
          },
        };
        engine.maybe_jobs = Some(own_jobs);
        let _ = ready.send(Ok(()));
        while let Some(job) = queue.recv().await {
          job(&mut engine).await;
        }
        engine.stop_runner().await;
      });
    })
    .map_err(|error| OpenError::Failed(TeamCraftError::Core(error.to_string())))?;
  opened
    .recv()
    .map_err(|_| OpenError::Failed(TeamCraftError::EngineStopped))??;
  Ok(jobs)
}

/// [`Engine::open`], with a panic turned into an error that says where it happened.
fn open_reporting_panics(start: EngineStart) -> Result<Engine, OpenError> {
  let report = Arc::new(Mutex::new(String::new()));
  let previous_hook = std::panic::take_hook();
  let hook_report = report.clone();
  std::panic::set_hook(Box::new(move |info| {
    if let Ok(mut report) = hook_report.lock() {
      *report = info.to_string();
    }
  }));
  let opened = std::panic::catch_unwind(AssertUnwindSafe(|| Engine::open(start)));
  std::panic::set_hook(previous_hook);
  opened.unwrap_or_else(|_| {
    let message = report.lock().map(|report| report.clone()).unwrap_or_default();
    Err(OpenError::Failed(TeamCraftError::Core(format!(
      "opening the engine panicked: {message}"
    ))))
  })
}

/// Sets the unreadable database at `path` aside: it and its log are renamed beside themselves
/// (`….db.unreadable-<now_ms>`), never deleted, so a new one opens there (issue #414). The log goes
/// first: a new database must never find the old one's log, and a failure leaves the database
/// where it was.
pub(crate) fn set_store_aside(path: &Path, now_ms: i64) -> Result<(), TeamCraftError> {
  for extension in ["db-wal", "db-shm", "db"] {
    let file = path.with_extension(extension);
    let aside = path.with_extension(format!("{extension}.unreadable-{now_ms}"));
    match std::fs::rename(&file, &aside) {
      Ok(()) => tracing::warn!(file = extension, "set the unreadable local database aside"),
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
      Err(error) => {
        return Err(TeamCraftError::Storage(format!(
          "could not set the local database aside: {error}"
        )));
      },
    }
  }
  Ok(())
}

/// Deletes plaintext folders (decrypted downloads, staged uploads); a missing one is fine.
pub(crate) fn delete_plaintext_dirs<'a>(
  directories: impl IntoIterator<Item = &'a Path>,
) -> Result<(), TeamCraftError> {
  for directory in directories {
    match std::fs::remove_dir_all(directory) {
      Ok(()) => {},
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
      Err(error) => return Err(TeamCraftError::Storage(error.to_string())),
    }
  }
  Ok(())
}

impl EngineStart {
  pub(crate) fn new(
    config: AppConfig,
    tokens: Arc<dyn TokenProvider>,
    listener: Arc<dyn EventListener>,
    signals: SyncSignals,
  ) -> Result<Self, TeamCraftError> {
    let key = SymmetricKey::from_slice(&config.database_key)
      .ok_or_else(|| TeamCraftError::InvalidInput("the database key must be 32 bytes".into()))?;
    if config.account.is_empty() || config.account.contains(['/', '\\']) || config.account.contains("..") {
      return Err(TeamCraftError::InvalidInput("account name".into()));
    }
    Ok(Self {
      server_url: config.server_url,
      maybe_push_gateway_url: config.push_gateway_url.filter(|url| !url.is_empty()),
      data_dir: PathBuf::from(config.data_dir),
      account: config.account,
      key,
      tokens,
      listener,
      signals,
      is_memory_constrained: config.is_memory_constrained,
    })
  }

  /// The account's sealed database.
  pub(crate) fn store_path(&self) -> PathBuf {
    self.data_dir.join(format!("{}.teamcraft.db", self.account))
  }

  /// Where the account's plaintext files lie: decrypted downloads and staged uploads, each per
  /// account, so another account signed in on this device never sees them.
  pub(crate) fn plaintext_dirs(&self) -> [PathBuf; 2] {
    [self.download_dir(), self.upload_staging_dir()]
  }

  fn download_dir(&self) -> PathBuf {
    self.data_dir.join(DOWNLOADS_DIR).join(&self.account)
  }

  fn upload_staging_dir(&self) -> PathBuf {
    self.data_dir.join(UPLOAD_STAGING_DIR).join(&self.account)
  }

  fn duplicate(&self) -> Self {
    Self {
      server_url: self.server_url.clone(),
      maybe_push_gateway_url: self.maybe_push_gateway_url.clone(),
      data_dir: self.data_dir.clone(),
      account: self.account.clone(),
      key: self.key.duplicate(),
      tokens: self.tokens.clone(),
      listener: self.listener.clone(),
      is_memory_constrained: self.is_memory_constrained,
      signals: self.signals.clone(),
    }
  }
}

impl Engine {
  fn open(start: EngineStart) -> Result<Self, OpenError> {
    std::fs::create_dir_all(&start.data_dir)
      .map_err(|error| OpenError::Failed(TeamCraftError::Storage(error.to_string())))?;
    remove_shared_downloads(&start.data_dir.join(DOWNLOADS_DIR));
    let [download_dir, upload_staging_dir] = start.plaintext_dirs();
    let path = start.store_path();
    let EngineStart {
      server_url,
      maybe_push_gateway_url,
      key,
      tokens,
      listener,
      signals,
      is_memory_constrained,
      ..
    } = start;
    let (limits, cached_messages) = if is_memory_constrained {
      (SealedStoreLimits::minimal(), 0)
    } else {
      (SealedStoreLimits::default(), MESSAGE_CACHE_MESSAGES)
    };
    let store = SealedSqliteStore::open_with_limits(&path, &StaticKeySource::new(key), limits)
      .map_err(|error| open_error(error.into()))?;
    let store = TrackedStore::with_message_cache(Arc::new(store), cached_messages);
    // Plaintext copies of files that were never sent (taken out of the composer, app stopped).
    if let Err(error) = sweep_staged_copies(&store, &upload_staging_dir) {
      tracing::debug!(%error, "stray staged upload copies could not be listed");
    }
    // A broken record only costs the preferences, not the store.
    let mentions: MentionSettings = load_mention_preferences(&store).unwrap_or_default().into();
    trim_downloads(&download_dir, MAX_DOWNLOADS_BYTES);
    let mut engine = Self {
      server_url,
      maybe_push_gateway_url,
      download_dir,
      upload_staging_dir,
      tokens: ForeignTokens(tokens),
      listener,
      store,
      client: ClientSlot::Missing,
      maybe_backend: None,
      identity_fingerprint: String::new(),
      maybe_recovery: None,
      maybe_link_offer: None,
      maybe_link_announcement: None,
      link_requests: LinkRequestList::default(),
      new_link: NewLinkWait::default(),
      maybe_approval: None,
      maybe_receipt: None,
      calls: CallHub::default(),
      mentions,
      signals,
      is_syncing_for_app: false,
      active_drains: 0,
      maybe_jobs: None,
    };
    // Records that don't decode, or a key bag its own key doesn't open, make the store unreadable
    // too.
    let maybe_client = Client::open(
      engine.api(),
      engine.store.clone(),
      Box::new(SystemClock),
      engine.client_settings(),
    )
    .map_err(open_error)?;
    if let Some(client) = maybe_client {
      engine.adopt(client);
    }
    Ok(engine)
  }

  pub(crate) async fn account_state(&mut self) -> Result<AccountState, TeamCraftError> {
    if let Some(backend) = &self.maybe_backend {
      let me = backend.me();
      return Ok(AccountState::Ready {
        user_id: me.id.to_string(),
        display_name: me.display_name,
        identity_fingerprint: self.identity_fingerprint.clone(),
      });
    }
    // A device link whose keys arrived before the app was stopped finishes enrolling now (#168).
    match PendingEnrollment::resume_link(
      self.api(),
      self.store.clone(),
      Box::new(SystemClock),
      self.client_settings(),
      Argon2Settings::default(),
    )
    .await
    {
      Ok(Some(client)) => {
        self.adopt(client);
        self.listener.on_change();
        return Box::pin(self.account_state()).await;
      },
      Ok(None) => {},
      Err(error) if error.is_transient() || error.is_sign_in_required() => return Err(error.into()),
      // Dropped if they can't work; the user links again.
      Err(error) => tracing::warn!(%error, "could not finish the enrollment of an interrupted device link"),
    }
    self.drop_abandoned_setup().await;
    let api = self.api();
    let me = api.me().await.map_err(teamcraft_core::CoreError::from)?;
    let has_existing_identity = api
      .identity_key(me.id)
      .await
      .map_err(teamcraft_core::CoreError::from)?
      .is_some();
    Ok(AccountState::NeedsEnrollment {
      has_existing_identity,
    })
  }

  /// Drops the device key pair of a setup the user gave up on and revokes its registration (issue
  /// #415). Not while a link is in progress here; enrolling is one job, so it never overlaps. A
  /// failure (offline) is retried when the app asks for the account state again.
  async fn drop_abandoned_setup(&self) {
    if self.maybe_link_offer.is_some() || self.new_link.is_linked() {
      return;
    }
    let settings = self.client_settings();
    if let Err(error) =
      PendingEnrollment::drop_abandoned(&self.api(), &self.store, &SystemClock, &settings).await
    {
      tracing::info!(%error, "could not revoke the device of a setup that was given up yet");
    }
  }

  /// The user signs out before this device is set up (issues #168, #415): the keys of a link and the
  /// device key pair go at once, and the server gets a few seconds to revoke the device that key
  /// pair registered (otherwise the next start of this account does). Nothing changes on a device
  /// that is set up.
  pub(crate) async fn forget_unfinished_setup(&mut self) -> Result<(), TeamCraftError> {
    if !matches!(self.client, ClientSlot::Missing) {
      return Ok(());
    }
    self.maybe_link_offer = None;
    self.maybe_link_announcement = None;
    self.new_link = NewLinkWait::default();
    PendingEnrollment::forget_unfinished(&self.api(), &self.store, &SystemClock, SIGN_OUT_REVOKE_PATIENCE)
      .await?;
    Ok(())
  }

  pub(crate) async fn enroll(
    &mut self,
    device_name: String,
    passphrase: String,
  ) -> Result<AccountState, TeamCraftError> {
    let options = enrollment_options(
      device_name,
      EnrollmentSecret::Passphrase(SecretBytes::new(passphrase.into_bytes())),
    );
    self.complete_enrollment(options).await
  }

  pub(crate) async fn start_device_link(
    &mut self,
    device_name: String,
    platform: LinkPlatform,
  ) -> Result<String, TeamCraftError> {
    let offer = LinkOffer::generate();
    let code = offer.to_code();
    self.maybe_link_offer = Some(Rc::new(offer));
    self.maybe_link_announcement = Some(LinkAnnouncement {
      device_name,
      platform: platform.into(),
    });
    self.new_link = NewLinkWait::default();
    Ok(code)
  }

  /// New device: asks the user's other devices to approve this one. Hands the code's key to the
  /// server, so the app calls it only when the user chose the prompt over scanning (e2ee.md §5).
  pub(crate) async fn announce_device_link(&mut self) -> Result<(), TeamCraftError> {
    let offer = self
      .maybe_link_offer
      .as_ref()
      .ok_or(TeamCraftError::NoLinkInProgress)?;
    let announcement = self
      .maybe_link_announcement
      .as_ref()
      .ok_or(TeamCraftError::NoLinkInProgress)?;
    offer.announce(&self.api(), announcement).await?;
    Ok(())
  }

  /// New device: waits in the relay for the other device; the receiver gets the short code both
  /// show. The wait runs beside the jobs, as long as no device answers: run as a job it held the
  /// engine, so the announcement that makes the other devices answer ([`Self::announce_device_link`])
  /// never went out (issue #999). A new link or a reset ends it, and with it the receiver.
  pub(crate) fn wait_for_link_peer(
    &mut self,
  ) -> Result<oneshot::Receiver<Result<String, TeamCraftError>>, TeamCraftError> {
    let offer = self
      .maybe_link_offer
      .clone()
      .ok_or(TeamCraftError::NoLinkInProgress)?;
    Ok(self.new_link.start(self.api(), offer))
  }

  /// New phone: joins the relay of the code a signed-in device shows for it (issue #998) instead of
  /// waiting in its own; the receiver gets the short code. A code of the wrong kind is refused at
  /// once and leaves the running wait alone; otherwise that wait ends.
  pub(crate) fn join_scanned_device_link(
    &mut self,
    code: &str,
  ) -> Result<oneshot::Receiver<Result<String, TeamCraftError>>, TeamCraftError> {
    if self.maybe_link_offer.is_none() {
      return Err(TeamCraftError::NoLinkInProgress);
    }
    if LinkOffer::parse(code)?.kind() != LinkCodeKind::ShownBySignedInDevice {
      return Err(CoreError::LinkCodeForSignedInDevice.into());
    }
    Ok(self.new_link.start_scanned(self.api(), code.to_owned()))
  }

  pub(crate) async fn finish_device_link(
    &mut self,
    device_name: String,
  ) -> Result<AccountState, TeamCraftError> {
    let link = self.new_link.take().ok_or(TeamCraftError::NoLinkInProgress)?;
    self.maybe_link_offer = None;
    self.maybe_link_announcement = None;
    let secrets = link.receive_secrets(&self.store, &device_name).await?;
    let options = enrollment_options(device_name, EnrollmentSecret::Linked(secrets));
    self.complete_enrollment(options).await
  }

  /// Existing device: joins the new device's relay with its scanned or typed `code`. The user then
  /// types the code the new device shows into [`Self::confirm_device_link`].
  pub(crate) async fn approve_device_link(&mut self, code: String) -> Result<(), TeamCraftError> {
    self
      .begin_approval(|client| Box::pin(async move { client.start_link_approval(&code).await }))
      .await
  }

  /// Existing device: takes up a request from a prompt; same next step as a scanned code.
  pub(crate) async fn review_link_request(&mut self, id: String) -> Result<(), TeamCraftError> {
    self
      .begin_approval(|client| Box::pin(async move { client.review_link_request(&id).await }))
      .await
  }

  pub(crate) async fn reject_link_request(&mut self, id: String) -> Result<(), TeamCraftError> {
    self
      .with_client(|client| Box::pin(async move { client.reject_link_request(&id).await }))
      .await
  }

  /// The open link requests, read from the shared list (the sync loop keeps running).
  pub(crate) fn link_requests(&self) -> Vec<LinkRequestItem> {
    self
      .link_requests
      .open(SystemClock.now_ms())
      .into_iter()
      .map(LinkRequestItem::from)
      .collect()
  }

  async fn begin_approval<F>(&mut self, start: F) -> Result<(), TeamCraftError>
  where
    F: for<'a> FnOnce(
      &'a mut CoreClient,
    )
      -> LocalFuture<'a, Result<LinkApproval<HttpRendezvous>, teamcraft_core::CoreError>>,
  {
    let was_running = matches!(self.client, ClientSlot::Running { .. });
    let mut client = self.take_client().await?;
    let result = start(&mut client).await;
    self.client = ClientSlot::Idle(client);
    let approval = match result {
      Ok(approval) => approval,
      Err(error) => {
        if was_running {
          self.start().await?;
        }
        return Err(error.into());
      },
    };
    self.maybe_approval = Some(PendingApproval {
      approval,
      was_running,
      misses: 0,
    });
    Ok(())
  }

  /// Existing device: the user typed the code the new device shows. Only a match sends the keys
  /// (docs/security-review.md C3: a script or a stray tap can't approve a device the user did not
  /// see); a few wrong codes end the attempt. Syncing resumes once the keys are sent; the new
  /// device's receipt is awaited beside the engine's jobs (issue #415), and the returned receiver
  /// gets its outcome. One receipt is awaited at a time: a wait still running for an earlier link
  /// ends as cancelled.
  pub(crate) async fn confirm_device_link(
    &mut self,
    typed_code: String,
  ) -> Result<oneshot::Receiver<Result<(), TeamCraftError>>, TeamCraftError> {
    let digits = |text: &str| text.chars().filter(char::is_ascii_digit).collect::<String>();
    let pending = self
      .maybe_approval
      .as_mut()
      .ok_or(TeamCraftError::NoLinkInProgress)?;
    if digits(&typed_code) != digits(&pending.approval.short_code()) {
      pending.misses += 1;
      let was_running = pending.was_running;
      if pending.misses >= MAX_APPROVAL_MISSES {
        self.maybe_approval = None;
        if was_running {
          self.start().await?;
        }
        return Err(TeamCraftError::LinkAttemptEnded);
      }
      return Err(TeamCraftError::LinkCodeRejected);
    }
    let pending = self
      .maybe_approval
      .take()
      .ok_or(TeamCraftError::NoLinkInProgress)?;
    let mut client = self.take_client().await?;
    let sent = client.send_link_transfer(pending.approval).await;
    self.client = ClientSlot::Idle(client);
    if pending.was_running {
      self.start().await?;
    }
    let (waiting, cancel) = sent?.detach();
    self.maybe_receipt = Some(cancel);
    let (outcome, receiver) = oneshot::channel();
    tokio::task::spawn_local(async move {
      // The app may have stopped waiting (a cancelled coroutine); nobody is left to tell then.
      let _ = outcome.send(waiting.await.map_err(TeamCraftError::from));
    });
    Ok(receiver)
  }

  /// Drops an approval the user abandoned (the codes were never confirmed) and resumes syncing; a
  /// receipt still awaited ends as cancelled.
  pub(crate) async fn cancel_device_link_approval(&mut self) -> Result<(), TeamCraftError> {
    self.maybe_receipt = None;
    let Some(pending) = self.maybe_approval.take() else {
      return Ok(());
    };
    if pending.was_running {
      self.start().await?;
    }
    Ok(())
  }

  pub(crate) async fn start(&mut self) -> Result<(), TeamCraftError> {
    let client = match std::mem::replace(&mut self.client, ClientSlot::Missing) {
      ClientSlot::Idle(client) => client,
      // Already running: keep it (the app starts on sign-in and again on every foreground).
      running @ ClientSlot::Running { .. } => {
        self.client = running;
        return Ok(());
      },
      ClientSlot::Missing => return Err(TeamCraftError::NotEnrolled),
    };
    let mut client = client;
    client.share_catch_up_focus(self.signals.catch_up_focus.clone());
    let listener = self.listener.clone();
    let (mut runner, handle) = SyncRunner::new(
      *client,
      RunnerSettings::default(),
      Box::new(move || listener.on_change()),
    );
    runner.watch_network(&self.signals.network);
    runner.share_connection_status(self.signals.connection.clone());
    let task = tokio::task::spawn_local(runner.run());
    self.client = ClientSlot::Running { handle, task };
    Ok(())
  }

  /// The device became active or idle (issue #472): the sync loop tells the server now and on every
  /// new connection; a client that isn't syncing keeps it for its next connection.
  pub(crate) fn report_activity(&mut self, activity: DeviceActivity) {
    match &mut self.client {
      ClientSlot::Running { handle, .. } => {
        // Refused only by a loop that ended abnormally; the app reports again on its next
        // foreground or touch.
        handle.send(RunnerCommand::SetActivity(activity));
      },
      ClientSlot::Idle(client) => client.set_device_activity(activity),
      // Not enrolled yet: the first connection identifies as active.
      ClientSlot::Missing => {},
    }
  }

  /// The app starts syncing (foreground); a loop a background drain started goes on as the app's.
  pub(crate) async fn start_syncing(&mut self) -> Result<(), TeamCraftError> {
    self.is_syncing_for_app = true;
    self.start().await
  }

  /// The app stops syncing (background). While a drain waits the loop goes on for it, and the last
  /// drain to end stops it.
  pub(crate) async fn stop_syncing(&mut self) -> Result<(), TeamCraftError> {
    self.is_syncing_for_app = false;
    if self.active_drains > 0 {
      return Ok(());
    }
    self.stop().await
  }

  /// Runs the sync loop until nothing waits for the server or `max_wait` has passed (issue #158),
  /// then answers with what is still pending. A loop the app syncs with keeps running; otherwise
  /// the loop stops at the end. The end runs on the engine thread whatever the caller does, so an
  /// OS that cancels the caller's background work leaves no loop running.
  pub(crate) async fn begin_drain(
    &mut self,
    max_wait: Duration,
  ) -> Result<oneshot::Receiver<Result<PendingWork, TeamCraftError>>, TeamCraftError> {
    self.start().await?;
    self.active_drains += 1;
    let (drained, is_drained) = futures_channel::oneshot::channel();
    if let ClientSlot::Running { handle, .. } = &self.client {
      // Refused only by a loop that ended abnormally: the wait ends at once then.
      handle.send(RunnerCommand::Drain(drained));
    }
    let maybe_jobs = self.maybe_jobs.clone();
    let (answer, answered) = oneshot::channel();
    tokio::task::spawn_local(async move {
      // Drained, out of time, or the loop stopped (the app went on or off meanwhile).
      let _ = tokio::time::timeout(max_wait, is_drained).await;
      let end: Job = Box::new(move |engine| {
        Box::pin(async move {
          // The caller may have been cancelled; the loop is stopped all the same.
          let _ = answer.send(engine.end_drain().await);
        })
      });
      // No queue: the app dropped its handle and the engine thread is ending; it stops the loop.
      if let Some(jobs) = maybe_jobs.and_then(|jobs| jobs.upgrade()) {
        let _ = jobs.send(end);
      }
    });
    Ok(answered)
  }

  /// What still waits for the server: the outbox, uploads and queued actions.
  pub(crate) fn pending_work(&self) -> Result<PendingWork, TeamCraftError> {
    PendingWork::read(&self.store).map_err(TeamCraftError::from)
  }

  async fn end_drain(&mut self) -> Result<PendingWork, TeamCraftError> {
    self.active_drains = self.active_drains.saturating_sub(1);
    if self.active_drains == 0 && !self.is_syncing_for_app {
      self.stop().await?;
    }
    self.pending_work()
  }

  pub(crate) async fn stop(&mut self) -> Result<(), TeamCraftError> {
    self.stop_runner().await;
    // The app goes to the background: clear what the debounce kept in the write-ahead log (#417).
    if let Err(error) = self.store.purge_freed() {
      tracing::warn!(%error, "could not clear the local database log");
    }
    Ok(())
  }

  pub(crate) fn conversations(&self) -> Result<Vec<ConversationItem>, TeamCraftError> {
    let backend = self.backend()?;
    let spaces: BTreeMap<_, _> = backend
      .spaces()
      .into_iter()
      .map(|space| (space.id, space.name))
      .collect();
    let mut conversations = Vec::new();
    for space in spaces.keys() {
      conversations.extend(backend.space_conversations(*space));
    }
    conversations.extend(backend.direct_conversations());
    let me = backend.me();
    Ok(
      conversations
        .into_iter()
        .map(|conversation| {
          let maybe_peer = direct_peer(backend, &conversation, me.id);
          let maybe_space_name = conversation
            .maybe_space
            .and_then(|space| spaces.get(&space).cloned());
          ConversationItem::from_view(conversation, maybe_space_name, maybe_peer, me.is_guest)
        })
        .collect(),
    )
  }

  /// Where a share from another app can go ([`share_targets`]), each with its last activity.
  pub(crate) fn share_targets(&self) -> Result<Vec<ConversationItem>, TeamCraftError> {
    let backend = self.backend()?;
    let items = self
      .conversations()?
      .into_iter()
      .filter(|item| item.can_post)
      .map(|item| {
        let maybe_last_activity_ms = parse_id::<ConversationId>(&item.id)
          .ok()
          .and_then(|id| backend.last_activity(id));
        ConversationItem {
          maybe_last_activity_ms,
          ..item
        }
      })
      .collect();
    Ok(share_targets(items))
  }

  pub(crate) fn key_catch_up(&self) -> Result<Option<KeyCatchUpItem>, TeamCraftError> {
    Ok(self.backend()?.key_catch_up().map(|progress| KeyCatchUpItem {
      caught_up: u32::try_from(progress.caught_up).unwrap_or(u32::MAX),
      total: u32::try_from(progress.total).unwrap_or(u32::MAX),
    }))
  }

  pub(crate) fn messages(
    &self,
    conversation_id: &str,
    limit: u32,
  ) -> Result<Vec<MessageItem>, TeamCraftError> {
    let backend = self.backend()?;
    let conversation: ConversationId = parse_id(conversation_id)?;
    self.signals.catch_up_focus.set(conversation);
    // One read for count and page: a separate count could change before the page is read and
    // shift the window (issue #1060).
    let tail = backend.feed_tail(conversation, None, usize::try_from(limit).unwrap_or(usize::MAX));
    Ok(self.message_items(backend, &tail.messages))
  }

  pub(crate) fn messages_from(
    &self,
    conversation_id: &str,
    maybe_oldest_id: Option<&str>,
    oldest_sent_at_ms: i64,
    older: u32,
  ) -> Result<MessagePage, TeamCraftError> {
    let backend = self.backend()?;
    let conversation: ConversationId = parse_id(conversation_id)?;
    self.signals.catch_up_focus.set(conversation);
    let tail = backend.feed_tail(
      conversation,
      feed_anchor(maybe_oldest_id, oldest_sent_at_ms)?,
      older_len(older),
    );
    Ok(MessagePage {
      messages: self.message_items(backend, &tail.messages),
      has_older: tail.has_older,
    })
  }

  pub(crate) fn thread(
    &self,
    conversation_id: &str,
    thread_id: &str,
  ) -> Result<Vec<MessageItem>, TeamCraftError> {
    let backend = self.backend()?;
    let conversation = parse_id(conversation_id)?;
    self.signals.catch_up_focus.set(conversation);
    let messages = backend.thread(conversation, parse_id(thread_id)?);
    Ok(self.message_items(backend, &messages))
  }

  pub(crate) async fn send_message(
    &mut self,
    conversation_id: String,
    maybe_thread_id: Option<String>,
    body: String,
  ) -> Result<(), TeamCraftError> {
    let conversation: ConversationId = parse_id(&conversation_id)?;
    let maybe_thread: Option<ThreadId> = maybe_thread_id.as_deref().map(parse_id).transpose()?;
    if body.trim().is_empty() {
      return Err(TeamCraftError::InvalidInput("empty message".into()));
    }
    self.backend()?.post_message(conversation, maybe_thread, &body);
    self.dispatch_commands().await
  }

  pub(crate) async fn mark_read(&mut self, conversation_id: String) -> Result<(), TeamCraftError> {
    self.backend()?.mark_read(parse_id(&conversation_id)?);
    self.dispatch_commands().await
  }

  /// Whether the conversation's menu offers "Leave" (#1105). Channel roles are read afresh while
  /// they are not known, so a later ask has the last-admin answer.
  pub(crate) fn conversation_leave_choice(
    &self,
    conversation_id: &str,
  ) -> Result<LeaveChoice, TeamCraftError> {
    let backend = self.backend()?;
    let id = parse_id(conversation_id)?;
    let Some(conversation) = backend.conversation(id) else {
      return Ok(LeaveChoice::Hidden);
    };
    let roles = backend.channel_roles(id);
    if roles.is_empty() {
      backend.refresh_channel_roles(id);
    }
    Ok(channel_leave(&conversation, backend.me().id, &roles).into())
  }

  pub(crate) fn space_menu(&self, space_id: &str) -> Result<Option<SpaceMenuItem>, TeamCraftError> {
    let backend = self.backend()?;
    let id = parse_id(space_id)?;
    let Some(space) = backend.spaces().into_iter().find(|space| space.id == id) else {
      return Ok(None);
    };
    let is_admin = space.maybe_my_role == Some(SpaceRole::SpaceAdmin);
    let roster = if is_admin {
      let roster = backend.space_members(id);
      if matches!(roster, SpaceRoster::Loading) {
        backend.refresh_space_roster(id);
      }
      roster
    } else {
      SpaceRoster::Loading
    };
    let has_unread = backend
      .space_conversations(id)
      .iter()
      .any(|conversation| conversation.unread_count > 0 || conversation.mention_count > 0);
    Ok(Some(SpaceMenuItem {
      name: space.name,
      has_unread,
      leave: space_leave(space.maybe_my_role.as_ref(), backend.me().id, &roster).into(),
    }))
  }

  pub(crate) async fn mark_space_read(&mut self, space_id: String) -> Result<(), TeamCraftError> {
    let backend = self.backend()?;
    for conversation in backend.space_conversations(parse_id(&space_id)?) {
      if conversation.unread_count > 0 || conversation.mention_count > 0 {
        backend.mark_read(conversation.id);
      }
    }
    self.dispatch_commands().await
  }

  /// Leaves the space and its channels, only where [`Self::space_menu`] offers it: the server's
  /// last-admin refusal is told before the call, not after.
  pub(crate) async fn leave_space(&mut self, space_id: String) -> Result<(), TeamCraftError> {
    let Some(menu) = self.space_menu(&space_id)? else {
      return Err(TeamCraftError::InvalidInput(format!(
        "not a space of this account: {space_id}"
      )));
    };
    if menu.leave != LeaveChoice::Allowed {
      return Err(TeamCraftError::InvalidInput(
        "leaving this space is not allowed".to_owned(),
      ));
    }
    let backend = self.backend()?;
    backend.remove_space_member(parse_id(&space_id)?, backend.me().id);
    self.dispatch_commands().await
  }

  pub(crate) async fn set_favorite(
    &mut self,
    conversation_id: String,
    is_favorite: bool,
  ) -> Result<(), TeamCraftError> {
    self
      .backend()?
      .set_favorite(parse_id(&conversation_id)?, is_favorite);
    self.dispatch_commands().await
  }

  pub(crate) async fn set_presence(&mut self, presence: Presence) -> Result<(), TeamCraftError> {
    self.backend()?.set_presence(presence.into());
    self.dispatch_commands().await
  }

  pub(crate) fn search(&self, query: &str) -> Result<Vec<SearchHitItem>, TeamCraftError> {
    let backend = self.backend()?;
    let results = backend.search(&parse_query(query));
    Ok(
      results
        .hits
        .into_iter()
        .map(|hit| SearchHitItem {
          message: self.message_item(backend, &hit.message),
          conversation_name: hit.conversation_name,
          maybe_space_name: hit.maybe_space_name,
        })
        .collect(),
    )
  }

  pub(crate) async fn register_push(
    &mut self,
    service: PushService,
    token: String,
  ) -> Result<(), TeamCraftError> {
    let was_running = matches!(self.client, ClientSlot::Running { .. });
    let mut client = self.take_client().await?;
    let result = client.register_push_keeping_key(service.into(), &token).await;
    self.client = ClientSlot::Idle(client);
    if was_running {
      self.start().await?;
    }
    Ok(result?)
  }

  /// Seals the registered device token again when that is due; whether it was renewed.
  pub(crate) async fn refresh_push(&mut self) -> Result<bool, TeamCraftError> {
    let was_running = matches!(self.client, ClientSlot::Running { .. });
    let mut client = self.take_client().await?;
    let result = client.refresh_push().await;
    self.client = ClientSlot::Idle(client);
    if was_running {
      self.start().await?;
    }
    Ok(result?)
  }

  pub(crate) async fn unregister_push(&mut self) -> Result<(), TeamCraftError> {
    self
      .with_client(|client| Box::pin(async move { client.unregister_push().await }))
      .await
  }

  /// What joining the conversation's call needs from the client; pauses syncing meanwhile.
  pub(crate) async fn prepare_call(
    &mut self,
    conversation_id: &str,
  ) -> Result<CallPreparation, TeamCraftError> {
    let conversation: ConversationId = parse_id(conversation_id)?;
    let was_running = matches!(self.client, ClientSlot::Running { .. });
    let mut client = self.take_client().await?;
    let result = prepare_call(&mut client, conversation).await;
    self.client = ClientSlot::Idle(client);
    if was_running {
      self.start().await?;
    }
    result
  }

  /// "Add people" in the DM call of `conversation_id`: a meeting room with everybody in the call
  /// and `added`, into which the call moves (docs/meetings.md). The same `request_key` again gets
  /// the same room. Returns the room's chat.
  pub(crate) async fn add_people_to_call(
    &mut self,
    conversation_id: &str,
    in_call: &[String],
    added: &[String],
    maybe_title: Option<String>,
    request_key: &str,
  ) -> Result<String, TeamCraftError> {
    let from: ConversationId = parse_id(conversation_id)?;
    let in_call = parse_ids(in_call)?;
    let added = parse_ids(added)?;
    let key = teamcraft_proto::IdempotencyKey::from_hex(request_key)
      .map_err(|_| TeamCraftError::InvalidInput("not a request key".to_owned()))?;
    let was_running = matches!(self.client, ClientSlot::Running { .. });
    let mut client = self.take_client().await?;
    let result = client
      .add_people_to_call(from, &in_call, &added, maybe_title, key)
      .await;
    self.client = ClientSlot::Idle(client);
    if was_running {
      self.start().await?;
    }
    self.listener.on_change();
    Ok(result?.to_string())
  }

  /// Checks an announced move of the call before this device follows it.
  pub(crate) async fn accept_call_move(&mut self, from: &str, to: &str) -> Result<(), TeamCraftError> {
    let from: ConversationId = parse_id(from)?;
    let to: ConversationId = parse_id(to)?;
    self
      .with_client(move |client| Box::pin(async move { client.accept_call_move(from, to).await }))
      .await
  }

  /// The running call's verified devices afresh, after catching up the conversation's group (a
  /// device added during the call joins it, e2ee.md §8.2).
  pub(crate) async fn call_devices(
    &mut self,
    conversation: ConversationId,
  ) -> Result<Vec<teamcraft_crypto::identity::VerifiedDevice>, TeamCraftError> {
    let was_running = matches!(self.client, ClientSlot::Running { .. });
    let mut client = self.take_client().await?;
    let result = client.refresh_call_devices(conversation).await;
    self.client = ClientSlot::Idle(client);
    if was_running {
      self.start().await?;
    }
    Ok(result?)
  }

  /// A fresh join token for the conversation's call, to rejoin after a lost connection.
  pub(crate) async fn call_join_token(
    &mut self,
    conversation: ConversationId,
  ) -> Result<CallJoinTokenResponse, TeamCraftError> {
    let was_running = matches!(self.client, ClientSlot::Running { .. });
    let client = self.take_client().await?;
    let result = client.call_join_token(conversation).await;
    self.client = ClientSlot::Idle(client);
    if was_running {
      self.start().await?;
    }
    Ok(result?)
  }

  pub(crate) fn calls(&self) -> &CallHub {
    &self.calls
  }

  pub(crate) async fn notification_for_push(
    &mut self,
    payload: Vec<u8>,
    utc_offset_minutes: i16,
    opening: PushOpening,
    reader: PushReader,
  ) -> Result<PushNotification, TeamCraftError> {
    let preview = self.push_preview_within_budget(&payload, reader).await?;
    let now_ms = SystemClock.now_ms();
    let store = &self.store;
    // A retry is the same push again: counting it would skew the received count and the delay.
    if opening == PushOpening::Received {
      push_guard::record_received(store, &preview.wake_up, now_ms);
    }
    let is_duplicate = push_guard::is_duplicate(store, &preview.wake_up)?;
    let maybe_suppression =
      push_guard::local_suppression(store, &preview.wake_up, now_ms, utc_offset_minutes)?;
    // Opening the conversation from this notification shows what it showed (issue #1065).
    if let Err(error) = notified::remember(store, &preview) {
      tracing::debug!(%error, "notified message not kept");
    }
    let mut notification: PushNotification = preview.into();
    notification.is_duplicate = is_duplicate;
    notification.maybe_suppression = maybe_suppression.map(Into::into);
    if !notification.is_mention
      && let (Some(text), Ok(backend)) = (&notification.maybe_text, self.backend())
    {
      let me = backend.me();
      notification.is_mention = is_mentioned(
        text,
        &MentionRules {
          username: &me.username,
          display_name: &me.display_name,
          first_name_is_mention: self.mentions.is_first_name_mention,
          channel_mentions_notify: self.mentions.do_channel_mentions_notify,
          keywords: &self.mentions.keywords,
        },
      );
    }
    Ok(notification)
  }

  /// The preview of a wake-up within [`PUSH_PREVIEW_BUDGET`], so the push service's time for the
  /// app (FCM: about 10 s) is never spent on a slow network: a fetch that takes longer, or a catch-up
  /// that doesn't finish, leaves a notification that names the sender instead of none.
  async fn push_preview_within_budget(
    &mut self,
    payload: &[u8],
    reader: PushReader,
  ) -> Result<PushPreview, TeamCraftError> {
    let deadline = tokio::time::Instant::now() + PUSH_PREVIEW_BUDGET;
    let api = self.api();
    // Without a token the preview still names conversation and sender.
    let maybe_api = self.tokens.0.access_token().map(|_| &api);
    // Cancel-safe: the preview only reads the store.
    let fetched =
      tokio::time::timeout(PUSH_FETCH_BUDGET, push_preview(&self.store, maybe_api, payload)).await;
    let mut preview = match fetched {
      Ok(result) => result?,
      Err(_elapsed) => {
        let mut preview = push_preview::<Api>(&self.store, None, payload).await?;
        // Only a missing message is the timeout's fault; a policy or setting cut stays as it is.
        if matches!(
          preview.maybe_gap,
          Some(PreviewGap::NoSession | PreviewGap::NotDecrypted)
        ) {
          preview.maybe_gap = Some(PreviewGap::TimedOut);
        }
        preview
      },
    };
    // The app's own client may catch the conversation up: a conversation this device was just
    // added to, or keys that changed while the app was closed. Not while the sync loop runs: it
    // brings the message itself, and taking the client would cut its connection. Never in a
    // notification extension ([`PushReader::Extension`]).
    let is_client_idle = matches!(self.client, ClientSlot::Idle(_));
    if reader.catches_up(preview.needs_catch_up(), is_client_idle)
      && let ClientSlot::Idle(client) = &mut self.client
    {
      let conversation = preview.wake_up.conversation_id;
      if let Err(error) = catch_up_for_push(client, conversation, deadline).await {
        tracing::debug!(%error, conversation_id = %conversation, "push catch-up stopped");
      }
      let caught_up = push_preview::<Api>(&self.store, None, payload).await?;
      // The store-only preview names `NoSession` while the message is still missing: then the first
      // try's reason stays (a failed decryption, a timeout), with any names the catch-up learned.
      if caught_up.maybe_gap != Some(PreviewGap::NoSession) {
        preview = caught_up;
      } else {
        preview.maybe_sender_name = preview.maybe_sender_name.or(caught_up.maybe_sender_name);
        preview.maybe_conversation_name = preview
          .maybe_conversation_name
          .or(caught_up.maybe_conversation_name);
      }
    }
    Ok(preview)
  }

  /// What the newest notification showed of the conversation, while the store hasn't caught up
  /// to it (see [`notified::notified_conversation`]).
  pub(crate) fn notified_conversation(
    &self,
    conversation_id: &str,
  ) -> Result<Option<NotifiedConversation>, TeamCraftError> {
    let conversation: ConversationId = parse_id(conversation_id)?;
    Ok(notified::notified_conversation(&self.store, conversation)?.map(Into::into))
  }

  pub(crate) fn push_diagnostics(&self) -> Result<PushDiagnosticsInfo, TeamCraftError> {
    Ok(push_guard::push_diagnostics(&self.store)?.into())
  }

  pub(crate) async fn send_test_push(&self) -> Result<TestPushResult, TeamCraftError> {
    let api = self.api();
    Ok(push_guard::send_test_push(&self.store, &api).await?.into())
  }

  /// Remembers the preferences in the store, so a process that opens the store later (the iOS
  /// notification extension) applies them too.
  pub(crate) fn set_mention_settings(&mut self, settings: MentionSettings) -> Result<(), TeamCraftError> {
    save_mention_preferences(&self.store, &settings.clone().into())?;
    self.mentions = settings;
    Ok(())
  }

  pub(crate) fn mention_settings(&self) -> MentionSettings {
    self.mentions.clone()
  }

  /// This device's notification privacy setting and the workspace policy's limit; both stored, so
  /// a notification extension reads them too.
  pub(crate) fn notification_content(&self) -> NotificationContentSettings {
    let store = &self.store;
    NotificationContentSettings {
      device: (&device_push_content(store)).into(),
      workspace: (&push_content_mode(store)).into(),
    }
  }

  pub(crate) fn set_notification_content(&self, content: NotificationContent) -> Result<(), TeamCraftError> {
    Ok(save_device_push_content(&self.store, &content.into())?)
  }

  pub(crate) fn segment_prefetch(&self) -> Result<SegmentPrefetchMode, TeamCraftError> {
    Ok(load_segment_prefetch(&self.store)?.into())
  }

  pub(crate) fn set_segment_prefetch(&self, mode: SegmentPrefetchMode) -> Result<(), TeamCraftError> {
    Ok(save_segment_prefetch(&self.store, mode.into())?)
  }

  /// Prefetches the search segments if the saved mode allows it on this network; nothing is
  /// fetched otherwise.
  pub(crate) async fn prefetch_segments(
    &mut self,
    is_network_metered: bool,
    max_total_bytes: u64,
  ) -> Result<SegmentPrefetchResult, TeamCraftError> {
    if !load_segment_prefetch(&self.store)?.allows(is_network_metered) {
      return Ok(SegmentPrefetchResult {
        fetched: 0,
        skipped: 0,
        stored_bytes: 0,
      });
    }
    self.prefetch_search_segments(max_total_bytes).await
  }

  /// Downloads missing search segments, newest first, within `max_total_bytes`; pauses syncing
  /// meanwhile.
  pub(crate) async fn prefetch_search_segments(
    &mut self,
    max_total_bytes: u64,
  ) -> Result<SegmentPrefetchResult, TeamCraftError> {
    let was_running = matches!(self.client, ClientSlot::Running { .. });
    let mut client = self.take_client().await?;
    let result = client.prefetch_index_segments(max_total_bytes).await;
    self.client = ClientSlot::Idle(client);
    if was_running {
      self.start().await?;
    }
    Ok(result?.into())
  }

  pub(crate) async fn complete_enrollment(
    &mut self,
    options: EnrollmentOptions,
  ) -> Result<AccountState, TeamCraftError> {
    let client = PendingEnrollment::new()
      .complete(
        self.api(),
        self.store.clone(),
        Box::new(SystemClock),
        self.client_settings(),
        options,
      )
      .await?;
    self.adopt(client);
    self.listener.on_change();
    self.account_state().await
  }

  fn adopt(&mut self, mut client: CoreClient) {
    self.attach_call_sink(&mut client);
    self.link_requests = client.link_request_list();
    self.identity_fingerprint = client.identity_fingerprint();
    self.maybe_backend = Some(
      CoreChatBackend::new(self.store.clone(), client.user_id(), Box::new(SystemClock))
        .with_typing(client.typing_state())
        .with_transfers(client.transfers()),
    );
    self.client = ClientSlot::Idle(Box::new(client));
  }

  /// Gateway call events (rings) of `client` go to the app's call listener.
  fn attach_call_sink(&self, client: &mut CoreClient) {
    let calls = self.calls.clone();
    client.set_call_event_sink(Box::new(move |event| calls.on_gateway_event(event)));
  }

  /// Stops the sync loop if it runs and hands out the client; put it back into `self.client`.
  async fn take_client(&mut self) -> Result<Box<CoreClient>, TeamCraftError> {
    self.stop_runner().await;
    match std::mem::replace(&mut self.client, ClientSlot::Missing) {
      ClientSlot::Idle(client) => Ok(client),
      _ => Err(TeamCraftError::NotEnrolled),
    }
  }

  async fn stop_runner(&mut self) {
    if let ClientSlot::Running { handle, task } = std::mem::replace(&mut self.client, ClientSlot::Missing) {
      handle.send(RunnerCommand::Stop);
      match task.await {
        Ok(client) => self.client = ClientSlot::Idle(Box::new(client)),
        // Only a panic in the sync loop gets here; reopen the client from the store.
        Err(error) => {
          tracing::error!(%error, "sync loop ended abnormally");
          if let Ok(Some(mut client)) = Client::open(
            self.api(),
            self.store.clone(),
            Box::new(SystemClock),
            self.client_settings(),
          ) {
            self.attach_call_sink(&mut client);
            self.link_requests = client.link_request_list();
            self.client = ClientSlot::Idle(Box::new(client));
          }
        },
      }
    }
  }

  /// Runs `operation` on the client with syncing paused, then resumes it and tells the app.
  pub(crate) async fn with_client<F>(&mut self, operation: F) -> Result<(), TeamCraftError>
  where
    F: for<'a> FnOnce(&'a mut CoreClient) -> LocalFuture<'a, Result<(), teamcraft_core::CoreError>>,
  {
    let was_running = matches!(self.client, ClientSlot::Running { .. });
    let mut client = self.take_client().await?;
    let result = operation(&mut client).await;
    self.client = ClientSlot::Idle(client);
    if was_running {
      self.start().await?;
    }
    self.listener.on_change();
    Ok(result?)
  }

  /// Hands the UI adapter's queued commands to the sync loop, or runs them right away.
  pub(crate) async fn dispatch_commands(&mut self) -> Result<(), TeamCraftError> {
    let commands = self.backend()?.drain_commands();
    match &mut self.client {
      ClientSlot::Running { handle, .. } => {
        for command in commands {
          handle.send(RunnerCommand::Ui(command));
        }
        handle.send(RunnerCommand::Flush);
      },
      ClientSlot::Idle(client) => {
        // What the action wrote locally (a new message in the outbox) shows before the server
        // answers.
        self.listener.on_change();
        for command in commands {
          run_offline_tolerant(client, command).await?;
        }
      },
      ClientSlot::Missing => return Err(TeamCraftError::NotEnrolled),
    }
    self.listener.on_change();
    Ok(())
  }

  /// Where the app puts copies of files to upload (created if missing).
  pub(crate) fn upload_staging_dir(&self) -> Result<String, TeamCraftError> {
    std::fs::create_dir_all(&self.upload_staging_dir)
      .map_err(|error| TeamCraftError::Storage(error.to_string()))?;
    Ok(self.upload_staging_dir.display().to_string())
  }

  /// Deletes this account's plaintext files: decrypted downloads and staged upload copies (at
  /// sign-out; the sealed store stays).
  pub(crate) fn delete_plaintext_files(&self) -> Result<(), TeamCraftError> {
    delete_plaintext_dirs([self.download_dir.as_path(), self.upload_staging_dir.as_path()])
  }

  pub(crate) fn backend(&self) -> Result<&CoreChatBackend<Store>, TeamCraftError> {
    self.maybe_backend.as_ref().ok_or(TeamCraftError::NotEnrolled)
  }

  fn message_items(&self, backend: &CoreChatBackend<Store>, messages: &[MessageView]) -> Vec<MessageItem> {
    messages
      .iter()
      .map(|message| self.message_item(backend, message))
      .collect()
  }

  fn message_item(&self, backend: &CoreChatBackend<Store>, message: &MessageView) -> MessageItem {
    let maybe_author = backend.user(message.author);
    MessageItem::from_view(message, maybe_author.as_ref(), is_pending(message.id))
  }

  fn client_settings(&self) -> ClientSettings {
    ClientSettings {
      maybe_download_dir: Some(self.download_dir.clone()),
      maybe_push_gateway_url: self.maybe_push_gateway_url.clone(),
      maybe_upload_staging_dir: Some(self.upload_staging_dir.clone()),
      creates_notes_to_self: true,
      ..ClientSettings::default()
    }
  }

  pub(crate) fn api(&self) -> Api {
    HttpServerApi::new(self.server_url.clone(), self.tokens.clone())
  }
}

impl NewLinkWait {
  /// Joins the relay and waits there; a wait still running for an earlier link ends.
  fn start(&mut self, api: Api, offer: Rc<LinkOffer>) -> oneshot::Receiver<Result<String, TeamCraftError>> {
    self.run(async move { NewDeviceLink::connect(&api, &offer).await })
  }

  /// Joins the relay of a scanned code (*Link a phone*); a wait still running ends.
  fn start_scanned(&mut self, api: Api, code: String) -> oneshot::Receiver<Result<String, TeamCraftError>> {
    self.run(async move { NewDeviceLink::join_scanned(&api, &code).await })
  }

  fn run(
    &mut self,
    connecting: impl Future<Output = Result<NewDeviceLink<HttpRendezvous>, CoreError>> + 'static,
  ) -> oneshot::Receiver<Result<String, TeamCraftError>> {
    *self = Self::default();
    let slot = self.link.clone();
    let (answer, receiver) = oneshot::channel();
    self.maybe_task = Some(tokio::task::spawn_local(async move {
      let result = match connecting.await {
        Ok(link) => {
          let short_code = link.short_code();
          *slot.borrow_mut() = Some(link);
          Ok(short_code)
        },
        Err(error) => Err(error.into()),
      };
      // The app may have stopped waiting (a cancelled coroutine); the link stays for a later finish.
      let _ = answer.send(result);
    }));
    receiver
  }

  fn is_linked(&self) -> bool {
    self.link.borrow().is_some()
  }

  fn take(&mut self) -> Option<NewDeviceLink<HttpRendezvous>> {
    self.link.borrow_mut().take()
  }
}

impl Drop for NewLinkWait {
  fn drop(&mut self) {
    if let Some(task) = &self.maybe_task {
      task.abort();
    }
  }
}

impl AccessTokenSource for ForeignTokens {
  fn access_token(&self) -> Result<String, ApiError> {
    self
      .0
      .access_token()
      .ok_or_else(|| ApiError::Token("signed out".into()))
  }
}

/// Runs a UI command on an idle client; offline is fine (the outbox keeps messages).
async fn prepare_call(
  client: &mut CoreClient,
  conversation: ConversationId,
) -> Result<CallPreparation, TeamCraftError> {
  client.ensure_call_group(conversation).await?;
  let join = client.call_join_token(conversation).await?;
  let devices = client.verified_call_devices(conversation).await?;
  Ok(CallPreparation {
    conversation,
    join,
    user_id: client.user_id(),
    device_id: client.device_id(),
    signer: client.call_key_signer(),
    devices,
  })
}

async fn run_offline_tolerant(client: &mut CoreClient, command: CoreCommand) -> Result<(), TeamCraftError> {
  match client.run_ui_command(command).await {
    Ok(()) => Ok(()),
    Err(error) if error.is_transient() => Ok(()),
    Err(error) => Err(error.into()),
  }
}

/// An error of opening the store or the client on it: data this version can't read, now or after a
/// restart, gets its own answer (issue #414); anything else stays a plain failure.
fn open_error(error: CoreError) -> OpenError {
  match error.unreadable_store() {
    Some(problem) => {
      tracing::error!(%error, ?problem, "the local database is unreadable");
      OpenError::Unreadable(problem.into())
    },
    None => OpenError::Failed(error.into()),
  }
}

fn enrollment_options(device_name: String, secret: EnrollmentSecret) -> EnrollmentOptions {
  EnrollmentOptions {
    device_name,
    secret,
    argon2: Argon2Settings::default(),
    maybe_attestation: None,
  }
}

fn parse_ids<T: Id + std::str::FromStr>(texts: &[String]) -> Result<Vec<T>, TeamCraftError> {
  texts.iter().map(|text| parse_id(text)).collect()
}

pub(crate) fn parse_id<T: Id + std::str::FromStr>(text: &str) -> Result<T, TeamCraftError> {
  text
    .parse()
    .map_err(|_| TeamCraftError::InvalidInput(format!("not an ID: {text}")))
}

/// The oldest row a paging list holds, as the apps name it ([`crate::TeamCraft::messages_from`]).
pub(crate) fn feed_anchor(
  maybe_id: Option<&str>,
  sent_at_ms: i64,
) -> Result<Option<FeedAnchor>, TeamCraftError> {
  maybe_id
    .map(|id| {
      parse_id(id).map(|id| FeedAnchor {
        id,
        sent_at: sent_at_ms,
      })
    })
    .transpose()
}

pub(crate) fn older_len(older: u32) -> usize {
  usize::try_from(older).unwrap_or(usize::MAX)
}

/// Downloads of older versions lay directly in the shared folder, visible to every account of
/// the device; they are plaintext caches and are dropped (the files can be downloaded again).
fn remove_shared_downloads(directory: &std::path::Path) {
  let Ok(entries) = std::fs::read_dir(directory) else {
    return;
  };
  for entry in entries.flatten() {
    if entry.file_type().is_ok_and(|kind| kind.is_file())
      && let Err(error) = std::fs::remove_file(entry.path())
    {
      tracing::debug!(%error, "an old shared download could not be deleted");
    }
  }
}

/// Deletes the least recently used files of `directory` (by modification time, which the apps bump
/// when they open a file) until the rest fit into `max_bytes`.
fn trim_downloads(directory: &std::path::Path, max_bytes: u64) {
  let Ok(entries) = std::fs::read_dir(directory) else {
    return;
  };
  let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
    .flatten()
    .filter_map(|entry| {
      let metadata = entry.metadata().ok()?;
      let modified = metadata.modified().unwrap_or(std::time::UNIX_EPOCH);
      metadata
        .is_file()
        .then(|| (modified, metadata.len(), entry.path()))
    })
    .collect();
  let mut total: u64 = files.iter().map(|(_, size, _)| size).sum();
  files.sort_by_key(|(modified, _, _)| *modified);
  for (_, size, path) in files {
    if total <= max_bytes {
      break;
    }
    match std::fs::remove_file(&path) {
      Ok(()) => total -= size,
      Err(error) => tracing::debug!(%error, "an old download could not be deleted"),
    }
  }
}

/// The other person's presence in a one-to-one DM; `None` for every other conversation.
fn direct_peer(
  backend: &impl ChatBackend,
  conversation: &ConversationView,
  me: UserId,
) -> Option<DirectPeer> {
  if conversation.kind != ConversationKindView::Direct {
    return None;
  }
  let other = *conversation.member_ids.iter().find(|member| **member != me)?;
  Some(DirectPeer {
    id: other,
    maybe_presence: backend.user(other).map(|user| Presence::from(user.presence)),
  })
}

#[cfg(test)]
mod tests {
  use std::time::{Duration, SystemTime};

  use super::*;

  /// Issue #424 review, H2: a notification extension never catches a conversation up (it would
  /// advance the MLS state next to the app's copy); the app's idle client does.
  #[test]
  fn only_the_apps_idle_client_catches_a_push_up() {
    assert!(PushReader::App.catches_up(true, true));
    assert!(
      !PushReader::App.catches_up(true, false),
      "the sync loop brings it"
    );
    assert!(!PushReader::App.catches_up(false, true), "nothing missing");
    assert!(!PushReader::Extension.catches_up(true, true));
  }

  #[test]
  fn downloads_above_the_limit_lose_their_least_recently_used_files() {
    let directory = TestDir::new("lru");
    let now = SystemTime::now();
    for (name, age_seconds) in [("old", 300), ("middle", 200), ("new", 100)] {
      let path = directory.path().join(name);
      std::fs::write(&path, [0u8; 10]).unwrap();
      let file = std::fs::File::options().write(true).open(&path).unwrap();
      file.set_modified(now - Duration::from_secs(age_seconds)).unwrap();
    }

    trim_downloads(directory.path(), 20);

    let mut left: Vec<String> = std::fs::read_dir(directory.path())
      .unwrap()
      .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
      .collect();
    left.sort();
    assert_eq!(left, ["middle", "new"]);
  }

  #[test]
  fn downloads_within_the_limit_or_missing_stay_as_they_are() {
    let directory = TestDir::new("within");
    std::fs::write(directory.path().join("one"), [0u8; 10]).unwrap();
    trim_downloads(directory.path(), 10);
    assert!(directory.path().join("one").is_file());
    trim_downloads(&directory.path().join("missing"), 0);
  }

  mod unreadable_store {
    use teamcraft_core::store::StoreError;

    use super::*;

    #[test]
    fn open_errors_name_what_this_version_can_not_read() {
      let newer = CoreError::Store(StoreError::NewerSchema {
        found: 9,
        supported: 2,
      });
      assert!(matches!(
        open_error(newer),
        OpenError::Unreadable(StoreProblem::NewerVersion)
      ));
      for damaged in [
        CoreError::Store(StoreError::WrongKey),
        CoreError::Store(StoreError::Corrupt("file is not a database".into())),
        CoreError::Corrupt("a record does not decode".into()),
      ] {
        assert!(matches!(
          open_error(damaged),
          OpenError::Unreadable(StoreProblem::Damaged)
        ));
      }
      // Busy or out of space: the data may be fine, so it stays a failure to try again.
      assert!(matches!(
        open_error(CoreError::Store(StoreError::Database(
          "database is locked".into()
        ))),
        OpenError::Failed(TeamCraftError::Storage(_))
      ));
    }

    #[test]
    fn the_database_and_its_log_are_renamed_aside_never_deleted() {
      let directory = TestDir::new("aside");
      let path = directory.path().join("alice.teamcraft.db");
      for (name, content) in [
        ("alice.teamcraft.db", "records"),
        ("alice.teamcraft.db-wal", "log"),
        ("alice.teamcraft.db-shm", "index"),
      ] {
        std::fs::write(directory.path().join(name), content).unwrap();
      }

      set_store_aside(&path, 1_700_000_000_000).unwrap();

      for (name, content) in [
        ("alice.teamcraft.db.unreadable-1700000000000", "records"),
        ("alice.teamcraft.db-wal.unreadable-1700000000000", "log"),
        ("alice.teamcraft.db-shm.unreadable-1700000000000", "index"),
      ] {
        assert_eq!(
          std::fs::read_to_string(directory.path().join(name)).unwrap(),
          content
        );
      }
      assert!(!path.exists());
      // Nothing left to move (the log already went, or never was): fine.
      set_store_aside(&path, 1_700_000_000_001).unwrap();
    }
  }

  /// A fresh folder in the system's temporary directory, removed when dropped.
  struct TestDir(PathBuf);

  impl TestDir {
    fn new(name: &str) -> Self {
      let path = std::env::temp_dir().join(format!("teamcraft-downloads-{}-{name}", std::process::id()));
      let _ = std::fs::remove_dir_all(&path);
      std::fs::create_dir_all(&path).unwrap();
      Self(path)
    }

    fn path(&self) -> &std::path::Path {
      &self.0
    }
  }

  impl Drop for TestDir {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.0);
    }
  }
}
