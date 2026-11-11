//! The engine thread: owns the client and the sync loop, and runs the jobs the [`crate::TeamCraft`]
//! handle posts, one at a time, on a current-thread runtime. What the app reads from the local store
//! doesn't wait for it ([`crate::reads`]).

use std::cell::RefCell;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::model::{
  Conversation as ConversationView, ConversationKind as ConversationKindView, UserId,
};
use teamcraft_client_api::notify::{MentionRules, is_mentioned};
use teamcraft_core::api::dpop::DpopProver;
use teamcraft_core::api::{AccessTokenSource, ApiError, HttpRendezvous, HttpServerApi, ServerApi};
use teamcraft_core::client::NewDeviceLink;
use teamcraft_core::client::files::sweep_staged_copies;
use teamcraft_core::client::{
  CatchUpFocus, HistoryBackfill, LinkAnnouncement, LinkApproval, LinkRequestList, PushRegistrar,
  ReceiptCancel, SIGN_OUT_REVOKE_PATIENCE, SegmentPlan,
};
use teamcraft_core::clock::{Clock as _, SystemClock};
use teamcraft_core::connection::{ConnectionStatus, NetworkChanges};
use teamcraft_core::notified;
use teamcraft_core::notify_rules::{load_mention_preferences, save_mention_preferences};
use teamcraft_core::pending_work::PendingWork;
use teamcraft_core::prefetch::{load_segment_prefetch, save_segment_prefetch};
use teamcraft_core::push_guard;
use teamcraft_core::push_preview::{
  PreviewGap, PushPreview, is_read, push_preview, save_device_push_content,
};
use teamcraft_core::push_probe::{PROBE_OVERLAY_BYTES, probe_push};
use teamcraft_core::records;
use teamcraft_core::runner::{RunnerCommand, RunnerHandle, RunnerSettings, SyncRunner};
use teamcraft_core::store::{
  CommitJournal, DEFAULT_MESSAGE_CACHE_BYTES, LaneStore, LocalStore as _, OverlayStore, SealedSqliteStore,
  SealedStoreLimits, StaticKeySource, TrackedStore,
};
use teamcraft_core::ui_backend::{CoreChatBackend, CoreCommand, EntryNeed, FeedAnchor};
use teamcraft_core::{
  Client, ClientSettings, CoreError, EnrollmentOptions, EnrollmentSecret, LinkCodeKind, LinkOffer,
  PendingEnrollment,
};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_crypto::recovery_key::RecoveryKey;
use teamcraft_crypto::secret::{SecretBytes, SymmetricKey};
use teamcraft_proto::{
  CallJoinTokenResponse, ConversationId, CreateCallJoinTokenRequest, DeviceActivity, DeviceId, Id, PushKind,
  PushWakeUp,
};
use tokio::sync::{mpsc, oneshot};
use tokio::task::{JoinHandle, LocalSet};

use crate::calls::{CallHub, CallPreparation, RunningCalls};
use crate::device_proofs::{DeviceKeyProofs, DeviceProofKey};
use crate::error::TeamCraftError;
use crate::logging::LIFECYCLE;
use crate::reads::{ReadLanes, ReadQueue};
use crate::server_pool::ServerPool;
use crate::types::{
  AccountState, AppConfig, DirectPeer, LinkPlatform, LinkRequestItem, MentionSettings, NotificationContent,
  Presence, PushNotification, PushService, SegmentPrefetchMode, SegmentPrefetchResult, StoreProblem,
  TestPushResult,
};
use crate::{AccessToken, EventListener, TokenProvider};

/// Decrypted downloads, one folder per account below it.
/// Wrong short codes allowed before a link approval is dropped.
const MAX_APPROVAL_MISSES: u8 = 3;
const DOWNLOADS_DIR: &str = "downloads";
/// The caches of the open chat's read connection: it reads a few conversations' pages, not the
/// whole chat list, so half the default (docs/performance.md, "Local store").
/// The segment prefetch's connection: it reads each message once, so small caches do.
const SEGMENT_PLAN_LIMITS: SealedStoreLimits = SealedStoreLimits {
  max_entry_bytes: 1024 * 1024,
  max_message_bytes: 1024 * 1024,
  max_index_node_bytes: 512 * 1024,
};

const READ_LANE_LIMITS: SealedStoreLimits = SealedStoreLimits {
  max_entry_bytes: 4 * 1024 * 1024,
  max_message_bytes: 2 * 1024 * 1024,
  max_index_node_bytes: 1024 * 1024,
};

/// Stack of the engine and read threads. They run the deepest calls (creating keys, MLS commits,
/// the sync loop); Rust's 2 MiB default overflowed on a phone in a debug build (#1504). Only
/// address space is reserved; pages are used as the stack grows.
pub(crate) const THREAD_STACK_BYTES: usize = 8 * 1024 * 1024;
/// How long stopping the engine waits for the purge of the local database's log (#1684): a few
/// syncs of a phone's flash when no read holds the log.
const PURGE_BEFORE_SUSPEND: Duration = Duration::from_secs(1);
/// The longest a wake-up is worked on before its notification is shown with what is known by then.
/// FCM gives a high-priority message about 10 s; the app hands over to expedited work at 8 s.
const PUSH_PREVIEW_BUDGET: Duration = Duration::from_secs(7);
/// The part of [`PUSH_PREVIEW_BUDGET`] for fetching and decrypting the message alone; the rest is
/// for catching the conversation up if that fails.
const PUSH_FETCH_BUDGET: Duration = Duration::from_secs(4);
/// What must be left of [`PUSH_PREVIEW_BUDGET`] to start following the groups, which can't be cut
/// short once begun.
const PUSH_FOLLOW_MIN_LEFT: Duration = Duration::from_secs(2);
/// The longest the iOS notification extension works on a wake-up it probes (issue #839): the
/// fetch, then the probe. The system gives it about 30 s, of which the wait for the app's lock may
/// take 8; past that it shows the generic text itself.
const PUSH_PROBE_BUDGET: Duration = Duration::from_secs(14);
/// The decrypted downloads an account keeps: the disk cache the apps reopen pictures and files from
/// without downloading them again. Above it the least recently used go at start and before every
/// download, as apps fetch by themselves while they run (a deleted file is fetched again when opened:
/// its transfer shows as remote). Partial downloads live elsewhere, so none of them goes.
const MAX_DOWNLOADS_BYTES: u64 = 1024 * 1024 * 1024;
/// Downloads used this recently aren't trimmed while the app runs: they may be on screen.
const RECENT_DOWNLOAD_USE: Duration = Duration::from_secs(10 * 60);
/// Plaintext copies of files waiting to be uploaded, one folder per account below it.
const UPLOAD_STAGING_DIR: &str = "outgoing";
/// Decoded messages of the conversations read last, kept so the open conversation isn't decrypted
/// again on every change (issue #155; as many as the desktop keeps). A memory-constrained process
/// (the iOS notification extension) keeps none. Bounded by bytes, not by messages (issue #1125).
const MESSAGE_CACHE_BYTES: usize = DEFAULT_MESSAGE_CACHE_BYTES;
/// A sync loop that keeps dying waits this long at most before the next one starts; the first
/// restart is immediate.
const MAX_LOOP_RESTART_DELAY: Duration = Duration::from_secs(300);
/// A sync loop that ran this long counts as healthy: the next death restarts at once again.
const LOOP_STABLE_AFTER: Duration = Duration::from_secs(300);

/// A boxed future that stays on the engine thread.
pub(crate) type LocalFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;
/// Work for the engine thread.
pub(crate) type Job = Box<dyn for<'a> FnOnce(&'a mut Engine) -> LocalFuture<'a, ()> + Send>;

pub(crate) type Api = HttpServerApi<ForeignTokens>;
pub(crate) type Store = TrackedStore<LaneStore>;
pub(crate) type CoreClient = Client<Api, Store>;

pub(crate) struct Engine {
  /// A process short of memory (the iOS notification extension): the client leaves upkeep to the app.
  is_memory_constrained: bool,
  /// Where downloaded files are decrypted to (inside the app's private storage).
  download_dir: PathBuf,
  maybe_push_gateway_url: Option<String>,
  /// Where the app copies files to upload; the core deletes each copy once it went up.
  upload_staging_dir: PathBuf,
  /// The folder of downloads older versions shared between accounts.
  shared_downloads: PathBuf,
  tokens: ForeignTokens,
  /// The engine thread's connections to the server, shared by every request made here.
  http: ServerPool,
  /// The connections of the requests on the HTTP runtime (pictures, directory), off this thread.
  side_http: Arc<ServerPool>,
  listener: Arc<dyn EventListener>,
  pub(crate) store: Store,
  pub(crate) client: ClientSlot,
  /// Who the user is, once enrolled. Everything else the backend knows the app reads on the read
  /// thread, which has its own ([`ReadQueue`]).
  maybe_backend: Option<CoreChatBackend<Store>>,
  reads: ReadQueue,
  /// Opens read connections of their own beside the writer's (the segment prefetch).
  lanes: LaneOpener,
  /// This device, once enrolled; readable while the sync loop owns the client.
  maybe_device_id: Option<DeviceId>,
  /// One push registration at a time, beside the sync loop and the jobs ([`PushRegistrar`]).
  push_turn: Rc<tokio::sync::Mutex<()>>,
  /// Sync loops that died since one last ran for [`LOOP_STABLE_AFTER`].
  loop_deaths: u32,
  maybe_loop_started: Option<tokio::time::Instant>,
  /// The enrolled identity's fingerprint (empty before enrollment).
  pub(crate) identity_fingerprint: String,
  pub(crate) maybe_recovery: Option<teamcraft_core::client::OrgRecoveryAttempt>,
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
  /// The newest ID token the app handed over.
  pub(crate) id_tokens: IdTokens,
  /// The calls running in the user's conversations (issue #1352).
  pub(crate) running_calls: RunningCalls,
}

/// Where a push's preview spent its time, for the `push opened` line: durations only.
#[derive(Clone, Copy, Debug, Default)]
struct PushTimes {
  /// Waiting for the app's access token (a refresh may ask the identity provider).
  auth_wait: Duration,
  /// Fetching and decrypting the message (a connection to the server first, after a pause).
  fetch: Duration,
  /// The app's own client catching the conversation up.
  catch_up: Duration,
  /// The notification extension's probe.
  probe: Duration,
}

/// The new device's wait in the relay ([`Engine::wait_for_link_peer`]): a task on the engine thread
/// beside the jobs, and the link it leaves once the other device answered. Dropping it ends the wait.
#[derive(Default)]
struct NewLinkWait {
  maybe_task: Option<JoinHandle<()>>,
  link: Rc<RefCell<Option<NewDeviceLink<HttpRendezvous>>>>,
}

/// Tells the engine when the sync loop's task unwinds: a panic ends only that task, and nothing else
/// would notice while the slot says `Running` (issue #1142).
struct LoopDeathNotice(Option<mpsc::WeakUnboundedSender<Job>>);

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
  /// The app's key for DPoP proofs, where it keeps one (iOS).
  maybe_device_key: Option<Arc<dyn DeviceProofKey>>,
  listener: Arc<dyn EventListener>,
  signals: SyncSignals,
  /// The process has little memory (the iOS notification extension): small store caches.
  is_memory_constrained: bool,
}

/// The queues of an opened engine.
#[derive(Clone)]
pub(crate) struct EngineThreads {
  pub(crate) jobs: mpsc::UnboundedSender<Job>,
  pub(crate) reads: ReadQueue,
  /// The server for requests that run on the HTTP runtime and need nothing of the engine.
  pub(crate) http: Arc<ServerPool>,
}

/// Told when the client finds the store unreadable after the engine started: why, and what opens the
/// engine again after a reset (issue #414).
pub(crate) type LateProblem = Box<dyn FnOnce(StoreProblem, EngineStart, Box<dyn FnOnce()>) + Send>;

/// Why the engine didn't open.
#[derive(Debug)]
pub(crate) enum OpenError {
  /// The local store can't be read by this version, now or after a restart (issue #414): the user
  /// decides what happens to it.
  Unreadable(StoreProblem),
  Failed(TeamCraftError),
}

/// Where the client is: not enrolled yet, held by the engine, or running in the sync loop.
/// `Missing` on an enrolled device repairs itself ([`Engine::repair_client`]), and `Running` may name
/// a loop that died ([`Engine::collect_dead_loop`]): neither takes a restart of the app (issue #1142).
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

/// What a background run of the sync loop waits for ([`Engine::begin_drain`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DrainGoal {
  /// Nothing waits for the server any more (issue #158).
  PendingWork,
  /// A connection is up and caught up (notifications.md, "Sync wake-ups").
  CaughtUp,
}

/// Whether a push payload is opened as it arrives or again later to update its notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PushOpening {
  /// The push service delivered it now.
  Received,
  /// The app opens it again because the first try could not read the message in time.
  Retry,
  /// A later look, minutes on, with time to spare: it asks the server how far the user has read.
  /// Not for a try with a tight bound ([`Self::Retry`]): the answer costs up to
  /// [`PUSH_FETCH_BUDGET`], and a text already read must not be lost to that wait.
  LateLook,
  /// The app looks whether the sync stored the message meanwhile: no network, nothing that waits.
  StoreOnly,
}

impl PushOpening {
  /// Whether this opening asks the server for the read position: only a late look of the app's own
  /// client, while it isn't syncing (the gateway keeps it fresh then).
  fn refreshes_read_position(self, reader: PushReader, is_client_idle: bool) -> bool {
    self == Self::LateLook && reader == PushReader::App && is_client_idle
  }

  /// Whether this opening asks the server how a ring ended (issue #1257): only a late look of the
  /// app at a ring. Unlike the read position, also while the sync runs: the gateway doesn't replay
  /// a ring that ended.
  fn asks_how_a_ring_ended(self, reader: PushReader, kind: &PushKind) -> bool {
    self == Self::LateLook && reader == PushReader::App && *kind == PushKind::Call
  }
}

/// Who opens a push: the app, whose client may catch the conversation up, or a notification
/// extension next to the app (iOS), which must not advance the account's state: two processes
/// would each advance their own copy of the MLS state (e2ee.md §7.2, issue #424 review H2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PushReader {
  App,
  Extension,
  /// A notification extension with memory to spare: when the preview couldn't read the message it
  /// catches the conversation up on a throwaway copy of the store through a read-only API, which
  /// advances nothing either (issue #839, [`teamcraft_core::push_probe`]).
  ProbingExtension,
}

impl PushReader {
  /// Whether the client catches the push's conversation up (welcomes, MLS backlog, messages)
  /// when the preview couldn't read the message: only the app's own, and only while it isn't
  /// syncing (the sync loop brings the message itself).
  fn catches_up(self, needs_catch_up: bool, is_client_idle: bool) -> bool {
    self == Self::App && needs_catch_up && is_client_idle
  }

  /// Whether the conversation is caught up on a probe instead (an extension that may).
  fn probes(self, needs_catch_up: bool) -> bool {
    self == Self::ProbingExtension && needs_catch_up
  }
}

/// Whether a push asks the server for its message, after the store's preview.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PushFetch {
  /// The store's preview is what a fetch would show too.
  NotNeeded,
  /// The sync loop runs and brings the message itself; the app fills the notification in from the
  /// store then (issue #1249), and the push keeps the engine for milliseconds, not seconds.
  LeftToSync,
  Fetch,
}

impl PushFetch {
  fn of(needs_server: bool, is_sync_running: bool) -> Self {
    match (needs_server, is_sync_running) {
      (false, _) => Self::NotNeeded,
      (true, true) => Self::LeftToSync,
      (true, false) => Self::Fetch,
    }
  }
}

/// What came of asking the app for an access token for a push.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PushAccess {
  /// Nothing to fetch: not asked.
  NotAsked,
  Supplied,
  /// The app had none: signed out, or a refresh that failed for now (offline, Doze). Either way
  /// more requests would fail; only the first is a sign-out, which the app handles itself.
  Missing,
  /// The app's refresh took longer than the fetch's time.
  TimedOut,
}

impl PushAccess {
  /// Whether more requests would only fail or wait for the same refresh again.
  fn is_missing(self) -> bool {
    matches!(self, Self::Missing | Self::TimedOut)
  }
}

/// A preview whose message couldn't be read in time: a missing message is the timeout's fault; a
/// policy or setting cut stays as it is.
fn timed_out(mut preview: PushPreview) -> PushPreview {
  if matches!(
    preview.maybe_gap,
    Some(PreviewGap::NoSession | PreviewGap::NotDecrypted)
  ) {
    preview.maybe_gap = Some(PreviewGap::TimedOut);
  }
  preview
}

/// The newest ID token of the session, as the app hands it over ([`crate::TeamCraft::note_id_token`]),
/// and whether the core would like a newer one. Apart from [`TokenProvider`], which the apps implement,
/// so an app that doesn't hand one over yet keeps building and simply signs no roles (e2ee.md §3.1.3).
#[derive(Clone, Default)]
pub(crate) struct IdTokens {
  maybe_token: Arc<Mutex<Option<String>>>,
  is_new_one_wanted: Arc<AtomicBool>,
}

impl IdTokens {
  pub(crate) fn note(&self, maybe_token: Option<String>) {
    // Plain data: a panic elsewhere while it was locked leaves nothing half-changed.
    *self.maybe_token.lock().unwrap_or_else(PoisonError::into_inner) = maybe_token;
  }

  fn current(&self) -> Option<String> {
    self
      .maybe_token
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clone()
  }

  fn ask_for_new_one(&self) {
    self.is_new_one_wanted.store(true, Ordering::Relaxed);
  }

  /// Whether the core asked for a newer token since the last time this was called.
  pub(crate) fn take_request(&self) -> bool {
    self.is_new_one_wanted.swap(false, Ordering::Relaxed)
  }
}

/// The app's [`TokenProvider`] as the core's token source.
#[derive(Clone)]
pub(crate) struct ForeignTokens {
  provider: Arc<dyn TokenProvider>,
  id_tokens: IdTokens,
  /// The thread that asks the app for a token where a request shouldn't wait on its own thread.
  asks: std::sync::mpsc::Sender<oneshot::Sender<Result<String, ApiError>>>,
  /// What signs for bound tokens; `None` on a platform that keeps no key.
  maybe_key_proofs: Option<DeviceKeyProofs>,
}

/// [`CoreClient::catch_up_for_push`] within `deadline`, cutting only the cancel-safe steps short.
/// Following the groups (welcomes, MLS) is never dropped halfway: it starts only with
/// [`PUSH_FOLLOW_MIN_LEFT`] to spare and then runs to completion, bounded by its requests' own
/// timeouts; a wake-up that outlasts the receiver's budget continues as expedited work.
async fn catch_up_for_push(
  client: &mut CoreClient,
  conversation: ConversationId,
  deadline: tokio::time::Instant,
) -> Result<(), TeamCraftError> {
  let _step = teamcraft_core::timing::step("push_catch_up");
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

/// `preview` with the names `later` learned where it had none.
fn with_names_of(mut preview: PushPreview, later: PushPreview) -> PushPreview {
  if preview.maybe_sender_name.is_none() {
    preview.is_sender_source = later.is_sender_source;
  }
  preview.maybe_sender_name = preview.maybe_sender_name.or(later.maybe_sender_name);
  preview.maybe_conversation_name = preview.maybe_conversation_name.or(later.maybe_conversation_name);
  preview
}

/// Whether the preview read the message, or knows why it shows none (a setting, a reaction).
fn is_readable(preview: &PushPreview) -> bool {
  !matches!(
    preview.maybe_gap,
    Some(PreviewGap::NoSession | PreviewGap::NotDecrypted | PreviewGap::TimedOut)
  )
}

/// What kind of error it was, for a log line that is exported: never its text.
fn error_class(error: &TeamCraftError) -> &'static str {
  match error {
    TeamCraftError::Network(_) => "network",
    TeamCraftError::SignedOut => "signed_out",
    TeamCraftError::NotEnrolled => "not_enrolled",
    TeamCraftError::Storage(_) => "storage",
    TeamCraftError::Core(_) => "core",
    _ => "other",
  }
}

/// Starts the engine thread for `start` and waits until the store is open and readable: the client
/// (the MLS state, which grows with the groups) opens after that, ahead of the first job, so what
/// the store holds can be read at once. A process short of memory (the iOS notification extension)
/// has no reads to hurry and waits for the client too. Should the client then find the store
/// unreadable, `late_problem` is told and the engine ends.
pub(crate) fn spawn(start: &EngineStart, late_problem: LateProblem) -> Result<EngineThreads, OpenError> {
  let start = start.duplicate();
  let waits_for_client = start.is_memory_constrained;
  let recovery_start = start.duplicate();
  let (ready, opened) = std::sync::mpsc::channel();
  let (jobs, mut queue) = mpsc::unbounded_channel::<Job>();
  let own_jobs = jobs.downgrade();
  std::thread::Builder::new()
    .name("teamcraft-engine".into())
    .stack_size(THREAD_STACK_BYTES)
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
        let mut engine = match reporting_panics(|| Engine::open(start, own_jobs.clone())) {
          Ok(engine) => engine,
          Err(error) => {
            let _ = ready.send(Err(error));
            return;
          },
        };
        engine.maybe_jobs = Some(own_jobs);
        if waits_for_client {
          if let Err(error) = reporting_panics(|| engine.open_client()) {
            let _ = ready.send(Err(error));
            return;
          }
          drop(recovery_start);
          let _ = ready.send(Ok((engine.reads.clone(), engine.side_http.clone())));
        } else {
          let _ = ready.send(Ok((engine.reads.clone(), engine.side_http.clone())));
          if let Err(problem) = engine.finish_opening() {
            // The store closes first: a reset sets its files aside, and a connection closing after
            // that would delete the log of the new database, which has the old one's name.
            let listener = engine.listener.clone();
            let close_store = Box::new(move || {
              engine.reads.close();
              drop(engine);
            });
            late_problem(problem, recovery_start, close_store);
            listener.on_change();
            return;
          }
          drop(recovery_start);
        }
        while let Some(job) = queue.recv().await {
          job(&mut engine).await;
        }
        engine.stop_runner().await;
      });
    })
    .map_err(|error| OpenError::Failed(TeamCraftError::Core(error.to_string())))?;
  let (reads, http) = opened
    .recv()
    .map_err(|_| OpenError::Failed(TeamCraftError::EngineStopped))??;
  Ok(EngineThreads { jobs, reads, http })
}

/// A step of opening the engine, with a panic turned into an error that says where it happened.
fn reporting_panics<T>(open: impl FnOnce() -> Result<T, OpenError>) -> Result<T, OpenError> {
  let report = Arc::new(Mutex::new(String::new()));
  let previous_hook = Arc::new(std::panic::take_hook());
  let own_hook_previous = previous_hook.clone();
  let hook_report = report.clone();
  let opening_thread = std::thread::current().id();
  // The hook is global and the client opens while the read thread already answers: a panic of any
  // other thread goes where it always went.
  std::panic::set_hook(Box::new(move |info| {
    if std::thread::current().id() != opening_thread {
      own_hook_previous(info);
    } else if let Ok(mut report) = hook_report.lock() {
      *report = info.to_string();
    }
  }));
  let opened = std::panic::catch_unwind(AssertUnwindSafe(open));
  std::panic::set_hook(Box::new(move |info| previous_hook(info)));
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
    maybe_device_key: Option<Arc<dyn DeviceProofKey>>,
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
      maybe_device_key,
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
      maybe_device_key: self.maybe_device_key.clone(),
      listener: self.listener.clone(),
      is_memory_constrained: self.is_memory_constrained,
      signals: self.signals.clone(),
    }
  }
}

impl Engine {
  /// Opens the store and the read thread on it. Records that don't decode, or a key bag its own key
  /// doesn't open, make the store unreadable here already.
  fn open(start: EngineStart, own_jobs: mpsc::WeakUnboundedSender<Job>) -> Result<Self, OpenError> {
    let _step = teamcraft_core::timing::step("engine_open_store");
    std::fs::create_dir_all(&start.data_dir)
      .map_err(|error| OpenError::Failed(TeamCraftError::Storage(error.to_string())))?;
    let [download_dir, upload_staging_dir] = start.plaintext_dirs();
    let shared_downloads = start.data_dir.join(DOWNLOADS_DIR);
    let path = start.store_path();
    let EngineStart {
      server_url,
      maybe_push_gateway_url,
      key,
      tokens,
      maybe_device_key,
      listener,
      signals,
      is_memory_constrained,
      ..
    } = start;
    let key_source_key = key.duplicate();
    let key_source = StaticKeySource::new(key);
    let (store, cached_bytes) = if is_memory_constrained {
      // Never the catalog's build: a first push after an update would run it under the limit.
      let limits = SealedStoreLimits::minimal();
      (
        SealedSqliteStore::open_without_building(&path, &key_source, limits),
        0,
      )
    } else {
      (SealedSqliteStore::open(&path, &key_source), MESSAGE_CACHE_BYTES)
    };
    let store = Arc::new(store.map_err(|error| open_error(error.into()))?);
    let journal = CommitJournal::new();
    if !is_memory_constrained {
      store.publish_commits(Arc::clone(&journal));
    }
    // The apps keep drafts of conversations, which go with them (#1161).
    let engine_store = TrackedStore::with_message_cache(LaneStore::writer(Arc::clone(&store)), cached_bytes)
      .with_conversation_purges();
    let lanes = LaneOpener {
      engine: engine_store.clone(),
      writer: Arc::clone(&store),
      path,
      key: key_source_key,
      journal,
      is_memory_constrained,
    };
    let read_lanes = lanes.read_lanes();
    let store = engine_store;
    // A broken record only costs the preferences, not the store.
    let mentions: MentionSettings = load_mention_preferences(&store).unwrap_or_default().into();
    let maybe_account = records::load_account(&store).map_err(open_error)?;
    let reads = ReadQueue::start(
      read_lanes,
      listener.clone(),
      signals.catch_up_focus.clone(),
      own_jobs,
    )
    .map_err(OpenError::Failed)?;
    let tokens =
      ForeignTokens::new(tokens, signals.id_tokens.clone(), maybe_device_key).map_err(OpenError::Failed)?;
    let http = ServerPool::new(
      HttpServerApi::new(server_url.clone(), tokens.clone()),
      &signals.network,
    );
    let side_http = Arc::new(ServerPool::new(
      HttpServerApi::new(server_url, tokens.clone()),
      &signals.network,
    ));
    let mut engine = Self {
      is_memory_constrained,
      maybe_push_gateway_url,
      download_dir,
      upload_staging_dir,
      tokens,
      http,
      side_http,
      listener,
      store,
      client: ClientSlot::Missing,
      maybe_backend: None,
      reads,
      lanes,
      shared_downloads,
      maybe_device_id: None,
      push_turn: Rc::default(),
      loop_deaths: 0,
      maybe_loop_started: None,
      identity_fingerprint: String::new(),
      maybe_recovery: None,
      maybe_link_offer: None,
      maybe_link_announcement: None,
      link_requests: LinkRequestList::default(),
      new_link: NewLinkWait::default(),
      maybe_approval: None,
      maybe_receipt: None,
      calls: CallHub::with_running(signals.running_calls.clone()),
      mentions,
      signals,
      is_syncing_for_app: false,
      active_drains: 0,
      maybe_jobs: None,
    };
    if let Some(account) = maybe_account {
      engine.know_account(&account);
    }
    Ok(engine)
  }

  /// What opening leaves for after the app can read: the upkeep of the plaintext folders and the
  /// client. Only a store the client can't read stops the engine; any other failure leaves the slot
  /// empty on an enrolled device, which repairs itself on the next use (issue #1142).
  fn finish_opening(&mut self) -> Result<(), StoreProblem> {
    match reporting_panics(|| self.open_client()) {
      Ok(()) => Ok(()),
      Err(OpenError::Unreadable(problem)) => Err(problem),
      Err(OpenError::Failed(error)) => {
        tracing::warn!(%error, "the client did not open; trying again on its next use");
        Ok(())
      },
    }
  }

  /// Makes room before a download: the folder stays at its limit while the app runs. Files used
  /// lately (on screen, in a viewer: the apps bump their time) stay; the downloads whose files went
  /// show as remote at once.
  pub(crate) fn trim_downloads_before_fetch(&self) {
    let deleted = trim_downloads(&self.download_dir, MAX_DOWNLOADS_BYTES, RECENT_DOWNLOAD_USE);
    if deleted.is_empty() {
      return;
    }
    match teamcraft_core::client::files::forget_deleted_downloads(&self.store, &deleted) {
      Ok(_) => self.listener.on_change(),
      // The next start trims and the apps treat a missing file as remote; only slower.
      Err(error) => tracing::debug!(%error, "trimmed downloads could not be forgotten"),
    }
  }

  fn open_client(&mut self) -> Result<(), OpenError> {
    let _step = teamcraft_core::timing::step("open_client");
    remove_shared_downloads(&self.shared_downloads);
    // Plaintext copies of files that were never sent (taken out of the composer, app stopped).
    if let Err(error) = sweep_staged_copies(&self.store, &self.upload_staging_dir) {
      tracing::debug!(%error, "stray staged upload copies could not be listed");
    }
    trim_downloads(&self.download_dir, MAX_DOWNLOADS_BYTES, Duration::ZERO);
    let maybe_client = Client::open(
      self.api(),
      self.store.clone(),
      Box::new(SystemClock),
      self.client_settings(),
    )
    .map_err(open_error)?;
    if let Some(client) = maybe_client {
      self.adopt(client);
    }
    Ok(())
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
    // An empty slot alone doesn't say so: it may be a client that is to be reopened (issue #1142).
    if !matches!(self.client, ClientSlot::Missing) || self.maybe_backend.is_some() {
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

  pub(crate) async fn enroll_with_recovery_key(
    &mut self,
    device_name: String,
    recovery_key: RecoveryKey,
  ) -> Result<AccountState, TeamCraftError> {
    let options = enrollment_options(device_name, EnrollmentSecret::RecoveryKey(recovery_key));
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
    self.collect_dead_loop().await;
    self.repair_client()?;
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
    let notice = LoopDeathNotice(self.maybe_jobs.clone());
    let task = tokio::task::spawn_local(async move {
      let _notice = notice;
      runner.run().await
    });
    self.client = ClientSlot::Running { handle, task };
    self.maybe_loop_started = Some(tokio::time::Instant::now());
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

  /// Runs the sync loop until `goal` is reached or `max_wait` has passed, then answers with what is
  /// still pending. A loop the app syncs with keeps running; otherwise the loop stops at the end. The
  /// end runs on the engine thread whatever the caller does, so an OS that cancels the caller's
  /// background work leaves no loop running.
  pub(crate) async fn begin_drain(
    &mut self,
    max_wait: Duration,
    goal: DrainGoal,
  ) -> Result<oneshot::Receiver<Result<PendingWork, TeamCraftError>>, TeamCraftError> {
    self.start().await?;
    self.active_drains += 1;
    let (drained, is_drained) = futures_channel::oneshot::channel();
    if let ClientSlot::Running { handle, .. } = &self.client {
      let command = match goal {
        DrainGoal::PendingWork => RunnerCommand::Drain(drained),
        DrainGoal::CaughtUp => RunnerCommand::CatchUp(drained),
      };
      // Refused only by a loop that ended abnormally: the wait ends at once then.
      handle.send(command);
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

  fn pending_work(&self) -> Result<PendingWork, TeamCraftError> {
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
    // The purge runs on a thread of its own (#1684): give it the moment before the OS may suspend
    // the app. One that a read holds up finishes after the resume, or when the store opens again.
    if !self.store.wait_for_purge(PURGE_BEFORE_SUSPEND) {
      tracing::debug!("the local database log is purged after the app resumes");
    }
    Ok(())
  }

  /// Registers the token beside the sync loop; the receiver gets the outcome.
  pub(crate) fn register_push(
    &self,
    service: PushService,
    token: String,
  ) -> Result<oneshot::Receiver<Result<(), TeamCraftError>>, TeamCraftError> {
    self.beside_the_loop(move |registrar| {
      Box::pin(async move { registrar.register_keeping_key(service.into(), &token).await })
    })
  }

  /// Seals the registered device token again when that is due, beside the sync loop; the receiver
  /// gets whether it was renewed.
  pub(crate) fn refresh_push(
    &self,
  ) -> Result<oneshot::Receiver<Result<bool, TeamCraftError>>, TeamCraftError> {
    self.beside_the_loop(|registrar| Box::pin(async move { registrar.refresh().await }))
  }

  pub(crate) fn unregister_push(
    &self,
  ) -> Result<oneshot::Receiver<Result<(), TeamCraftError>>, TeamCraftError> {
    self.beside_the_loop(|registrar| Box::pin(async move { registrar.unregister().await }))
  }

  /// Runs push registration work on the engine thread beside the sync loop and the jobs: it needs
  /// nothing of the sync state, and stopping the loop for it cut the gateway connection on every
  /// start and return to the foreground (issue #1142). A slow push gateway holds up nothing else.
  fn beside_the_loop<T, F>(
    &self,
    work: F,
  ) -> Result<oneshot::Receiver<Result<T, TeamCraftError>>, TeamCraftError>
  where
    T: 'static,
    F: for<'a> FnOnce(PushRegistrar<'a, Api, Store>) -> LocalFuture<'a, Result<T, CoreError>> + 'static,
  {
    let device_id = self.maybe_device_id.ok_or(TeamCraftError::NotEnrolled)?;
    let (api, store, settings) = (self.api(), self.store.clone(), self.client_settings());
    let turn = self.push_turn.clone();
    let (answer, answered) = oneshot::channel();
    tokio::task::spawn_local(async move {
      let _turn = turn.lock().await;
      let registrar = PushRegistrar::new(&api, &store, &SystemClock, &settings, device_id);
      // The app may have stopped waiting (a cancelled coroutine); nobody is left to tell then.
      let _ = answer.send(work(registrar).await.map_err(TeamCraftError::from));
    });
    Ok(answered)
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
    // A plain request: no reason to stop the sync loop, least of all during a call.
    let device_id = self.maybe_device_id.ok_or(TeamCraftError::NotEnrolled)?;
    let request = CreateCallJoinTokenRequest { device_id };
    let token = self.api().create_call_join_token(conversation, &request).await;
    Ok(token.map_err(CoreError::from)?)
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
    let _step = teamcraft_core::timing::step("push_notification");
    let opened_at = std::time::Instant::now();
    let mut times = PushTimes::default();
    let preview = if opening == PushOpening::StoreOnly {
      push_preview::<Api>(&self.store, None, &payload).await?
    } else {
      self
        .push_preview_within_budget(&payload, reader, &mut times)
        .await?
    };
    // Why a notification said less, and how long it took: IDs and reasons only (security.md), so
    // an exported diagnostics log shows how often each path is taken.
    if opening != PushOpening::StoreOnly {
      tracing::info!(
        target: LIFECYCLE,
        ?opening,
        gap = ?preview.maybe_gap,
        ran_ms = opened_at.elapsed().as_millis(),
        auth_wait_ms = times.auth_wait.as_millis(),
        fetch_ms = times.fetch.as_millis(),
        catch_up_ms = times.catch_up.as_millis(),
        probe_ms = times.probe.as_millis(),
        is_client_idle = matches!(self.client, ClientSlot::Idle(_)),
        "push opened"
      );
    }
    // A later look asks the server how far this user has read: the sync doesn't run in the
    // background, and a `dismiss` may come after this. Best effort; the store's position else.
    let is_client_idle = matches!(self.client, ClientSlot::Idle(_));
    if opening.refreshes_read_position(reader, is_client_idle)
      && let ClientSlot::Idle(client) = &mut self.client
    {
      let refreshed = tokio::time::timeout(
        PUSH_FETCH_BUDGET,
        client.push_refresh_conversation(preview.wake_up.conversation_id),
      )
      .await;
      if !matches!(refreshed, Ok(Ok(()))) {
        tracing::debug!("read position not refreshed for a push");
      }
    }
    let is_ring_answered = self.is_ring_answered(&preview.wake_up, opening, reader).await;
    let is_read = is_read(&self.store, &preview.wake_up);
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
    notification.is_read = is_read;
    notification.maybe_suppression = maybe_suppression.map(Into::into);
    notification.is_ring_answered = is_ring_answered;
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

  /// Whether a ring looked at late was answered on another device, as the server's call history says
  /// (issue #1257). Only a late look of the app asks: a ring opened as it arrives rings, and the
  /// answer costs up to [`PUSH_FETCH_BUDGET`]. Best effort: `false`, a missed call, when the server
  /// doesn't answer in time.
  async fn is_ring_answered(&self, wake_up: &PushWakeUp, opening: PushOpening, reader: PushReader) -> bool {
    let Some(call) = wake_up.maybe_call_id else {
      return false;
    };
    if !opening.asks_how_a_ring_ended(reader, &wake_up.kind) {
      return false;
    }
    let api = self.api();
    let answered = tokio::time::timeout(PUSH_FETCH_BUDGET, push_guard::is_ring_answered(&api, call)).await;
    if !matches!(answered, Ok(Ok(_))) {
      tracing::debug!("call history not read for a late ring");
    }
    matches!(answered, Ok(Ok(true)))
  }

  /// The preview of a wake-up within [`PUSH_PREVIEW_BUDGET`], so the push service's time for the
  /// app (FCM: about 10 s) is never spent on a slow network: a fetch that takes longer, or a catch-up
  /// that doesn't finish, leaves a notification that names the sender instead of none.
  async fn push_preview_within_budget(
    &mut self,
    payload: &[u8],
    reader: PushReader,
    times: &mut PushTimes,
  ) -> Result<PushPreview, TeamCraftError> {
    let started = tokio::time::Instant::now();
    let deadline = started + PUSH_PREVIEW_BUDGET;
    // The store first: a message the sync already brought needs no token and no connection, which
    // after a pause costs the radio's wake-up, DNS, TCP and TLS (issue #1662).
    let stored = push_preview::<Api>(&self.store, None, payload).await?;
    let is_sync_running = matches!(self.client, ClientSlot::Running { .. });
    let (mut preview, access) = match PushFetch::of(stored.needs_server(), is_sync_running) {
      PushFetch::NotNeeded => (stored, PushAccess::NotAsked),
      PushFetch::LeftToSync => {
        tracing::info!(target: LIFECYCLE, "push left to the running sync");
        (stored, PushAccess::NotAsked)
      },
      PushFetch::Fetch => {
        self
          .fetch_for_push(payload, stored, started + PUSH_FETCH_BUDGET, times)
          .await?
      },
    };
    // The app's own client may catch the conversation up: a conversation this device was just
    // added to, or keys that changed while the app was closed. Not while the sync loop runs: it
    // brings the message itself, and taking the client would cut its connection. Never in a
    // notification extension ([`PushReader::Extension`]). Not without a token either: every
    // request would ask the app for one again and fail, which is no sign-out.
    let is_client_idle = matches!(self.client, ClientSlot::Idle(_));
    if reader.catches_up(preview.needs_catch_up(), is_client_idle)
      && !access.is_missing()
      && let ClientSlot::Idle(client) = &mut self.client
    {
      let conversation = preview.wake_up.conversation_id;
      let catch_up_started = tokio::time::Instant::now();
      let caught = catch_up_for_push(client, conversation, deadline).await;
      times.catch_up = catch_up_started.elapsed();
      if let Err(error) = caught {
        // The class only: an error's text can carry what a server said.
        let cause = error_class(&error);
        tracing::info!(target: LIFECYCLE, cause, conversation_id = %conversation, "push catch-up stopped");
      }
      let caught_up = push_preview::<Api>(&self.store, None, payload).await?;
      // The store-only preview names `NoSession` while the message is still missing: then the
      // first try's reason stays (a failed decryption, a timeout), with any names the catch-up
      // learned.
      if caught_up.maybe_gap != Some(PreviewGap::NoSession) {
        preview = caught_up;
      } else {
        preview = with_names_of(preview, caught_up);
      }
    }
    if reader.probes(preview.needs_catch_up()) && !access.is_missing() {
      let probe_started = tokio::time::Instant::now();
      preview = self
        .probe_for_push(payload, started + PUSH_PROBE_BUDGET, preview)
        .await;
      times.probe = probe_started.elapsed();
    }
    Ok(preview)
  }

  /// The preview with the message fetched from the server, within `fetch_deadline` for the token
  /// and the fetch together: a slow token refresh leaves less time for the fetch instead of adding
  /// to it, so a push never holds the engine longer than [`PUSH_FETCH_BUDGET`] here. Without a
  /// token in time `stored` stays, the store's preview, and the app tries again later.
  async fn fetch_for_push(
    &self,
    payload: &[u8],
    stored: PushPreview,
    fetch_deadline: tokio::time::Instant,
    times: &mut PushTimes,
  ) -> Result<(PushPreview, PushAccess), TeamCraftError> {
    let auth_started = tokio::time::Instant::now();
    let access = match tokio::time::timeout_at(fetch_deadline, self.tokens.fresh_token()).await {
      Ok(Ok(_)) => PushAccess::Supplied,
      Ok(Err(_)) => PushAccess::Missing,
      Err(_elapsed) => PushAccess::TimedOut,
    };
    times.auth_wait = auth_started.elapsed();
    if access.is_missing() {
      // Either way the store's preview stays: a refresh that failed for now (offline, Doze, the
      // identity provider slow) is retried later, and the app signs out itself when its session ends.
      tracing::info!(target: LIFECYCLE, ?access, "no access token for the push");
      let preview = if access == PushAccess::TimedOut {
        timed_out(stored)
      } else {
        stored
      };
      return Ok((preview, access));
    }
    let api = self.api();
    // Cancel-safe: the preview only reads the store.
    let fetch_started = tokio::time::Instant::now();
    let fetched =
      tokio::time::timeout_at(fetch_deadline, push_preview(&self.store, Some(&api), payload)).await;
    times.fetch = fetch_started.elapsed();
    let preview = match fetched {
      Ok(result) => result?,
      Err(_elapsed) => timed_out(stored),
    };
    Ok((preview, access))
  }

  /// The preview of a wake-up the extension couldn't read, after catching its conversation up on a
  /// probe (issue #839): an overlay over the store that is dropped afterwards, and the server
  /// through a read-only API. Nothing is saved and nothing is sent; the app processes the same
  /// welcomes and commits later. Within `deadline`, cut short anywhere (all of it is thrown away),
  /// after which the overlay still shows what was learned. `preview` stays, with any names learned,
  /// when the probe didn't make the message readable.
  async fn probe_for_push(
    &mut self,
    payload: &[u8],
    deadline: tokio::time::Instant,
    preview: PushPreview,
  ) -> PushPreview {
    let _step = teamcraft_core::timing::step("push_probe");
    // One copy of the MLS state in memory at a time: the extension's own client goes before the
    // probe's opens (it has nothing to do in the extension; the next use opens it again).
    if matches!(self.client, ClientSlot::Idle(_)) {
      self.client = ClientSlot::Missing;
    }
    let started = tokio::time::Instant::now();
    let overlay = OverlayStore::new(&self.store, PROBE_OVERLAY_BYTES);
    let probe = probe_push(
      self.api(),
      &overlay,
      Box::new(SystemClock),
      self.client_settings(),
      payload,
    );
    let probed = match tokio::time::timeout_at(deadline, probe).await {
      Ok(Ok(Some((probed, report)))) => {
        tracing::info!(
          target: LIFECYCLE,
          refused_calls = report.refused_calls,
          overlay_kib = report.overlay_bytes / 1024,
          read_messages = report.read_messages,
          ran_ms = started.elapsed().as_millis(),
          "push probed"
        );
        probed
      },
      Ok(Ok(None)) => return preview,
      Ok(Err(error)) => {
        // The class only: an error's text can carry what a server said.
        let cause = error_class(&TeamCraftError::from(error));
        tracing::info!(target: LIFECYCLE, cause, "push probe failed");
        return preview;
      },
      Err(_elapsed) => {
        tracing::info!(target: LIFECYCLE, overlay_kib = overlay.overlay_bytes() / 1024, "push probe timed out");
        match push_preview::<Api>(&overlay, None, payload).await {
          Ok(probed) => probed,
          Err(_) => return preview,
        }
      },
    };
    if is_readable(&probed) {
      probed
    } else {
      with_names_of(preview, probed)
    }
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

  pub(crate) fn set_notification_content(&self, content: NotificationContent) -> Result<(), TeamCraftError> {
    Ok(save_device_push_content(&self.store, &content.into())?)
  }

  pub(crate) fn set_segment_prefetch(&self, mode: SegmentPrefetchMode) -> Result<(), TeamCraftError> {
    Ok(save_segment_prefetch(&self.store, mode.into())?)
  }

  /// Starts the search segment prefetch if the saved mode allows it on this network; nothing is
  /// fetched otherwise.
  pub(crate) fn begin_prefetch_segments(
    &mut self,
    is_network_metered: bool,
    max_total_bytes: u64,
  ) -> Result<SegmentPrefetchRun, TeamCraftError> {
    if !load_segment_prefetch(&self.store)?.allows(is_network_metered) {
      return Ok(SegmentPrefetchRun::done(SegmentPrefetchResult {
        fetched: 0,
        skipped: 0,
        stored_bytes: 0,
      }));
    }
    self.begin_prefetch_search_segments(max_total_bytes)
  }

  /// Starts downloading the missing search segments, newest first, within `max_total_bytes`, as a
  /// task beside the jobs (issue #1634): the sync loop keeps running and the jobs don't wait. Its
  /// look through the store reads on a connection of its own, off this thread; a download holds no
  /// lock, and each segment is one short write. Dropping the run stops it.
  pub(crate) fn begin_prefetch_search_segments(
    &mut self,
    max_total_bytes: u64,
  ) -> Result<SegmentPrefetchRun, TeamCraftError> {
    if self.maybe_backend.is_none() {
      return Err(TeamCraftError::NotEnrolled);
    }
    let store = self.lanes.open(SEGMENT_PLAN_LIMITS);
    let api = self.api();
    let (reply, answer) = oneshot::channel();
    let task = tokio::task::spawn_local(async move {
      let result = prefetch_segments_beside(api, store, max_total_bytes).await;
      // The caller may have stopped waiting; the task ends anyway.
      let _ = reply.send(result);
    });
    Ok(SegmentPrefetchRun {
      answer,
      _stop: AbortOnDrop(Some(task.abort_handle())),
    })
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
    self.set_account(
      client.user_id(),
      client.device_id(),
      client.identity_fingerprint(),
    );
    self
      .reads
      .follow_client(client.typing_state(), client.transfers());
    self.client = ClientSlot::Idle(Box::new(client));
  }

  /// The account the store holds, before its client is open: the app can read already.
  fn know_account(&mut self, account: &records::Account) {
    let fingerprint = account.identity_public_key().fingerprint().to_string();
    self.set_account(account.user_id, account.device_id, fingerprint);
  }

  fn set_account(&mut self, user: UserId, device: DeviceId, fingerprint: String) {
    self.identity_fingerprint = fingerprint.clone();
    self.maybe_device_id = Some(device);
    self.maybe_backend = Some(
      CoreChatBackend::new(self.store.clone(), user, Box::new(SystemClock))
        .with_entry_need(EntryNeed::Unread),
    );
    self.reads.know_account(user, fingerprint);
  }

  /// Gateway call events (rings) of `client` go to the app's call listener.
  fn attach_call_sink(&self, client: &mut CoreClient) {
    let calls = self.calls.clone();
    let listener = self.listener.clone();
    client.set_call_event_sink(Box::new(move |event| {
      // A call started or ended where nothing was stored: the lists read the running calls again.
      if calls.on_gateway_event(event) {
        listener.on_change();
      }
    }));
  }

  /// Stops the sync loop if it runs and hands out the client; put it back into `self.client`.
  async fn take_client(&mut self) -> Result<Box<CoreClient>, TeamCraftError> {
    self.stop_runner().await;
    self.repair_client()?;
    match std::mem::replace(&mut self.client, ClientSlot::Missing) {
      ClientSlot::Idle(client) => Ok(client),
      _ => Err(TeamCraftError::NotEnrolled),
    }
  }

  /// Stops the sync loop if it runs; an idle client stays where it is.
  async fn stop_runner(&mut self) {
    match std::mem::replace(&mut self.client, ClientSlot::Missing) {
      ClientSlot::Running { handle, task } => {
        handle.send(RunnerCommand::Stop);
        match task.await {
          Ok(client) => self.client = ClientSlot::Idle(Box::new(client)),
          // Only a panic in the sync loop gets here (the panic log names it, logging.rs); reopen
          // the client from the store.
          Err(error) => {
            tracing::error!(%error, "sync loop ended abnormally");
            // Said by `reopen_client`; the next start tries again.
            let _ = self.reopen_client();
          },
        }
      },
      other => self.client = other,
    }
  }

  /// An enrolled device whose slot is empty gets its client back from the store.
  fn repair_client(&mut self) -> Result<(), TeamCraftError> {
    if !matches!(self.client, ClientSlot::Missing) || self.maybe_backend.is_none() {
      return Ok(());
    }
    tracing::warn!("no client on an enrolled device; reopening it from the store");
    self.reopen_client()
  }

  fn reopen_client(&mut self) -> Result<(), TeamCraftError> {
    let opened = Client::open(
      self.api(),
      self.store.clone(),
      Box::new(SystemClock),
      self.client_settings(),
    );
    match opened {
      Ok(Some(client)) => {
        // Everything hanging off the old client (typing, transfers, the call sink) moves over.
        self.adopt(client);
        Ok(())
      },
      Ok(None) => {
        tracing::error!("could not reopen the client: the store holds no account");
        Err(TeamCraftError::NotEnrolled)
      },
      Err(error) => {
        tracing::error!(%error, "could not reopen the client from the store");
        Err(error.into())
      },
    }
  }

  /// A loop that died leaves the slot saying `Running`: collects it, so the client comes back.
  async fn collect_dead_loop(&mut self) {
    if matches!(&self.client, ClientSlot::Running { task, .. } if task.is_finished()) {
      self.stop_runner().await;
    }
  }

  /// The sync loop panicked ([`LoopDeathNotice`]): brings the client back and starts a new loop,
  /// at once the first time, then after a growing pause, so a panic that repeats doesn't spin. The
  /// app's next start doesn't wait for the pause.
  async fn recover_sync_loop(&mut self) {
    if !matches!(&self.client, ClientSlot::Running { task, .. } if task.is_finished()) {
      return;
    }
    let was_stable = self
      .maybe_loop_started
      .is_some_and(|started| started.elapsed() >= LOOP_STABLE_AFTER);
    self.loop_deaths = if was_stable {
      1
    } else {
      self.loop_deaths.saturating_add(1)
    };
    self.stop_runner().await;
    self.signals.connection.report_restarting();
    self.listener.on_change();
    let delay = loop_restart_delay(self.loop_deaths);
    if delay.is_zero() {
      self.restart_sync_loop().await;
      return;
    }
    tracing::warn!(
      delay_secs = delay.as_secs(),
      "the sync loop keeps dying; waiting before the next one"
    );
    let maybe_jobs = self.maybe_jobs.clone();
    tokio::task::spawn_local(async move {
      tokio::time::sleep(delay).await;
      if let Some(jobs) = maybe_jobs.and_then(|jobs| jobs.upgrade()) {
        let restart: Job = Box::new(|engine| Box::pin(engine.restart_sync_loop()));
        // The engine thread is ending otherwise; nothing to restart then.
        let _ = jobs.send(restart);
      }
    });
  }

  /// A new sync loop after one died, if the app or a drain still wants one.
  async fn restart_sync_loop(&mut self) {
    if !self.is_syncing_for_app && self.active_drains == 0 {
      return;
    }
    if let Err(error) = self.start().await {
      tracing::error!(?error, "could not start a new sync loop");
    }
  }

  /// Runs `operation` on the client with syncing paused, then resumes it and tells the app.
  pub(crate) async fn with_client<R, F>(&mut self, operation: F) -> Result<R, TeamCraftError>
  where
    F: for<'a> FnOnce(&'a mut CoreClient) -> LocalFuture<'a, Result<R, teamcraft_core::CoreError>>,
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
  /// Hands what an action queued to the client. The app is already told what it wrote locally
  /// ([`ReadQueue`]).
  pub(crate) async fn run_commands(&mut self, commands: Vec<CoreCommand>) -> Result<(), TeamCraftError> {
    match &mut self.client {
      ClientSlot::Running { handle, .. } => {
        for command in commands {
          handle.send(RunnerCommand::Ui(command));
        }
        handle.send(RunnerCommand::Flush);
      },
      ClientSlot::Idle(client) => {
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

  fn client_settings(&self) -> ClientSettings {
    ClientSettings {
      maybe_download_dir: Some(self.download_dir.clone()),
      maybe_push_gateway_url: self.maybe_push_gateway_url.clone(),
      maybe_upload_staging_dir: Some(self.upload_staging_dir.clone()),
      creates_notes_to_self: true,
      history_backfill: HistoryBackfill::KEEP_RECENT,
      leaves_mls_upkeep_to_app: self.is_memory_constrained,
      // The feeds show holes (issue #1438): a busy conversation's newest messages come ahead of
      // the rest.
      ..ClientSettings::default().with_capped_catch_up()
    }
  }

  /// A handle on the server whose requests share the engine thread's open connections.
  pub(crate) fn api(&self) -> Api {
    self.http.api()
  }
}

impl Drop for LoopDeathNotice {
  fn drop(&mut self) {
    if !std::thread::panicking() {
      return;
    }
    if let Some(jobs) = self.0.as_ref().and_then(mpsc::WeakUnboundedSender::upgrade) {
      let recover: Job = Box::new(|engine| Box::pin(engine.recover_sync_loop()));
      // No queue left: the engine thread is ending and has no use for a new loop.
      let _ = jobs.send(recover);
    }
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

/// How long to wait before the next sync loop after `deaths` died in a row: none after the first.
fn loop_restart_delay(deaths: u32) -> Duration {
  let doublings = deaths.saturating_sub(2).min(16);
  match deaths {
    0 | 1 => Duration::ZERO,
    _ => Duration::from_secs(1u64 << doublings).min(MAX_LOOP_RESTART_DELAY),
  }
}

/// A server handle for tests, with a provider that has a fixed token.
#[cfg(test)]
pub(crate) fn test_api(server_url: &str) -> Api {
  struct FixedToken;
  impl TokenProvider for FixedToken {
    fn access_token(&self) -> AccessToken {
      AccessToken::Token {
        value: "token".into(),
      }
    }
  }
  let tokens = ForeignTokens::new(Arc::new(FixedToken), IdTokens::default(), None)
    .unwrap_or_else(|error| panic!("token thread: {error}"));
  HttpServerApi::new(server_url, tokens)
}

impl ForeignTokens {
  /// The asking thread lives as long as a clone of this: a token refresh in the app (AppAuth blocks
  /// for up to 10 s) holds it, never the engine thread with the sync loop (e2ee.md §7.2 is untouched:
  /// no keys go through here, only the access token). The device key is read here too, and only then:
  /// a bound token needs it, and the store may take a moment.
  fn new(
    provider: Arc<dyn TokenProvider>,
    id_tokens: IdTokens,
    maybe_device_key: Option<Arc<dyn DeviceProofKey>>,
  ) -> Result<Self, TeamCraftError> {
    let (asks, asked) = std::sync::mpsc::channel::<oneshot::Sender<Result<String, ApiError>>>();
    let asking = provider.clone();
    let maybe_key_proofs = maybe_device_key.map(DeviceKeyProofs::new);
    let proving = maybe_key_proofs.clone();
    std::thread::Builder::new()
      .name("teamcraft-tokens".into())
      .spawn(move || {
        for answer in asked {
          let token = std::panic::catch_unwind(AssertUnwindSafe(|| -> Result<String, ApiError> {
            let token = signed_in_token(asking.as_ref())?;
            if let Some(proofs) = &proving {
              proofs.prepare_for(&token)?;
            }
            Ok(token)
          }));
          // The request may have been dropped meanwhile; nobody needs the token then.
          let _ = answer.send(token.unwrap_or_else(|_| {
            Err(ApiError::Network(
              "the app could not supply an access token".into(),
            ))
          }));
        }
      })
      .map_err(|error| TeamCraftError::Core(error.to_string()))?;
    Ok(Self {
      provider,
      id_tokens,
      asks,
      maybe_key_proofs,
    })
  }
}

impl AccessTokenSource for ForeignTokens {
  fn access_token(&self) -> Result<String, ApiError> {
    signed_in_token(self.provider.as_ref())
  }

  async fn fresh_token(&self) -> Result<String, ApiError> {
    let (answer, answered) = oneshot::channel();
    if self.asks.send(answer).is_err() {
      return self.access_token();
    }
    match answered.await {
      Ok(token) => token,
      Err(_) => self.access_token(),
    }
  }

  fn dpop_prover(&self) -> Option<Arc<DpopProver>> {
    self.maybe_key_proofs.as_ref()?.prover()
  }

  fn id_token(&self) -> Option<String> {
    self.id_tokens.current()
  }

  fn ask_for_new_id_token(&self) {
    self.id_tokens.ask_for_new_one();
  }
}

/// The app's answer as the core's: only an ended session is a sign-out; a refresh that failed is a
/// network problem, so callers retry with backoff and keep their state (issue #1671).
fn signed_in_token(provider: &dyn TokenProvider) -> Result<String, ApiError> {
  match provider.access_token() {
    AccessToken::Token { value } => Ok(value),
    AccessToken::Unavailable => Err(ApiError::Network(
      "the app could not refresh its access token".into(),
    )),
    AccessToken::SignedOut => Err(ApiError::Token("signed out".into())),
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
    // Before a workspace was verified: the default policy, as the desktop takes it.
    screen_policy: teamcraft_core::workspace::screen_share_policy(client.store()).unwrap_or_default(),
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
/// Opens read connections beside the engine's writer (issue #1476). A process that must stay small
/// (the notification extension) opens none, and a reader that doesn't open leaves its user on the
/// writer as before.
pub(crate) struct LaneOpener {
  engine: Store,
  writer: Arc<SealedSqliteStore>,
  path: PathBuf,
  key: SymmetricKey,
  journal: Arc<CommitJournal>,
  is_memory_constrained: bool,
}

impl LaneOpener {
  /// The read threads' views of the store: each reads through a connection of its own, beside the
  /// engine's groups of writes.
  fn read_lanes(&self) -> ReadLanes {
    ReadLanes {
      main: self.open(SealedStoreLimits::default()),
      query: self.open(READ_LANE_LIMITS),
      // No read connection: an action reads inside its group, which goes to the writer anyway.
      // A view of its own, so its groups never send the main read thread's reads to the writer.
      act: self.engine.sharing(LaneStore::writer(Arc::clone(&self.writer))),
    }
  }

  /// A view that reads through a new connection and writes through the writer's.
  fn open(&self, limits: SealedStoreLimits) -> Store {
    if self.is_memory_constrained {
      return self.engine.clone();
    }
    let key_source = StaticKeySource::new(self.key.duplicate());
    match SealedSqliteStore::open_reader(&self.path, &key_source, limits, Arc::clone(&self.journal)) {
      Ok(reader) => self
        .engine
        .sharing(LaneStore::reader(Arc::clone(&self.writer), Arc::new(reader))),
      Err(error) => {
        tracing::warn!(%error, "could not open a read connection; reading through the writer's");
        self.engine.clone()
      },
    }
  }
}

/// A segment prefetch running beside the engine's jobs ([`Engine::begin_prefetch_search_segments`]).
/// Dropping it, as a cancelled caller does, stops the prefetch.
pub(crate) struct SegmentPrefetchRun {
  answer: oneshot::Receiver<Result<SegmentPrefetchResult, TeamCraftError>>,
  _stop: AbortOnDrop,
}

/// Stops a task beside the jobs when its caller goes.
struct AbortOnDrop(Option<tokio::task::AbortHandle>);

impl SegmentPrefetchRun {
  fn done(result: SegmentPrefetchResult) -> Self {
    let (reply, answer) = oneshot::channel();
    // Invariant: `answer` is alive right here, so the send can't fail.
    let _ = reply.send(Ok(result));
    Self {
      answer,
      _stop: AbortOnDrop(None),
    }
  }

  /// Waits for the prefetch to end.
  pub(crate) async fn finished(self) -> Result<SegmentPrefetchResult, TeamCraftError> {
    self.answer.await.map_err(|_| TeamCraftError::EngineStopped)?
  }
}

impl Drop for AbortOnDrop {
  fn drop(&mut self) {
    if let Some(abort) = self.0.take() {
      abort.abort();
    }
  }
}

/// The prefetch itself: the look through the store on a blocking thread (it reads the announced
/// segments, and the first time backfills them from the messages), then the downloads.
async fn prefetch_segments_beside(
  api: Api,
  store: Store,
  max_total_bytes: u64,
) -> Result<SegmentPrefetchResult, TeamCraftError> {
  let reading = store.clone();
  let plan = tokio::task::spawn_blocking(move || SegmentPlan::read(&reading))
    .await
    .map_err(|error| TeamCraftError::Storage(format!("the segment look-up ended abnormally: {error}")))??;
  if plan.missing_count() > 0 {
    tracing::info!(missing = plan.missing_count(), "prefetching search segments");
  }
  Ok(plan.fetch(&api, &store, max_total_bytes).await?.into())
}

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

pub(crate) fn parse_ids<T: Id + std::str::FromStr>(texts: &[String]) -> Result<Vec<T>, TeamCraftError> {
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
/// when they open a file) until the rest fit into `max_bytes`, sparing those used within
/// `recent_use`. Returns the deleted paths.
fn trim_downloads(directory: &std::path::Path, max_bytes: u64, recent_use: Duration) -> Vec<PathBuf> {
  let Ok(entries) = std::fs::read_dir(directory) else {
    return Vec::new();
  };
  let spared_from = std::time::SystemTime::now()
    .checked_sub(recent_use)
    .unwrap_or(std::time::UNIX_EPOCH);
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
  let mut deleted = Vec::new();
  for (modified, size, path) in files {
    if total <= max_bytes || (!recent_use.is_zero() && modified >= spared_from) {
      break;
    }
    match std::fs::remove_file(&path) {
      Ok(()) => {
        total -= size;
        deleted.push(path);
      },
      Err(error) => tracing::debug!(%error, "an old download could not be deleted"),
    }
  }
  deleted
}

/// The other person's presence in a one-to-one DM; `None` for every other conversation.
pub(crate) fn direct_peer(
  backend: &impl ChatBackend,
  conversation: &ConversationView,
  me: UserId,
) -> Option<DirectPeer> {
  if conversation.kind != ConversationKindView::Direct {
    return None;
  }
  let other = *conversation.member_ids.iter().find(|member| **member != me)?;
  let maybe_user = backend.user(other);
  Some(DirectPeer {
    id: other,
    maybe_presence: maybe_user.as_ref().map(|user| Presence::from(user.presence)),
    maybe_profile: maybe_user.map(|user| (user.is_bot, user.avatar_hue)),
  })
}

#[cfg(test)]
mod tests {
  use std::time::{Duration, SystemTime};

  use super::*;

  /// Issue #1276: what the app hands over reaches the core's token source; an app that hands over
  /// nothing leaves it without one, and the core's wish for a new one is told once.
  #[test]
  fn the_app_s_id_token_reaches_the_core_and_its_wish_is_told_once() {
    let id_tokens = IdTokens::default();
    assert_eq!(id_tokens.current(), None);
    id_tokens.note(Some("a.b.c".into()));
    assert_eq!(id_tokens.clone().current().as_deref(), Some("a.b.c"));
    id_tokens.note(None);
    assert_eq!(id_tokens.current(), None);

    assert!(!id_tokens.take_request());
    id_tokens.ask_for_new_one();
    assert!(id_tokens.clone().take_request());
    assert!(!id_tokens.take_request());
  }

  /// Issue #424 review, H2: a notification extension never catches a conversation up (it would
  /// advance the MLS state next to the app's copy); the app's idle client does.
  #[test]
  fn only_a_late_look_of_the_apps_idle_client_asks_for_the_read_position() {
    assert!(PushOpening::LateLook.refreshes_read_position(PushReader::App, true));
    assert!(!PushOpening::LateLook.refreshes_read_position(PushReader::App, false));
    assert!(!PushOpening::LateLook.refreshes_read_position(PushReader::Extension, true));
    // The try right away has a tight bound: a text read must not wait for this (issue #1249).
    assert!(!PushOpening::Retry.refreshes_read_position(PushReader::App, true));
    assert!(!PushOpening::Received.refreshes_read_position(PushReader::App, true));
    assert!(!PushOpening::StoreOnly.refreshes_read_position(PushReader::App, true));
  }

  /// Only a late look of the app at a ring asks the server how the call ended (issue #1257): a ring
  /// opened as it arrives rings, and an extension never looks late.
  #[test]
  fn only_a_late_look_at_a_ring_asks_how_the_call_ended() {
    assert!(PushOpening::LateLook.asks_how_a_ring_ended(PushReader::App, &PushKind::Call));
    assert!(!PushOpening::LateLook.asks_how_a_ring_ended(PushReader::App, &PushKind::Message));
    assert!(!PushOpening::LateLook.asks_how_a_ring_ended(PushReader::App, &PushKind::Dismiss));
    assert!(!PushOpening::LateLook.asks_how_a_ring_ended(PushReader::Extension, &PushKind::Call));
    assert!(!PushOpening::Received.asks_how_a_ring_ended(PushReader::App, &PushKind::Call));
    assert!(!PushOpening::Retry.asks_how_a_ring_ended(PushReader::App, &PushKind::Call));
    assert!(!PushOpening::StoreOnly.asks_how_a_ring_ended(PushReader::App, &PushKind::Call));
  }

  #[test]
  fn only_the_apps_idle_client_catches_a_push_up() {
    assert!(PushReader::App.catches_up(true, true));
    assert!(
      !PushReader::App.catches_up(true, false),
      "the sync loop brings it"
    );
    assert!(!PushReader::App.catches_up(false, true), "nothing missing");
    assert!(!PushReader::Extension.catches_up(true, true));
    assert!(!PushReader::ProbingExtension.catches_up(true, true));
  }

  /// Issue #1662: a push goes to the server only for what the store lacks, and not while the sync
  /// loop runs: it brings the message itself, and the app fills the notification in from the store.
  #[test]
  fn a_push_fetches_only_what_the_store_lacks_and_the_sync_does_not_bring() {
    assert_eq!(PushFetch::of(false, false), PushFetch::NotNeeded);
    assert_eq!(PushFetch::of(false, true), PushFetch::NotNeeded);
    assert_eq!(PushFetch::of(true, true), PushFetch::LeftToSync);
    assert_eq!(PushFetch::of(true, false), PushFetch::Fetch);
  }

  /// Issue #1671: only an ended session is a sign-out. A refresh that failed for now is a network
  /// problem: the connection, the sync and queued work retry it with backoff and keep their state,
  /// and the apps don't show the sign-in.
  #[test]
  fn only_an_ended_session_is_a_sign_out() {
    struct Answers(fn() -> AccessToken);
    impl TokenProvider for Answers {
      fn access_token(&self) -> AccessToken {
        (self.0)()
      }
    }
    let answer = |token: fn() -> AccessToken| signed_in_token(&Answers(token));

    let supplied = answer(|| AccessToken::Token {
      value: "token".into(),
    });
    assert_eq!(supplied.ok().as_deref(), Some("token"));

    let Err(failed_refresh) = answer(|| AccessToken::Unavailable) else {
      panic!("a failed refresh supplies no token");
    };
    assert!(failed_refresh.is_transient());
    assert!(!failed_refresh.is_sign_in_required());
    assert!(matches!(
      TeamCraftError::from(CoreError::Api(failed_refresh)),
      TeamCraftError::Network(_)
    ));

    let Err(signed_out) = answer(|| AccessToken::SignedOut) else {
      panic!("a signed-out app supplies no token");
    };
    assert!(!signed_out.is_transient());
    assert!(signed_out.is_sign_in_required());
    assert!(matches!(
      TeamCraftError::from(CoreError::Api(signed_out)),
      TeamCraftError::SignedOut
    ));
  }

  #[test]
  fn only_a_token_the_app_could_not_give_stops_more_requests() {
    assert!(PushAccess::Missing.is_missing());
    assert!(PushAccess::TimedOut.is_missing());
    assert!(!PushAccess::Supplied.is_missing());
    assert!(!PushAccess::NotAsked.is_missing());
  }

  #[test]
  fn only_an_extension_that_may_probes_and_only_for_what_it_could_not_read() {
    assert!(PushReader::ProbingExtension.probes(true));
    assert!(!PushReader::ProbingExtension.probes(false));
    assert!(!PushReader::Extension.probes(true));
    assert!(!PushReader::App.probes(true));
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

    let deleted = trim_downloads(directory.path(), 20, Duration::ZERO);
    assert_eq!(deleted, [directory.path().join("old")]);

    let mut left: Vec<String> = std::fs::read_dir(directory.path())
      .unwrap()
      .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
      .collect();
    left.sort();
    assert_eq!(left, ["middle", "new"]);
  }

  #[test]
  fn downloads_used_lately_stay_while_the_app_runs() {
    let directory = TestDir::new("recent");
    let now = SystemTime::now();
    for (name, age_seconds) in [("old", 3_600), ("watched", 60)] {
      let path = directory.path().join(name);
      std::fs::write(&path, [0u8; 10]).unwrap();
      let file = std::fs::File::options().write(true).open(&path).unwrap();
      file.set_modified(now - Duration::from_secs(age_seconds)).unwrap();
    }

    let deleted = trim_downloads(directory.path(), 0, Duration::from_secs(600));

    assert_eq!(deleted, [directory.path().join("old")]);
    assert!(
      directory.path().join("watched").is_file(),
      "over the limit, but just used"
    );
  }

  #[test]
  fn downloads_within_the_limit_or_missing_stay_as_they_are() {
    let directory = TestDir::new("within");
    std::fs::write(directory.path().join("one"), [0u8; 10]).unwrap();
    assert!(trim_downloads(directory.path(), 10, Duration::ZERO).is_empty());
    assert!(directory.path().join("one").is_file());
    assert!(trim_downloads(&directory.path().join("missing"), 0, Duration::ZERO).is_empty());
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
      // Not migrated yet is the app's to do, not a store to reset (the extension's refusal, #1198).
      assert!(matches!(
        open_error(CoreError::Store(StoreError::LayoutBehind {
          found: 1,
          supported: 5,
        })),
        OpenError::Failed(TeamCraftError::Storage(_))
      ));
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
  /// The client's slot and the sync loop (issue #1142). The server is unreachable, so the loop stays
  /// at its first connection attempts; what matters here is the slot.
  mod client_slot {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use teamcraft_core::push_preview::PUSH_KEY_RECORD;
    use teamcraft_core::store::Table;
    use teamcraft_crypto::push::seal_wakeup;
    use teamcraft_proto::{Base64Bytes, MessageId, Snowflake};

    use teamcraft_core::api::fake::FakeServer;
    use teamcraft_crypto::backup::Argon2Settings;

    use super::*;

    const UNREACHABLE_SERVER: &str = "http://127.0.0.1:9";
    const KEY: [u8; 32] = [7; 32];

    #[tokio::test]
    async fn stopping_or_taking_an_idle_client_keeps_it() {
      in_engine_thread(async {
        let directory = TestDir::new("idle");
        let mut engine = enrolled_engine(&directory, Listener::default()).await;
        engine.stop().await.unwrap();
        engine.stop().await.unwrap();
        assert!(matches!(engine.client, ClientSlot::Idle(_)));
        let client = engine.take_client().await.unwrap();
        engine.client = ClientSlot::Idle(client);
        // The first job in the background used to lose the client here.
        let result = engine.with_client(|_| Box::pin(async { Ok(()) })).await;
        assert!(result.is_ok());
        assert!(matches!(engine.client, ClientSlot::Idle(_)));
      })
      .await;
    }

    #[tokio::test]
    async fn an_empty_slot_on_an_enrolled_device_repairs_itself_on_start() {
      in_engine_thread(async {
        let directory = TestDir::new("empty");
        let mut engine = enrolled_engine(&directory, Listener::default()).await;
        engine.client = ClientSlot::Missing;
        engine.start_syncing().await.unwrap();
        assert!(matches!(engine.client, ClientSlot::Running { .. }));
        engine.stop_runner().await;
      })
      .await;
    }

    #[tokio::test]
    async fn an_empty_slot_is_no_abandoned_setup_on_an_enrolled_device() {
      in_engine_thread(async {
        let directory = TestDir::new("not-abandoned");
        let mut engine = enrolled_engine(&directory, Listener::default()).await;
        engine.client = ClientSlot::Missing;
        engine.forget_unfinished_setup().await.unwrap();
        engine.start_syncing().await.unwrap();
        assert!(matches!(engine.client, ClientSlot::Running { .. }));
        engine.stop_runner().await;
      })
      .await;
    }

    #[tokio::test]
    async fn a_sync_loop_that_dies_is_replaced_at_once() {
      in_engine_thread(async {
        let directory = TestDir::new("dies");
        let listener = Listener::default();
        let mut engine = enrolled_engine(&directory, listener.clone()).await;
        let (jobs, mut queue) = mpsc::unbounded_channel::<Job>();
        engine.maybe_jobs = Some(jobs.downgrade());
        listener.panic_next_time();
        engine.start_syncing().await.unwrap();
        let dead = running_task(&engine);

        let notice = queue.recv().await.unwrap();
        notice(&mut engine).await;

        assert_ne!(running_task(&engine), dead, "a new loop runs");
        assert_eq!(engine.loop_deaths, 1);
        engine.stop_runner().await;
      })
      .await;
    }

    #[tokio::test]
    async fn start_notices_a_dead_loop_though_the_slot_says_running() {
      in_engine_thread(async {
        let directory = TestDir::new("noticed");
        let listener = Listener::default();
        let mut engine = enrolled_engine(&directory, listener.clone()).await;
        listener.panic_next_time();
        engine.start_syncing().await.unwrap();
        let dead = running_task(&engine);
        while !matches!(&engine.client, ClientSlot::Running { task, .. } if task.is_finished()) {
          tokio::task::yield_now().await;
        }

        engine.start_syncing().await.unwrap();

        assert_ne!(running_task(&engine), dead);
        engine.stop_runner().await;
      })
      .await;
    }

    #[tokio::test]
    async fn a_push_refresh_leaves_the_sync_loop_running() {
      in_engine_thread(async {
        let directory = TestDir::new("refresh");
        let mut engine = enrolled_engine(&directory, Listener::default()).await;
        engine.start_syncing().await.unwrap();
        let task = running_task(&engine);

        // Nothing registered yet: answered from the store alone.
        let renewed = engine.refresh_push().unwrap().await.unwrap();

        assert!(!renewed.unwrap());
        assert_eq!(running_task(&engine), task);
        engine.stop_runner().await;
      })
      .await;
    }

    /// The app retries a registration on a network error (`PushRegistrationRetry`).
    #[tokio::test]
    async fn a_failed_registration_says_network_and_leaves_the_loop_running() {
      in_engine_thread(async {
        let directory = TestDir::new("register");
        let mut engine = enrolled_engine(&directory, Listener::default()).await;
        engine.start_syncing().await.unwrap();
        let task = running_task(&engine);

        let registered = engine
          .register_push(PushService::Fcm, "token".into())
          .unwrap()
          .await
          .unwrap();

        assert!(matches!(registered, Err(TeamCraftError::Network(_))));
        assert_eq!(running_task(&engine), task);
        engine.stop_runner().await;
      })
      .await;
    }

    #[test]
    fn a_loop_that_keeps_dying_waits_longer_each_time() {
      assert_eq!(loop_restart_delay(1), Duration::ZERO);
      assert_eq!(loop_restart_delay(2), Duration::from_secs(1));
      assert_eq!(loop_restart_delay(3), Duration::from_secs(2));
      assert_eq!(loop_restart_delay(100), MAX_LOOP_RESTART_DELAY);
    }

    /// Issue #1662 and GlitchTip TEAMCRAFT-1O: an app without a token right now (a refresh that
    /// failed offline) gets the store's preview at once, without a catch-up that would only fail
    /// and be logged as a sign-out.
    ///
    /// Real time, not paused: the token is answered by the app's thread, outside the runtime, so a
    /// paused clock sees an idle runtime while it waits and jumps past [`PUSH_FETCH_BUDGET`] first.
    #[tokio::test]
    async fn a_push_without_a_token_shows_what_the_store_has_and_skips_the_catch_up() {
      in_engine_thread(async {
        let directory = TestDir::new("push-no-token");
        let tokens = Arc::new(CountedTokens::default());
        let mut engine = enrolled_engine_with(&directory, Listener::default(), tokens.clone()).await;
        let payload = sealed_message_push(&engine);
        let asked_before = tokens.asked();
        let mut times = PushTimes::default();

        let preview = engine
          .push_preview_within_budget(&payload, PushReader::App, &mut times)
          .await
          .unwrap();

        assert_eq!(
          preview.maybe_gap,
          Some(PreviewGap::NoSession),
          "tried again later"
        );
        assert_eq!(
          tokens.asked() - asked_before,
          1,
          "one ask, no catch-up asking again"
        );
        assert_eq!(times.fetch, Duration::ZERO);
        assert_eq!(times.catch_up, Duration::ZERO);
        assert!(matches!(engine.client, ClientSlot::Idle(_)));
      })
      .await;
    }

    /// A token refresh that hangs (AppAuth waits up to 10 s) uses up the fetch's time, not more:
    /// the token wait and the fetch share [`PUSH_FETCH_BUDGET`], and the engine is free again.
    #[tokio::test(start_paused = true)]
    async fn a_hanging_token_refresh_holds_a_push_no_longer_than_the_fetch_budget() {
      in_engine_thread(async {
        let directory = TestDir::new("push-hanging-token");
        let tokens = Arc::new(CountedTokens::default());
        let mut engine = enrolled_engine_with(&directory, Listener::default(), tokens.clone()).await;
        let payload = sealed_message_push(&engine);
        let hang = tokens.hang();
        let started = tokio::time::Instant::now();
        let mut times = PushTimes::default();

        let preview = engine
          .push_preview_within_budget(&payload, PushReader::App, &mut times)
          .await
          .unwrap();

        assert_eq!(started.elapsed(), PUSH_FETCH_BUDGET);
        assert_eq!(preview.maybe_gap, Some(PreviewGap::TimedOut));
        assert_eq!(
          times.catch_up,
          Duration::ZERO,
          "no second wait for the same refresh"
        );
        drop(hang);
      })
      .await;
    }

    /// What the store answers alone (a ring shows no message) needs no token and no connection.
    #[tokio::test]
    async fn a_push_the_store_answers_asks_for_no_token() {
      in_engine_thread(async {
        let directory = TestDir::new("push-stored");
        let tokens = Arc::new(CountedTokens::default());
        let mut engine = enrolled_engine_with(&directory, Listener::default(), tokens.clone()).await;
        let payload = sealed_push(&engine, PushKind::Call, None);
        let asked_before = tokens.asked();
        let mut times = PushTimes::default();

        engine
          .push_preview_within_budget(&payload, PushReader::App, &mut times)
          .await
          .unwrap();

        assert_eq!(tokens.asked(), asked_before);
        assert_eq!(times.auth_wait, Duration::ZERO);
      })
      .await;
    }

    /// Tokens as an app hands them over: none (signed out, or a refresh that failed), and on
    /// request a refresh that doesn't end until the returned guard goes.
    #[derive(Default)]
    struct CountedTokens {
      asked: AtomicUsize,
      maybe_hang: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    }

    impl CountedTokens {
      fn asked(&self) -> usize {
        self.asked.load(Ordering::SeqCst)
      }

      fn hang(&self) -> std::sync::mpsc::Sender<()> {
        let (release, released) = std::sync::mpsc::channel();
        *self.maybe_hang.lock().unwrap() = Some(released);
        release
      }
    }

    impl TokenProvider for CountedTokens {
      fn access_token(&self) -> AccessToken {
        self.asked.fetch_add(1, Ordering::SeqCst);
        if let Some(released) = self.maybe_hang.lock().unwrap().take() {
          // Ends when the test drops its sender.
          let _ = released.recv();
        }
        AccessToken::Unavailable
      }
    }

    fn sealed_message_push(engine: &Engine) -> Vec<u8> {
      let message = MessageId::from_snowflake(Snowflake::new(2).unwrap());
      sealed_push(engine, PushKind::Message, Some(message))
    }

    /// A wake-up as the server seals it, with a push key put into the store as registering does.
    fn sealed_push(engine: &Engine, kind: PushKind, maybe_message_id: Option<MessageId>) -> Vec<u8> {
      let key = [9u8; 32];
      records::put_json(
        &engine.store,
        Table::Account,
        PUSH_KEY_RECORD,
        &Base64Bytes::new(key.to_vec()),
      )
      .unwrap();
      // A workspace whose policy shows the text: without one a notification fails closed.
      let mut policy = teamcraft_proto::WorkspacePolicy::default();
      policy.notifications.push_content = teamcraft_proto::PushContentMode::Full;
      records::put_json(
        &engine.store,
        Table::Workspace,
        "state",
        &serde_json::json!({ "policy": policy }),
      )
      .unwrap();
      let wake_up = PushWakeUp {
        kind,
        conversation_id: ConversationId::from_snowflake(Snowflake::new(1).unwrap()),
        maybe_message_id,
        maybe_sender_id: None,
        is_urgent: false,
        maybe_thread_id: None,
        maybe_call_id: None,
        maybe_badge: None,
        maybe_sent_at: None,
        maybe_meeting: None,
        maybe_message: None,
      };
      seal_wakeup(
        &SymmetricKey::from_slice(&key).unwrap(),
        &serde_json::to_vec(&wake_up).unwrap(),
        &mut rand_core::OsRng,
      )
      .unwrap()
    }

    /// Panics once when armed, like a bug anywhere in what the sync loop runs.
    #[derive(Clone, Default)]
    struct Listener(Arc<AtomicBool>);

    impl Listener {
      fn panic_next_time(&self) {
        self.0.store(true, Ordering::SeqCst);
      }
    }

    impl EventListener for Listener {
      fn on_change(&self) {
        assert!(!self.0.swap(false, Ordering::SeqCst), "a bug in the sync loop");
      }
    }

    struct Token;

    impl TokenProvider for Token {
      fn access_token(&self) -> AccessToken {
        AccessToken::Token {
          value: "token".into(),
        }
      }
    }

    async fn in_engine_thread(test: impl Future<Output = ()>) {
      LocalSet::new().run_until(test).await;
    }

    /// An engine as a restarted app has it: the device enrolled earlier, the account in the store.
    async fn enrolled_engine(directory: &TestDir, listener: Listener) -> Engine {
      enrolled_engine_with(directory, listener, Arc::new(Token)).await
    }

    /// [`enrolled_engine`] whose app hands over tokens as `tokens` does.
    async fn enrolled_engine_with(
      directory: &TestDir,
      listener: Listener,
      tokens: Arc<dyn TokenProvider>,
    ) -> Engine {
      let config = AppConfig {
        server_url: UNREACHABLE_SERVER.into(),
        data_dir: directory.path().to_string_lossy().into_owned(),
        account: "account".into(),
        database_key: KEY.to_vec(),
        push_gateway_url: None,
        is_memory_constrained: false,
      };
      let signals = SyncSignals {
        network: NetworkChanges::new(),
        connection: ConnectionStatus::new(),
        catch_up_focus: CatchUpFocus::default(),
        id_tokens: Default::default(),
        running_calls: Default::default(),
      };
      let start = EngineStart::new(config, tokens, None, Arc::new(listener), signals).unwrap();
      let key_source = StaticKeySource::new(SymmetricKey::from_slice(&KEY).unwrap());
      let store = SealedSqliteStore::open(&start.store_path(), &key_source).unwrap();
      let client = PendingEnrollment::new()
        .complete(
          FakeServer::new().add_user("Alice"),
          Arc::new(store),
          Box::new(SystemClock),
          ClientSettings::default(),
          EnrollmentOptions {
            argon2: Argon2Settings {
              memory_kib: 1024,
              iterations: 1,
              parallelism: 1,
            },
            ..enrollment_options(
              "phone".into(),
              EnrollmentSecret::Passphrase(SecretBytes::new(b"passphrase".to_vec())),
            )
          },
        )
        .await
        .unwrap();
      drop(client);
      let mut engine = opened_store(start);
      engine.open_client().map_err(|_| "client did not open").unwrap();
      engine
    }

    fn opened_store(start: EngineStart) -> Engine {
      let (jobs, _queue) = mpsc::unbounded_channel::<Job>();
      Engine::open(start, jobs.downgrade())
        .map_err(|_| "engine did not open")
        .unwrap()
    }

    fn running_task(engine: &Engine) -> tokio::task::Id {
      match &engine.client {
        ClientSlot::Running { task, .. } => task.id(),
        _ => panic!("no sync loop"),
      }
    }
  }

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

  /// The app's device key behind the engine's token source (e2ee.md §3.1.2).
  mod device_key {
    use teamcraft_core::api::ApiError;

    use super::*;
    use crate::device_proofs::DeviceProofError;
    use crate::device_proofs::test_support::{StandInKey, bound_token};

    struct OneToken(String);

    impl TokenProvider for OneToken {
      fn access_token(&self) -> AccessToken {
        AccessToken::Token {
          value: self.0.clone(),
        }
      }
    }

    fn tokens_with(token: String, maybe_key: Option<Arc<StandInKey>>) -> ForeignTokens {
      let maybe_key = maybe_key.map(|key| key as Arc<dyn crate::device_proofs::DeviceProofKey>);
      ForeignTokens::new(Arc::new(OneToken(token)), IdTokens::default(), maybe_key).unwrap()
    }

    #[tokio::test]
    async fn a_bound_token_finds_its_key_before_the_request() {
      let key = StandInKey::new();
      let token = bound_token(&key.thumbprint());
      let tokens = tokens_with(token.clone(), Some(key.clone()));

      assert_eq!(tokens.fresh_token().await.unwrap(), token);

      assert_eq!(tokens.dpop_prover().unwrap().thumbprint(), key.thumbprint());
    }

    #[tokio::test]
    async fn an_unbound_token_touches_no_key() {
      let key = StandInKey::new();
      let tokens = tokens_with("opaque".into(), Some(key.clone()));

      tokens.fresh_token().await.unwrap();

      assert!(tokens.dpop_prover().is_none());
    }

    #[tokio::test]
    async fn a_lost_key_asks_for_a_sign_in_and_a_locked_one_waits() {
      let key = StandInKey::new();
      let token = bound_token(&key.thumbprint());
      let tokens = tokens_with(token, Some(key.clone()));

      key.fail_with(DeviceProofError::KeyUnavailable);
      let waiting = tokens.fresh_token().await.err().unwrap();
      assert!(
        waiting.is_transient() && !waiting.is_sign_in_required(),
        "{waiting}"
      );

      key.fail_with(DeviceProofError::KeyLost);
      let lost = tokens.fresh_token().await.err().unwrap();
      assert!(matches!(lost, ApiError::BoundKeyLost));
    }

    /// Android: no key behind the core, tokens stay bearer as before.
    #[tokio::test]
    async fn a_platform_without_a_key_has_no_prover() {
      let tokens = tokens_with("opaque".into(), None);
      tokens.fresh_token().await.unwrap();
      assert!(tokens.dpop_prover().is_none());
    }
  }
}
