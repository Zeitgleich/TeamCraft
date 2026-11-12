//! [`ChatBackend`] over the client core that runs in the desktop shell (`apps/desktop`).
//!
//! The trait is synchronous, the core is not reachable synchronously from a webview. So this backend
//! keeps a mirror: the sidebar data (a [`ChatSnapshot`]) and the pages of each conversation feed that
//! were read so far. A read that misses the mirror returns nothing and starts a load; when it
//! arrives the UI is told to re-read through the change notifier. The shell emits `core-changed`
//! whenever its store changes and the mirror then refreshes what it holds.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;
use teamcraft_client_api::calls::{CallMoveOffer, OrgBackgroundRule};
use teamcraft_proto::OrgBackgroundId;

use futures_util::future::{join_all, join3};
use leptos::prelude::set_timeout;
use leptos::task::spawn_local;
use serde_json::json;
use teamcraft_client_api::actions::{
  BrowsableChannel, CategoryInfo, ChannelChanges, DownloadControl, DownloadEntry, FileInfo, LinkedMessage,
  MediaAnchor, MediaPage, MessageExtras, MessagePriority, NewChannel, NewSpace, OutgoingMessage,
  PermalinkTarget, SavedItem, ShareLink, ShareLinkOptions, SpaceChanges, SpaceInvite,
};
use teamcraft_client_api::backend::{ChatBackend, SearchBackend};
use teamcraft_client_api::calls::BackgroundImageInfo;
use teamcraft_client_api::files_query::{FilesPage, FilesQuery};
use teamcraft_client_api::links::{LinkPage, links_page};
use teamcraft_client_api::model::{
  Conversation, ConversationId, CustomStatus, GapEdge, HistoryGap, InviteId, JoinableSpace, Message,
  MessageId, MessageKind, OlderHistory, PendingInvite, PresenceStatus, Space, SpaceId, SpaceRole,
  SpaceRoster, ThreadId, ThreadSummary, UnseenReactions, User, UserId, is_pending, pending_message_id,
};
use teamcraft_client_api::remote::{
  CORE_CHANGED_EVENT, CORE_FAILED_EVENT, ChatAction, ChatAnswer, ChatQuery, ChatSnapshot, CoreConnection,
  CoreStatus, EnrollRequest, EnrollSecret, LinkRequestView, LinkStatus, MessageWindow, OrgRecoveryStatus,
  PhoneLinkStatus, StoreHealth,
};
use teamcraft_client_api::scheduled::ScheduledOverview;
use teamcraft_client_api::search::{ParsedQuery, SavedSearch, SearchHit, SearchResults};
use teamcraft_client_api::snapshot_changes::{SnapshotChanges, SnapshotCursor};
use teamcraft_proto::{
  AttachmentId, BackgroundId, CategoryId, ClientMessageId, HistoryPolicy, ScheduledMessageId,
  WebPushKeyResponse,
};

use crate::auth::Authenticator;
use crate::auth::tauri::{call, listen, listen_text};
use crate::feedback::{ActionFailure, ActionTicket, report_background_failure, report_failure};
use crate::perf::{PerfOperation, PerfStatus};
use crate::route::Route;

/// Messages fetched per request. Feeds are read in 2-row windows by the list, so pages keep the
/// number of requests small while scrolling.
const PAGE: usize = 100;
/// Events arrive in bursts (a sync delivers many); refresh once after they settle.
const REFRESH_DELAY: Duration = Duration::from_millis(120);
/// A change of the core asks a query again at most this often; one that comes sooner is answered
/// once the interval is over. A steady stream of changes (a channel an integration posts to every
/// second) asked every query on every change, an expensive one (the activity feed, for a badge)
/// back to back. The user's own actions still ask at once.
const MIN_REQUERY_INTERVAL: Duration = Duration::from_secs(1);

thread_local! {
  /// The backend the core's events go to (#344). The page subscribes once; a new sign-in connects a
  /// new backend, which takes the events over, so listeners never stack and the old backend's
  /// mirror is not refreshed any more.
  static CORE_EVENTS: RefCell<EventRoute<RemoteChat>> = const { RefCell::new(EventRoute::new()) };
}

/// Where the local client stands, as the shell reports it.
pub async fn core_status(user: UserId) -> Result<CoreStatus, String> {
  call("core_start", &json!({ "user": user })).await
}

/// The user asked to try again: the desktop shell may ask the OS keychain again at once.
pub async fn retry_keychain() -> Result<(), String> {
  call("core_retry_keychain", &json!({})).await
}

/// Where the core's gateway connection stands (answered without waiting for the core).
pub async fn connection_state() -> Result<CoreConnection, String> {
  call("core_connection", &json!({})).await
}

/// The network came back or the page became visible again: the core checks its connection now.
pub async fn network_changed() -> Result<(), String> {
  call("core_network_changed", &json!({})).await
}

/// Whether the browser saves the local data (e2ee.md §9.4). Browser only.
pub async fn store_health() -> Result<StoreHealth, String> {
  call("core_store_health", &json!({})).await
}

/// Creates (or recovers) the keys of this device.
pub async fn enroll(request: &EnrollRequest) -> Result<(), String> {
  call("core_enroll", &json!({ "request": request })).await
}

/// A meeting guest's ephemeral device: no passphrase, no backup (browser only).
pub async fn enroll_guest(device_name: &str) -> Result<(), String> {
  enroll(&EnrollRequest {
    secret: EnrollSecret::Guest,
    device_name: device_name.to_owned(),
  })
  .await
}

/// A guest leaves: deletes this browser's database of the guest device (browser only).
pub async fn wipe_guest_store() -> Result<(), String> {
  call("core_wipe_guest", &json!({})).await
}

/// This guest device's identity key fingerprint and key code (browser only).
pub async fn own_key() -> Result<OwnKey, String> {
  call("core_own_key", &json!({})).await
}

/// A guest device's key as its page reports and shows it.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct OwnKey {
  pub fingerprint: String,
  pub code: String,
}

/// A guest keeps the organizer's identity key fingerprint from the link for the meeting chat: its
/// group is only joined from a welcome of that key's devices, or of an admitter's the key signed
/// for `meeting` (browser only).
pub async fn guest_anchor(
  conversation: ConversationId,
  meeting: teamcraft_proto::MeetingId,
  fingerprint: &str,
) -> Result<(), String> {
  call(
    "core_guest_anchor",
    &json!({ "conversation": conversation, "meeting": meeting, "fingerprint": fingerprint }),
  )
  .await
}

/// New device: shows a link code and waits for an existing device (e2ee.md §5).
pub async fn link_start(device_name: &str) -> Result<(), String> {
  call("core_link_start", &json!({ "device_name": device_name })).await
}

pub async fn link_status() -> Result<LinkStatus, String> {
  call("core_link_status", &json!({})).await
}

pub async fn link_cancel() -> Result<(), String> {
  call("core_link_cancel", &json!({})).await
}

/// Hands a chat action to the core: typed to the core in this page (the browser), which never puts
/// it into JSON (#1413), or as the desktop shell's `core_action` command. Their JSON is in a
/// module of its own (`areas::shell_action`), which only the desktop app loads.
async fn core_action(action: ChatAction) -> Result<(), String> {
  #[cfg(target_arch = "wasm32")]
  if !crate::auth::tauri::is_available() {
    return crate::web_core::act(action).await;
  }
  crate::areas::shell_action(action)
    .await
    .map_err(|error| error.to_string())?
    .await
}

/// Asks the core a chat query, like [`core_action`].
async fn core_query(query: ChatQuery) -> Result<ChatAnswer, String> {
  #[cfg(target_arch = "wasm32")]
  if !crate::auth::tauri::is_available() {
    return crate::web_core::query(query).await;
  }
  crate::areas::shell_query(query)
    .await
    .map_err(|error| error.to_string())?
    .await
}

/// `core_action` for the desktop shell, in JSON (in the `shell` chunk).
pub(crate) async fn shell_action(action: ChatAction) -> Result<(), String> {
  call("core_action", &json!({ "action": action })).await
}

/// `core_query` for the desktop shell, in JSON (in the `shell` chunk).
pub(crate) async fn shell_query(query: ChatQuery) -> Result<ChatAnswer, String> {
  call("core_query", &json!({ "query": query })).await
}

/// Reads the core's snapshot or a feed, like [`core_action`]: typed from the core in this page,
/// in JSON from the desktop shell (`areas::shell_read`, #1699).
async fn core_read(read: CoreRead) -> Result<CoreReadAnswer, String> {
  #[cfg(target_arch = "wasm32")]
  if !crate::auth::tauri::is_available() {
    return crate::web_core::read(read).await;
  }
  crate::areas::shell_read(read)
    .await
    .map_err(|error| error.to_string())?
    .await
}

/// A read for the desktop shell, in JSON (in the `shell` chunk).
pub(crate) async fn shell_read(read: CoreRead) -> Result<CoreReadAnswer, String> {
  let (command, args) = read.to_json();
  let answer: serde_json::Value = call(command, &args).await?;
  read.answer_from_json(answer)
}

async fn read_snapshot() -> Result<ChatSnapshot, String> {
  match core_read(CoreRead::Snapshot).await? {
    CoreReadAnswer::Snapshot(snapshot) => Ok(*snapshot),
    _ => Err(CoreRead::Snapshot.mismatch()),
  }
}

async fn read_snapshot_changes(maybe_since: Option<SnapshotCursor>) -> Result<SnapshotChanges, String> {
  let read = CoreRead::Changes(maybe_since);
  match core_read(read).await? {
    CoreReadAnswer::Changes(changes) => Ok(*changes),
    _ => Err(read.mismatch()),
  }
}

async fn read_messages(
  conversation: ConversationId,
  start: usize,
  len: usize,
) -> Result<MessageWindow, String> {
  let read = CoreRead::Messages {
    conversation,
    start,
    len,
  };
  match core_read(read).await? {
    CoreReadAnswer::Messages(window) => Ok(window),
    _ => Err(read.mismatch()),
  }
}

async fn read_thread(conversation: ConversationId, thread: ThreadId) -> Result<Vec<Message>, String> {
  let read = CoreRead::Thread { conversation, thread };
  match core_read(read).await? {
    CoreReadAnswer::Thread(messages) => Ok(messages),
    _ => Err(read.mismatch()),
  }
}

/// A read of the core's snapshot or of a feed (`core_snapshot`, `core_snapshot_changes`,
/// `core_messages`, `core_thread`). The page's core answers it typed; in JSON it only travels to
/// the desktop shell and between two tabs of an account, whose code is in the `shell` and `tabs`
/// modules (#1699): the readers and writers of the snapshot's JSON were in every first visit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CoreRead {
  Snapshot,
  /// What changed since the cursor; without one, the whole snapshot.
  Changes(Option<SnapshotCursor>),
  Messages {
    conversation: ConversationId,
    start: usize,
    len: usize,
  },
  Thread {
    conversation: ConversationId,
    thread: ThreadId,
  },
}

/// The answer to a [`CoreRead`].
pub(crate) enum CoreReadAnswer {
  Snapshot(Box<ChatSnapshot>),
  Changes(Box<SnapshotChanges>),
  Messages(MessageWindow),
  Thread(Vec<Message>),
}

impl CoreRead {
  /// The read as the shell's command and its arguments.
  pub(crate) fn to_json(self) -> (&'static str, serde_json::Value) {
    match self {
      Self::Snapshot => ("core_snapshot", json!({})),
      Self::Changes(maybe_since) => ("core_snapshot_changes", json!({ "since": maybe_since })),
      Self::Messages {
        conversation,
        start,
        len,
      } => (
        "core_messages",
        json!({ "conversation": conversation, "start": start, "len": len }),
      ),
      Self::Thread { conversation, thread } => (
        "core_thread",
        json!({ "conversation": conversation, "thread": thread }),
      ),
    }
  }

  /// Reads the answer to this read from JSON.
  pub(crate) fn answer_from_json(self, answer: serde_json::Value) -> Result<CoreReadAnswer, String> {
    let read = match self {
      Self::Snapshot => {
        serde_json::from_value(answer).map(|snapshot| CoreReadAnswer::Snapshot(Box::new(snapshot)))
      },
      Self::Changes(_) => {
        serde_json::from_value(answer).map(|changes| CoreReadAnswer::Changes(Box::new(changes)))
      },
      Self::Messages { .. } => serde_json::from_value(answer).map(CoreReadAnswer::Messages),
      Self::Thread { .. } => serde_json::from_value(answer).map(CoreReadAnswer::Thread),
    };
    read.map_err(|error| format!("unreadable answer of {}: {error}", self.to_json().0))
  }

  /// The error of an answer of another kind than the read's: a mistake here.
  fn mismatch(self) -> String {
    format!("the answer of {} is of another read", self.to_json().0)
  }
}

#[cfg(any(target_arch = "wasm32", test))]
impl CoreReadAnswer {
  /// The answer in JSON, for another tab of the account.
  pub(crate) fn to_json(&self) -> Result<serde_json::Value, String> {
    let written = match self {
      Self::Snapshot(snapshot) => serde_json::to_value(snapshot),
      Self::Changes(changes) => serde_json::to_value(changes),
      Self::Messages(window) => serde_json::to_value(window),
      Self::Thread(messages) => serde_json::to_value(messages),
    };
    written.map_err(|error| error.to_string())
  }
}

/// Wraps the key backup with a new recovery passphrase.
pub async fn change_passphrase(passphrase: &str) -> Result<(), String> {
  core_action(ChatAction::ChangePassphrase {
    passphrase: passphrase.to_owned(),
  })
  .await
}

/// Wraps the key backup with the recovery key of a new kit the user saved (e2ee.md §6.5.7); the old
/// kit and a legacy passphrase stop working.
pub async fn set_recovery_key(recovery_key: &str) -> Result<(), String> {
  core_action(ChatAction::SetRecoveryKey {
    recovery_key: recovery_key.to_owned(),
  })
  .await
}

/// Existing device: joins the link of a new device. The short code appears only on the new device.
pub async fn approval_start(code: &str) -> Result<(), String> {
  call("core_approval_start", &json!({ "code": code })).await
}

/// Existing device: the user typed the short code of the new device; the core checks it and then
/// hands the keys over.
pub async fn approval_confirm(short_code: &str) -> Result<(), String> {
  call("core_approval_confirm", &json!({ "short_code": short_code })).await
}

/// New device: asks the other devices of the account to approve this one (hands the code's key to
/// the server; only on the user's choice).
pub async fn link_announce() -> Result<(), String> {
  call("core_link_announce", &json!({})).await
}

/// New device without kit or other device: asks the organization to recover the account
/// (e2ee.md §6.3.1). `maybe_previous` is a request made before the page reloaded; it is withdrawn.
pub async fn org_recovery_start(
  maybe_previous: Option<teamcraft_proto::RecoveryRequestId>,
) -> Result<OrgRecoveryStatus, String> {
  call(
    "core_org_recovery_start",
    &json!({ "maybe_previous": maybe_previous }),
  )
  .await
}

/// Where this device's recovery request stands (asked from the server).
pub async fn org_recovery_status() -> Result<OrgRecoveryStatus, String> {
  call("core_org_recovery_status", &json!({})).await
}

/// The person went back: withdraws the request.
pub async fn org_recovery_cancel() -> Result<(), String> {
  call("core_org_recovery_cancel", &json!({})).await
}

/// The settings the account holds (e2ee.md §6.4), fresh from the core rather than from the cache of
/// answers: the caller reads them once per change of `ChatBackend::settings_mark`.
pub async fn account_settings() -> Result<teamcraft_proto::synced_settings::SyncedSettings, String> {
  match core_query(ChatQuery::AccountSettings).await? {
    ChatAnswer::AccountSettings { settings } => Ok(settings),
    _ => Err("unexpected answer to the account settings query".to_owned()),
  }
}

/// The new devices of this account that ask to be linked (e2ee.md §5).
pub async fn link_requests() -> Result<Vec<LinkRequestView>, String> {
  call("core_link_requests", &json!({})).await
}

/// Takes up a link request and joins its relay; the user then types the code the new device shows
/// ([`approval_confirm`]).
pub async fn link_request_review(id: &str) -> Result<(), String> {
  call("core_link_request_review", &json!({ "id": id })).await
}

pub async fn link_request_reject(id: &str) -> Result<(), String> {
  call("core_link_request_reject", &json!({ "id": id })).await
}

pub async fn approval_cancel() -> Result<(), String> {
  call("core_approval_cancel", &json!({})).await
}

/// Signed-in device: shows a code a new phone scans (*Link a phone*, issue #998). Once it did, the
/// user types the code the phone shows ([`approval_confirm`]); [`approval_cancel`] closes the code.
pub async fn phone_link_start() -> Result<(), String> {
  call("core_phone_link_start", &json!({})).await
}

pub async fn phone_link_status() -> Result<PhoneLinkStatus, String> {
  call("core_phone_link_status", &json!({})).await
}

/// Browser: subscribes this browser to Web Push with the server's key, so the service worker can show
/// notifications while no page is open.
pub async fn enable_web_push(auth: &dyn Authenticator) -> Result<(), String> {
  let reply = auth
    .api_get("/api/v1/push/web-key")
    .await
    .map_err(|error| error.to_string())?;
  if reply.status != 200 {
    return Err(format!("the server answered HTTP {}", reply.status));
  }
  let key: WebPushKeyResponse =
    serde_json::from_str(&reply.body).map_err(|error| format!("unreadable push key: {error}"))?;
  let Some(vapid_key) = key.maybe_public_key else {
    return Err("this server does not send Web Push".to_owned());
  };
  call("core_enable_push", &json!({ "vapid_key": vapid_key })).await
}

/// Sets the unreadable local database aside (desktop) or deletes it (browser) and opens a new one,
/// so the device can be set up again (issue #160).
pub async fn reset_store() -> Result<(), String> {
  call("core_reset_store", &json!({})).await
}

/// Signs the core out; the backend of this session gets no more events (#344).
pub async fn stop_core() -> Result<(), String> {
  // async-ok: a thread-local, not a signal.
  CORE_EVENTS.with(|route| route.borrow_mut().clear());
  call("core_stop", &json!({})).await
}

#[derive(Default)]
struct Feed {
  total: usize,
  rows: BTreeMap<usize, Message>,
  /// Read by the UI since it was last told about a change: a refresh reloads its pages.
  is_used: bool,
  /// The core changed while nobody read this feed: its pages are reloaded on the next read.
  is_stale: bool,
  /// Pages the UI read or asked ahead for since it was last told about a change: a refresh reloads
  /// these and drops the others, so its work stays with what is shown, not with how far the user
  /// scrolled back (issue #1050).
  read_pages: BTreeSet<usize>,
  /// The pages of rows the UI showed since that change (read, not only asked ahead for): a page
  /// loaded ahead tells the UI only when one of its rows is waiting for it.
  shown_pages: BTreeSet<usize>,
  /// Rows that arrived before the ones held ([`ChatBackend::rows_prepended`]).
  prepended: usize,
  /// The core's total jumped and the newest rows are being read to see where the rows held went:
  /// until then the feed keeps its total, so the UI never reads the old rows by the new length.
  is_moving: bool,
  /// A page of older history was asked for and hasn't moved the rows yet: its last page can be a
  /// single row, which a look at the total alone takes for a new message.
  is_expecting_older: bool,
  /// A page of older history was asked for and no answer read since says how that stands. What is
  /// cached from before still says "available", and asking again on it would read on and on.
  is_asking_older: bool,
  /// A page of a hole in the history was asked for and hasn't moved the rows yet (#1156): its rows
  /// arrive among the rows held, not before them.
  maybe_gap_rows: Option<GapRows>,
  /// A page of a hole was asked for and no answer read since says how that stands, as
  /// [`Self::is_asking_older`].
  is_asking_gap: bool,
}

/// A row shown for a message the core has not answered for yet ([`RemoteChat::show_pending`]).
#[derive(Clone, Copy, Debug)]
struct PendingRow {
  conversation: ConversationId,
  maybe_thread: Option<ThreadId>,
  id: MessageId,
}

/// Where the rows of a hole's page arrive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GapRows {
  /// The rows from here on move.
  index: usize,
  /// They arrive before what the reader looks at, read from the hole's newer side: they count as
  /// prepended, so the reader's rows stay where they are.
  is_before_reader: bool,
}

struct Mirror {
  snapshot: ChatSnapshot,
  /// How far the snapshot was read; the core answers the next refresh with what changed since.
  cursor: SnapshotCursor,
  feeds: HashMap<ConversationId, Feed>,
  threads: HashMap<(ConversationId, ThreadId), Vec<Message>>,
  /// Threads read since the UI was last told about a change; a refresh reloads only these.
  used_threads: HashSet<(ConversationId, ThreadId)>,
  loading: HashSet<LoadKey>,
  /// Loads asked for again while in flight: their answer may predate a change, so they are read
  /// once more when it arrives.
  outdated: HashSet<LoadKey>,
}

/// What a refresh reads again: only what the UI showed since its last change (issue #246).
#[derive(Default)]
struct RefreshPlan {
  is_snapshot_changed: bool,
  pages: Vec<(ConversationId, usize)>,
  /// Open feeds whose total jumped, with the new total: their pages are planned once the rows
  /// held moved to where they are now.
  moved: Vec<(ConversationId, usize)>,
  threads: Vec<(ConversationId, ThreadId)>,
  queries: Vec<(String, ChatQuery)>,
  /// Queries asked too recently to ask again now: they are due after [`MIN_REQUERY_INTERVAL`].
  has_deferred_queries: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum LoadKey {
  Page(ConversationId, usize),
  Thread(ConversationId, ThreadId),
}

type ChangeNotifier = Rc<dyn Fn()>;

/// The answer to one query, kept so the synchronous trait can read it. A change in the core marks
/// answers stale; they are shown as they are until the next read has fetched fresh ones.
#[derive(Default)]
struct Slot {
  /// The query, to ask it again (its key is the query in JSON, which is never read back: the
  /// JSON reader of `ChatQuery` is only in the module two tabs talk through, #1413).
  maybe_query: Option<ChatQuery>,
  maybe_answer: Option<ChatAnswer>,
  is_loading: bool,
  is_stale: bool,
  /// Read by the UI since it was last told about a change: a refresh asks again right away.
  is_used: bool,
  /// The core changed while an answer was in flight: it is asked again when that one arrives.
  is_outdated: bool,
  /// When it was last asked ([`crate::perf::now_ms`]).
  maybe_asked_ms: Option<f64>,
  /// The core changed less than [`MIN_REQUERY_INTERVAL`] after it was asked: asked again once the
  /// interval is over ([`RemoteChat::queue_requery`]).
  is_requery_due: bool,
}

/// The receiver of events the page subscribed to once (see `CORE_EVENTS`).
struct EventRoute<T> {
  is_subscribed: bool,
  maybe_target: Option<T>,
}

#[derive(Clone)]
pub struct RemoteChat {
  mirror: Rc<RefCell<Mirror>>,
  notify: Rc<RefCell<Option<ChangeNotifier>>>,
  /// The newest action failure, for the admin console's inline error (`last_action_error`).
  maybe_error: Rc<RefCell<Option<String>>>,
  refresh_queued: Rc<Cell<bool>>,
  requery_queued: Rc<Cell<bool>>,
  answers: Rc<RefCell<HashMap<String, Slot>>>,
}

impl Feed {
  /// Takes a page read from the core; returns whether any row or the total changed. A page from
  /// after a jump of the total that shows none of the rows held can't say where those went, and
  /// its rows would land among them at the wrong places: it is left out until the newest rows
  /// have told ([`Self::take_newest`]).
  fn apply(&mut self, window: MessageWindow) -> bool {
    if self.is_jump(&window) {
      match self.shift_in(&window) {
        Some(shift) => self.move_rows(shift),
        None => return false,
      }
    }
    self.store(window)
  }

  /// Takes the newest rows after the total jumped: what arrived before the rows held moved them.
  /// Without any of them among these rows (more new messages than a page) they stay where they are.
  fn take_newest(&mut self, window: MessageWindow) -> bool {
    self.is_moving = false;
    self.is_expecting_older = false;
    if let Some(shift) = self.shift_in(&window) {
      self.move_rows(shift);
    }
    self.store(window)
  }

  fn is_jump(&self, window: &MessageWindow) -> bool {
    window.total > self.total + 1 && !self.rows.is_empty()
  }

  /// How far the newest held row that is in `window` moved; `None` when none of them is in it.
  fn shift_in(&self, window: &MessageWindow) -> Option<usize> {
    let places: HashMap<MessageId, usize> = window
      .messages
      .iter()
      .enumerate()
      .map(|(offset, message)| (message.id, window.start + offset))
      .collect();
    self
      .rows
      .iter()
      .rev()
      .find_map(|(index, row)| places.get(&row.id).map(|place| place.saturating_sub(*index)))
  }

  /// `shift` rows arrived before the rows held, or with a hole's page asked for, among them: the
  /// rows after them and the pages read follow.
  fn move_rows(&mut self, shift: usize) {
    if shift == 0 {
      return;
    }
    let GapRows {
      index: from,
      is_before_reader,
    } = self.maybe_gap_rows.take().unwrap_or(GapRows {
      index: 0,
      is_before_reader: true,
    });
    self.rows = std::mem::take(&mut self.rows)
      .into_iter()
      .map(|(index, row)| (if index >= from { index + shift } else { index }, row))
      .collect();
    self.read_pages = moved_pages(&self.read_pages, from, shift);
    self.shown_pages = moved_pages(&self.shown_pages, from, shift);
    if is_before_reader {
      self.prepended += shift;
    }
  }

  /// Appends a message the user just sent, before the core has answered (issue #778). Only when
  /// the feed reaches its newest message and nothing is moving: the row then sits where the core
  /// will put it, and the core's answer replaces it in place (it has the same ID). Returns whether
  /// the row was added.
  fn push_pending(&mut self, message: Message) -> bool {
    let is_at_newest = self.total == 0 || self.rows.contains_key(&(self.total - 1));
    if !is_at_newest || self.is_moving || self.is_expecting_older || self.maybe_gap_rows.is_some() {
      return false;
    }
    self.rows.insert(self.total, message);
    self.total += 1;
    true
  }

  /// Takes back a row of [`Self::push_pending`] the core refused; the rows after it move up.
  fn withdraw_pending(&mut self, id: MessageId) {
    let Some(index) = self
      .rows
      .iter()
      .find_map(|(index, row)| (row.id == id).then_some(*index))
    else {
      return;
    };
    self.rows = std::mem::take(&mut self.rows)
      .into_iter()
      .filter(|(held, _)| *held != index)
      .map(|(held, row)| (if held > index { held - 1 } else { held }, row))
      .collect();
    self.total = self.total.saturating_sub(1);
  }

  fn store(&mut self, window: MessageWindow) -> bool {
    let mut is_changed = self.total != window.total;
    self.total = window.total;
    for (offset, message) in window.messages.into_iter().enumerate() {
      let index = window.start + offset;
      if self.rows.get(&index) != Some(&message) {
        if let Some(previous) = self.rows.get(&index)
          && is_pending(previous.id)
          && !is_pending(message.id)
        {
          // The server took a message sent from here (perf.rs).
          crate::perf::end_pending(
            crate::perf::PerfOperation::MessageSend,
            &previous.id.to_string(),
            crate::perf::PerfStatus::Ok,
          );
        }
        self.rows.insert(index, message);
        is_changed = true;
      }
    }
    is_changed
  }

  /// The pages with a row of `start..end` that is not held.
  fn missing_pages(&self, start: usize, end: usize) -> Vec<usize> {
    if start >= end {
      return Vec::new();
    }
    (start / PAGE..=(end - 1) / PAGE)
      .filter(|page| {
        let rows = (page * PAGE).max(start)..((page + 1) * PAGE).min(end);
        self.rows.range(rows.clone()).count() < rows.len()
      })
      .collect()
  }

  /// Notes that the UI reads (or will read) rows `start..end`.
  fn note_read(&mut self, start: usize, end: usize) {
    if start < end {
      self.read_pages.extend(start / PAGE..=(end - 1) / PAGE);
    }
  }

  /// Notes that the UI shows rows `start..end`.
  fn note_shown(&mut self, start: usize, end: usize) {
    self.note_read(start, end);
    if start < end {
      self.shown_pages.extend(start / PAGE..=(end - 1) / PAGE);
    }
  }

  /// The held pages a refresh reloads: the ones read since the last change and the last one.
  /// The rows of the other pages are dropped; they are out of date after the change and are
  /// loaded again when read.
  fn pages_to_refresh(&mut self) -> Vec<usize> {
    let last = self.total.saturating_sub(1) / PAGE;
    let read_pages = &self.read_pages;
    self
      .rows
      .retain(|index, _| index / PAGE == last || read_pages.contains(&(index / PAGE)));
    self.held_pages()
  }

  /// The pages held, plus the last one (where new messages arrive).
  fn held_pages(&self) -> Vec<usize> {
    let last = self.total.saturating_sub(1) / PAGE;
    let pages: BTreeSet<usize> = self.rows.keys().map(|index| index / PAGE).chain([last]).collect();
    pages.into_iter().collect()
  }
}

/// Where the rows of `pages` are once `shift` rows arrived at `from`: those before it stay.
fn moved_pages(pages: &BTreeSet<usize>, from: usize, shift: usize) -> BTreeSet<usize> {
  pages
    .iter()
    .flat_map(|page| {
      let (start, end) = (page * PAGE, (page + 1) * PAGE - 1);
      let maybe_stays = (start < from).then_some(*page);
      let maybe_moves = (end >= from).then(|| (start.max(from) + shift) / PAGE..=(end + shift) / PAGE);
      maybe_stays.into_iter().chain(maybe_moves.into_iter().flatten())
    })
    .collect()
}

impl Slot {
  /// Takes a fresh answer; returns whether it differs from the one held.
  fn settle(&mut self, answer: ChatAnswer) -> bool {
    self.is_loading = false;
    self.is_stale = false;
    if self.maybe_answer.as_ref() == Some(&answer) {
      return false;
    }
    self.maybe_answer = Some(answer);
    true
  }
}

impl<T: Clone> EventRoute<T> {
  const fn new() -> Self {
    Self {
      is_subscribed: false,
      maybe_target: None,
    }
  }

  /// Makes `target` the receiver; returns whether the caller still has to subscribe (the first
  /// time only).
  fn route_to(&mut self, target: T) -> bool {
    self.maybe_target = Some(target);
    !std::mem::replace(&mut self.is_subscribed, true)
  }

  fn target(&self) -> Option<T> {
    self.maybe_target.clone()
  }

  /// Drops the receiver (sign-out); the subscription stays for the next one.
  fn clear(&mut self) {
    self.maybe_target = None;
  }
}

impl RemoteChat {
  /// Reads the first snapshot and starts following the shell's change events.
  pub async fn connect() -> Result<Self, String> {
    let first = read_snapshot_changes(None).await?;
    let cursor = first.cursor;
    let snapshot = match first.maybe_full {
      Some(full) => *full,
      // Invariant: a reader without a cursor gets the whole snapshot; should a shell not send it,
      // the plain read does.
      None => read_snapshot().await?,
    };
    let backend = Self {
      mirror: Rc::new(RefCell::new(Mirror {
        snapshot,
        cursor,
        feeds: HashMap::new(),
        threads: HashMap::new(),
        used_threads: HashSet::new(),
        loading: HashSet::new(),
        outdated: HashSet::new(),
      })),
      notify: Rc::new(RefCell::new(None)),
      maybe_error: Rc::new(RefCell::new(None)),
      refresh_queued: Rc::new(Cell::new(false)),
      requery_queued: Rc::new(Cell::new(false)),
      answers: Rc::new(RefCell::new(HashMap::new())),
    };
    // async-ok: a thread-local, not a signal.
    let must_subscribe = CORE_EVENTS.with(|route| route.borrow_mut().route_to(backend.clone()));
    if must_subscribe {
      listen(CORE_CHANGED_EVENT, || {
        // async-ok: a thread-local, not a signal.
        let maybe_target = CORE_EVENTS.with(|route| route.borrow().target());
        if let Some(target) = maybe_target {
          target.queue_refresh();
        }
      });
      // Desktop: failures of the core's background commands (the browser core reports its own).
      listen_text(CORE_FAILED_EVENT, |text| {
        report_background_failure(ActionFailure::from_wire(&text))
      });
    }
    Ok(backend)
  }

  /// The answer to a query, or `None` while it is being fetched (the UI re-reads when it arrives).
  /// A stale answer is returned as it is while a fresh one is fetched.
  fn ask(&self, query: ChatQuery) -> Option<ChatAnswer> {
    let key = serde_json::to_string(&query).ok()?;
    let (existing, must_load) = {
      let mut answers = self.answers.borrow_mut();
      let slot = answers.entry(key.clone()).or_default();
      slot.maybe_query.get_or_insert_with(|| query.clone());
      slot.is_used = true;
      let must_load = !slot.is_loading && (slot.maybe_answer.is_none() || slot.is_stale);
      if must_load {
        slot.is_loading = true;
      }
      (slot.maybe_answer.clone(), must_load)
    };
    if must_load {
      let backend = self.clone();
      spawn_local(async move {
        if backend.fetch_answer(key, query).await {
          backend.changed();
        }
      });
    }
    existing
  }

  /// Everything fetched so far may be out of date.
  fn mark_answers_stale(&self) {
    for slot in self.answers.borrow_mut().values_mut() {
      slot.is_stale = true;
      // An answer in flight may predate the change: asked again when it arrives.
      slot.is_outdated |= slot.is_loading;
    }
  }

  /// Asks the core for one answer again; returns whether it differs from the one held. Asks once
  /// more when the core changed while the answer was in flight.
  async fn fetch_answer(&self, key: String, query: ChatQuery) -> bool {
    let mut is_changed = false;
    loop {
      if let Some(slot) = self.answers.borrow_mut().get_mut(&key) {
        slot.maybe_asked_ms = Some(crate::perf::now_ms());
      }
      let outcome = core_query(query.clone()).await;
      let mut answers = self.answers.borrow_mut();
      let slot = answers.entry(key.clone()).or_default();
      slot.maybe_query.get_or_insert_with(|| query.clone());
      match outcome {
        Ok(answer) => is_changed |= slot.settle(answer),
        Err(error) => {
          tracing::warn!(%error, "a query failed");
          // The old answer stays and nothing is asked again until something changes.
          slot.is_loading = false;
          slot.is_stale = false;
        },
      }
      if !std::mem::take(&mut slot.is_outdated) {
        return is_changed;
      }
      slot.is_loading = true;
    }
  }

  /// Tells the UI to read again. What it reads is marked used anew, so a later refresh reloads
  /// only what is still shown.
  fn changed(&self) {
    for slot in self.answers.borrow_mut().values_mut() {
      slot.is_used = false;
    }
    {
      let mut mirror = self.mirror.borrow_mut();
      mirror.used_threads.clear();
      for feed in mirror.feeds.values_mut() {
        feed.is_used = false;
        feed.read_pages.clear();
        feed.shown_pages.clear();
      }
    }
    let notify = self.notify.borrow().clone();
    if let Some(notify) = notify {
      notify();
    }
  }

  fn queue_refresh(&self) {
    if self.refresh_queued.replace(true) {
      return;
    }
    let backend = self.clone();
    set_timeout(
      move || {
        // async-ok: a Cell, not a signal.
        backend.refresh_queued.set(false);
        spawn_local(async move { backend.refresh().await });
      },
      REFRESH_DELAY,
    );
  }

  /// Asks the queries a change of the core found too recently asked once [`MIN_REQUERY_INTERVAL`]
  /// is over: one timer however many changes came meanwhile.
  fn queue_requery(&self) {
    if self.requery_queued.replace(true) {
      return;
    }
    let backend = self.clone();
    set_timeout(
      move || {
        // async-ok: a Cell, not a signal.
        backend.requery_queued.set(false);
        spawn_local(async move { backend.requery_due().await });
      },
      MIN_REQUERY_INTERVAL,
    );
  }

  async fn requery_due(&self) {
    let due = self.take_due_queries();
    let changed = join_all(due.into_iter().map(|(key, query)| self.fetch_answer(key, query))).await;
    if changed.into_iter().any(|is_changed| is_changed) {
      self.changed();
    }
  }

  /// The queries due again, marked loading; one still in flight is asked again when it arrives.
  fn take_due_queries(&self) -> Vec<(String, ChatQuery)> {
    let mut due = Vec::new();
    for (key, slot) in self.answers.borrow_mut().iter_mut() {
      if !std::mem::take(&mut slot.is_requery_due) {
        continue;
      }
      if slot.is_loading {
        slot.is_outdated = true;
        continue;
      }
      if let Some(query) = slot.maybe_query.clone() {
        slot.is_loading = true;
        due.push((key.clone(), query));
      }
    }
    due
  }

  /// Takes what changed in the snapshot, re-reads what the UI showed of the conversations that
  /// changed, and tells the UI only when something differs: a change elsewhere in the core costs
  /// no re-render, and a change in one conversation no re-read of the others (issue #246).
  async fn refresh(&self) {
    let refresh_ms = crate::perf::now_ms();
    let since = self.mirror.borrow().cursor;
    let changes = match read_snapshot_changes(Some(since)).await {
      Ok(changes) => changes,
      Err(error) => {
        tracing::warn!(%error, "could not refresh the chat snapshot");
        return;
      },
    };
    let mut plan = self.plan_refresh(changes, crate::perf::now_ms());
    if plan.has_deferred_queries {
      self.queue_requery();
    }
    // First, and before the pages: their numbers are valid once the rows held moved.
    let moves = join_all(
      plan
        .moved
        .iter()
        .map(|(conversation, total)| self.follow_moved_rows(*conversation, *total)),
    )
    .await;
    plan.pages.extend(self.pages_of_moved_feeds(&plan.moved));
    let pages = join_all(
      plan
        .pages
        .into_iter()
        .map(|(conversation, page)| self.fetch_page(conversation, page)),
    );
    let threads = join_all(
      plan
        .threads
        .into_iter()
        .map(|(conversation, thread)| self.fetch_thread(conversation, thread)),
    );
    let answers = join_all(
      plan
        .queries
        .into_iter()
        .map(|(key, query)| self.fetch_answer(key, query)),
    );
    let (pages, threads, answers) = join3(pages, threads, answers).await;
    let is_changed = plan.is_snapshot_changed
      || moves.into_iter().any(|is_changed| is_changed)
      || pages.into_iter().any(|is_changed| is_changed)
      || threads.into_iter().any(|is_changed| is_changed)
      || answers.into_iter().any(|is_changed| is_changed);
    if is_changed {
      let handed_ms = crate::perf::now_ms();
      self.changed();
      // Rows of a page of older history moved in: its measure ends once they are painted.
      for (conversation, _) in &plan.moved {
        crate::perf::older_page_shown(&conversation.to_string(), refresh_ms, handed_ms);
      }
    }
  }

  /// Takes what changed and picks what to reload: feeds and threads of the conversations that
  /// changed, if the UI read them since its last change; the others of those conversations are
  /// only marked stale and reloaded when read again. Queries are asked again whenever the core
  /// changed (they read more than the snapshot).
  fn plan_refresh(&self, changes: SnapshotChanges, now_ms: f64) -> RefreshPlan {
    let mut plan = RefreshPlan::default();
    let applied = {
      let mut mirror = self.mirror.borrow_mut();
      let mirror = &mut *mirror;
      mirror.cursor = changes.cursor;
      let applied = mirror.snapshot.apply(changes);
      plan.is_snapshot_changed = applied.is_changed;
      // The feeds the UI opened, not every conversation's count (issue #246). A jump in an open
      // one may be older history arriving (#1132): its rows are moved before anything reads them.
      // One new row, the common case, needs no look and no extra read.
      for (conversation, feed) in &mut mirror.feeds {
        if applied.touches(*conversation)
          && let Some(total) = mirror.snapshot.message_count(*conversation)
        {
          let is_expecting = feed.is_expecting_older || feed.maybe_gap_rows.is_some();
          let is_jump = total > feed.total + 1 || (is_expecting && total > feed.total);
          if feed.is_used && !feed.rows.is_empty() && is_jump {
            feed.is_moving = true;
            plan.moved.push((*conversation, total));
          } else {
            feed.total = total;
          }
        }
      }
      let touched_threads: Vec<(ConversationId, ThreadId)> = mirror
        .used_threads
        .iter()
        .filter(|(conversation, _)| applied.touches(*conversation))
        .copied()
        .collect();
      let used_threads = &mirror.used_threads;
      mirror
        .threads
        .retain(|key, _| !applied.touches(key.0) || used_threads.contains(key));
      for key in &touched_threads {
        mirror.used_threads.remove(key);
      }
      plan.threads = touched_threads;
      for (conversation, feed) in &mut mirror.feeds {
        if !applied.touches(*conversation) {
          continue;
        }
        if feed.is_moving {
          continue;
        }
        if feed.is_used {
          plan.pages.extend(
            feed
              .pages_to_refresh()
              .into_iter()
              .map(|page| (*conversation, page)),
          );
        } else {
          feed.is_stale = true;
        }
      }
      applied
    };
    for (key, slot) in self.answers.borrow_mut().iter_mut() {
      let maybe_query = slot.maybe_query.clone();
      // A message link's preview only changes with its own conversation: other changes keep
      // the answer it has (#319, no rescans on every message elsewhere).
      if let Some(ChatQuery::LinkedMessage { link }) = &maybe_query
        && slot.maybe_answer.is_some()
        && !slot.is_loading
        && Route::from_permalink(link)
          .and_then(|route| route.conversation())
          .is_some_and(|conversation| !applied.touches(conversation))
      {
        continue;
      }
      let is_recent = slot
        .maybe_asked_ms
        .is_some_and(|asked_ms| now_ms - asked_ms < MIN_REQUERY_INTERVAL.as_secs_f64() * 1_000.0);
      if is_recent && (slot.is_loading || slot.is_used) {
        slot.is_requery_due = true;
        plan.has_deferred_queries = true;
        continue;
      }
      if slot.is_loading {
        slot.is_outdated = true;
        continue;
      }
      match (slot.is_used, maybe_query) {
        (true, Some(query)) => {
          slot.is_loading = true;
          plan.queries.push((key.clone(), query));
        },
        _ => slot.is_stale = true,
      }
    }
    plan
  }

  /// Reads the newest rows of a feed whose total jumped to `total`, which tell where the rows held
  /// are now; returns whether anything changed.
  async fn follow_moved_rows(&self, conversation: ConversationId, total: usize) -> bool {
    let outcome = read_messages(conversation, total.saturating_sub(PAGE), PAGE).await;
    let mut mirror = self.mirror.borrow_mut();
    let feed = mirror.feeds.entry(conversation).or_default();
    match outcome {
      Ok(window) => feed.take_newest(window),
      Err(error) => {
        tracing::warn!(%error, %conversation, "could not load messages");
        feed.is_moving = false;
        feed.total = total;
        true
      },
    }
  }

  /// What a refresh reloads of the feeds whose rows just moved.
  fn pages_of_moved_feeds(&self, moved: &[(ConversationId, usize)]) -> Vec<(ConversationId, usize)> {
    let mut mirror = self.mirror.borrow_mut();
    moved
      .iter()
      .flat_map(|(conversation, _)| {
        let pages = mirror
          .feeds
          .get_mut(conversation)
          .map_or_else(Vec::new, Feed::pages_to_refresh);
        pages.into_iter().map(|page| (*conversation, page))
      })
      .collect()
  }

  fn load_page(&self, conversation: ConversationId, page: usize) {
    let backend = self.clone();
    spawn_local(async move {
      if backend.fetch_page(conversation, page).await {
        backend.changed();
      }
    });
  }

  /// Loads a page the UI will read soon. Its rows are read when they come into view; the UI is
  /// told only if it already shows one of them. Telling it re-reads everything shown, a long task
  /// in the frame the page arrives in (issue #1050).
  fn load_page_ahead(&self, conversation: ConversationId, page: usize) {
    let backend = self.clone();
    spawn_local(async move {
      let total_before = backend.feed_total(conversation);
      if backend.fetch_page(conversation, page).await
        && (backend.is_shown(conversation, page) || backend.feed_total(conversation) != total_before)
      {
        backend.changed();
      }
    });
  }

  fn is_shown(&self, conversation: ConversationId, page: usize) -> bool {
    let mirror = self.mirror.borrow();
    mirror
      .feeds
      .get(&conversation)
      .is_some_and(|feed| feed.shown_pages.contains(&page))
  }

  fn feed_total(&self, conversation: ConversationId) -> usize {
    let mirror = self.mirror.borrow();
    mirror.feeds.get(&conversation).map_or(0, |feed| feed.total)
  }

  /// Reads one page of a feed; returns whether it differs from the rows held.
  async fn fetch_page(&self, conversation: ConversationId, page: usize) -> bool {
    let key = LoadKey::Page(conversation, page);
    if !self.begin_load(key) {
      return false;
    }
    let mut is_changed = false;
    loop {
      let started_ms = crate::perf::now_ms();
      let outcome = read_messages(conversation, page * PAGE, PAGE).await;
      // A switch whose rows weren't in the mirror: the read from the core's store is a step of it.
      crate::perf::with_pending(
        crate::perf::PerfOperation::ChannelSwitch,
        &conversation.to_string(),
        |measure| {
          let status = if outcome.is_ok() {
            crate::perf::PerfStatus::Ok
          } else {
            crate::perf::PerfStatus::Error
          };
          measure.span("db.query", "core_messages", started_ms, status);
          measure.tag("source", "store");
        },
      );
      let mut mirror = self.mirror.borrow_mut();
      match outcome {
        Ok(window) => is_changed |= mirror.feeds.entry(conversation).or_default().apply(window),
        Err(error) => tracing::warn!(%error, %conversation, "could not load messages"),
      }
      if !mirror.outdated.remove(&key) {
        mirror.loading.remove(&key);
        return is_changed;
      }
    }
  }

  /// Marks a load as in flight; `false` when it already is (it is then read once more, since the
  /// caller may know of a change the load in flight predates).
  fn begin_load(&self, key: LoadKey) -> bool {
    let mut mirror = self.mirror.borrow_mut();
    if mirror.loading.insert(key) {
      return true;
    }
    mirror.outdated.insert(key);
    false
  }

  fn load_thread(&self, conversation: ConversationId, thread: ThreadId) {
    let backend = self.clone();
    spawn_local(async move {
      if backend.fetch_thread(conversation, thread).await {
        backend.changed();
      }
    });
  }

  /// Reads one thread; returns whether it differs from the one held.
  async fn fetch_thread(&self, conversation: ConversationId, thread: ThreadId) -> bool {
    let key = LoadKey::Thread(conversation, thread);
    if !self.begin_load(key) {
      return false;
    }
    let mut is_changed = false;
    loop {
      let outcome = read_thread(conversation, thread).await;
      let mut mirror = self.mirror.borrow_mut();
      match outcome {
        Ok(messages) => {
          is_changed |= mirror.threads.get(&(conversation, thread)) != Some(&messages);
          mirror.threads.insert((conversation, thread), messages);
        },
        Err(error) => tracing::warn!(%error, "could not load a thread"),
      }
      if !mirror.outdated.remove(&key) {
        mirror.loading.remove(&key);
        return is_changed;
      }
    }
  }

  /// Hands an action to the core. A failure is shown to the user (issue #322); a button waiting
  /// for the action keeps its ticket until the core answered.
  fn act(&self, action: ChatAction) {
    self.act_showing(action, None);
  }

  /// [`Self::act`] for an action whose effect is already shown ([`Self::show_pending`]): a refusal
  /// takes the row back.
  fn act_showing(&self, action: ChatAction, maybe_pending: Option<PendingRow>) {
    let backend = self.clone();
    let maybe_ticket = ActionTicket::current();
    spawn_local(async move {
      let outcome = core_action(action).await;
      *backend.maybe_error.borrow_mut() = outcome.as_ref().err().cloned();
      if let Err(error) = outcome {
        if let Some(pending) = maybe_pending {
          backend.withdraw_pending(pending);
        }
        let failure = ActionFailure::from_wire(&error);
        if let Some(ticket) = &maybe_ticket {
          ticket.fail(failure.clone());
        }
        report_failure(failure);
      } else {
        // The user's own change shows as soon as the core has it (a sent message as pending),
        // not after the debounce for the core's change events (#717).
        backend.refresh().await;
      }
      backend.mark_answers_stale();
      backend.changed();
      // Dropped only now, so the button stays pending until the change is visible.
      drop(maybe_ticket);
    });
  }

  /// Puts a plain message the user just sent into the mirror, in the same frame: its row is the
  /// outbox message the core writes under the same client ID, so the core's answer replaces it in
  /// place (issue #778). Messages whose row needs more than the text (a reply's quote, files,
  /// urgency) appear with the core's answer.
  fn show_pending(&self, message: &OutgoingMessage, id: ClientMessageId) -> Option<PendingRow> {
    let is_plain = message.files.is_empty()
      && message.maybe_reply_to.is_none()
      && message.priority == MessagePriority::Normal
      && !message.wants_acknowledgement
      && !message.is_silent
      && !message.also_send_to_channel
      && !message.body.trim().is_empty();
    if !is_plain {
      return None;
    }
    let pending = PendingRow {
      conversation: message.conversation,
      maybe_thread: message.maybe_thread,
      id: pending_message_id(id),
    };
    let is_shown = {
      let mut mirror = self.mirror.borrow_mut();
      let row = Message {
        id: pending.id,
        conversation: message.conversation,
        author: mirror.snapshot.me.id,
        kind: MessageKind::User,
        sent_at: crate::clock::now_ms(),
        body: message.body.clone(),
        is_edited: false,
        reactions: Vec::new(),
        reply_count: 0,
        maybe_thread: message.maybe_thread,
        maybe_imported: None,
        maybe_source: None,
        maybe_placeholder: None,
        group_mentions: Vec::new(),
        custom_emoji: Vec::new(),
        mentions_me: false,
        maybe_poll: None,
      };
      match message.maybe_thread {
        Some(thread) => mirror
          .threads
          .get_mut(&(message.conversation, thread))
          .map(|replies| replies.push(row))
          .is_some(),
        None => mirror
          .feeds
          .get_mut(&message.conversation)
          .is_some_and(|feed| feed.push_pending(row)),
      }
    };
    if is_shown {
      self.changed();
      Some(pending)
    } else {
      None
    }
  }

  fn withdraw_pending(&self, pending: PendingRow) {
    crate::perf::end_pending(
      crate::perf::PerfOperation::MessageSend,
      &pending.id.to_string(),
      crate::perf::PerfStatus::Error,
    );
    let mut mirror = self.mirror.borrow_mut();
    match pending.maybe_thread {
      Some(thread) => {
        if let Some(replies) = mirror.threads.get_mut(&(pending.conversation, thread)) {
          replies.retain(|row| row.id != pending.id);
        }
      },
      None => {
        if let Some(feed) = mirror.feeds.get_mut(&pending.conversation) {
          feed.withdraw_pending(pending.id);
        }
      },
    }
  }

  fn update_conversation(&self, conversation: ConversationId, change: impl FnOnce(&mut Conversation)) {
    let mut mirror = self.mirror.borrow_mut();
    if let Some(found) = mirror.snapshot.conversation_mut(conversation) {
      change(found);
    }
  }
}

impl ChatBackend for RemoteChat {
  fn can_sign_workspace(&self) -> bool {
    true
  }

  fn set_activity(&self, is_idle: bool) {
    self.act(ChatAction::SetActivity { is_idle });
  }

  fn set_attention(&self, is_attentive: bool) {
    self.act(ChatAction::SetAttention { is_attentive });
  }

  fn focus_conversation(&self, conversation: ConversationId) {
    spawn_local(async move {
      // A hint for the catch-up order; nothing for the user to act on if it is lost.
      if let Err(error) = call::<()>("core_focus", &json!({ "conversation": conversation })).await {
        tracing::debug!(%error, "the core did not take the focus");
      }
    });
  }

  fn search_backend(&self) -> Option<&dyn SearchBackend> {
    Some(self)
  }

  fn send_message(&self, mut message: OutgoingMessage) {
    // The ID is the UI's, so the row can be shown before the core has answered (issue #778).
    let id = *message
      .maybe_client_message_id
      .get_or_insert_with(ClientMessageId::random);
    let maybe_pending = self.show_pending(&message, id);
    // Performance data: until the server's answer replaces the pending row (`Feed::store`).
    if let Some(pending) = maybe_pending
      .as_ref()
      .filter(|pending| pending.maybe_thread.is_none())
    {
      crate::perf::begin_pending(crate::perf::PerfOperation::MessageSend, &pending.id.to_string());
    }
    self.act_showing(ChatAction::SendMessage { message }, maybe_pending);
  }

  fn message_extras(&self, conversation: ConversationId, messages: &[MessageId]) -> Vec<MessageExtras> {
    match self.ask(ChatQuery::MessageExtras {
      conversation,
      messages: messages.to_vec(),
    }) {
      Some(ChatAnswer::MessageExtras { extras }) => extras,
      _ => Vec::new(),
    }
  }

  fn edit_message(&self, conversation: ConversationId, message: MessageId, body: &str) {
    self.act(ChatAction::EditMessage {
      conversation,
      message,
      body: body.to_owned(),
    });
  }

  fn delete_message(&self, conversation: ConversationId, message: MessageId) {
    self.act(ChatAction::DeleteMessage {
      conversation,
      message,
    });
  }

  fn forward_message(&self, from: ConversationId, message: MessageId, to: ConversationId, comment: &str) {
    self.act(ChatAction::ForwardMessage {
      from,
      message,
      to,
      comment: comment.to_owned(),
      maybe_attachment: None,
    });
  }

  fn forward_attachment(
    &self,
    from: ConversationId,
    message: MessageId,
    attachment: AttachmentId,
    to: ConversationId,
    comment: &str,
  ) {
    self.act(ChatAction::ForwardMessage {
      from,
      message,
      to,
      comment: comment.to_owned(),
      maybe_attachment: Some(attachment),
    });
  }

  fn set_pinned(&self, conversation: ConversationId, message: MessageId, is_pinned: bool) {
    self.act(ChatAction::SetPinned {
      conversation,
      message,
      is_pinned,
    });
  }

  fn post_poll(&self, conversation: ConversationId, poll: teamcraft_client_api::polls::NewPoll) {
    self.act(ChatAction::PostPoll { conversation, poll });
  }

  fn vote_in_poll(&self, conversation: ConversationId, message: MessageId, choices: Vec<u32>) {
    self.act(ChatAction::VoteInPoll {
      conversation,
      message,
      choices,
    });
  }

  fn close_poll(&self, conversation: ConversationId, message: MessageId) {
    self.act(ChatAction::ClosePoll {
      conversation,
      message,
    });
  }

  fn pinned_messages(&self, conversation: ConversationId) -> Vec<Message> {
    match self.ask(ChatQuery::PinnedMessages { conversation }) {
      Some(ChatAnswer::Messages { messages }) => messages,
      _ => Vec::new(),
    }
  }

  fn set_saved(&self, conversation: ConversationId, message: MessageId, is_saved: bool) {
    self.act(ChatAction::SetSaved {
      conversation,
      message,
      is_saved,
    });
  }

  fn saved_items(&self) -> Vec<SavedItem> {
    match self.ask(ChatQuery::SavedItems) {
      Some(ChatAnswer::SavedItems { items }) => items,
      _ => Vec::new(),
    }
  }

  fn background_images(&self) -> Vec<BackgroundImageInfo> {
    match self.ask(ChatQuery::BackgroundImages) {
      Some(ChatAnswer::BackgroundImages { images }) => images,
      _ => Vec::new(),
    }
  }

  fn add_background_image(&self, name: &str, picture: Vec<u8>) {
    self.act(ChatAction::AddBackgroundImage {
      name: name.to_owned(),
      picture: picture.into(),
    });
  }

  fn add_org_background(&self, name: &str, picture: Vec<u8>) {
    self.act(ChatAction::AddOrgBackground {
      name: name.to_string(),
      picture: picture.into(),
    });
  }

  fn delete_background_image(&self, id: BackgroundId) {
    self.act(ChatAction::DeleteBackgroundImage { id });
  }

  fn delete_org_background(&self, id: OrgBackgroundId) {
    self.act(ChatAction::DeleteOrgBackground { id });
  }

  fn set_org_background_rule(&self, rule: OrgBackgroundRule) {
    self.act(ChatAction::SetOrgBackgroundRule { rule });
  }

  fn custom_emoji(&self) -> teamcraft_client_api::custom_emoji::CustomEmojiSet {
    match self.ask(ChatQuery::CustomEmoji) {
      Some(ChatAnswer::CustomEmoji { set }) => set,
      _ => teamcraft_client_api::custom_emoji::CustomEmojiSet::default(),
    }
  }

  fn custom_emoji_picture(&self, sha256: teamcraft_proto::CustomEmojiHash) -> Option<Vec<u8>> {
    match self.ask(ChatQuery::CustomEmojiPicture { sha256 }) {
      Some(ChatAnswer::CustomEmojiPicture { maybe_picture }) => {
        maybe_picture.map(teamcraft_proto::Base64Bytes::into_vec)
      },
      _ => None,
    }
  }

  fn want_custom_emoji(&self) {
    self.act(ChatAction::WantCustomEmoji);
  }

  fn add_custom_emoji(&self, shortcode: &str, picture: Vec<u8>) {
    self.act(ChatAction::AddCustomEmoji {
      shortcode: shortcode.to_owned(),
      picture: picture.into(),
    });
  }

  fn rename_custom_emoji(&self, id: teamcraft_proto::CustomEmojiId, shortcode: &str) {
    self.act(ChatAction::RenameCustomEmoji {
      id,
      shortcode: shortcode.to_owned(),
    });
  }

  fn delete_custom_emoji(&self, id: teamcraft_proto::CustomEmojiId) {
    self.act(ChatAction::DeleteCustomEmoji { id });
  }

  fn permalink(&self, conversation: ConversationId, message: MessageId) -> Option<String> {
    match self.ask(ChatQuery::Permalink {
      conversation,
      message,
    }) {
      Some(ChatAnswer::Link { maybe_link }) => maybe_link,
      _ => None,
    }
  }

  fn resolve_permalink(&self, link: &str) -> Option<PermalinkTarget> {
    match self.ask(ChatQuery::ResolvePermalink {
      link: link.to_owned(),
    }) {
      Some(ChatAnswer::PermalinkTarget { maybe_target }) => maybe_target,
      _ => None,
    }
  }

  fn linked_message(&self, link: &str) -> Option<LinkedMessage> {
    match self.ask(ChatQuery::LinkedMessage {
      link: link.to_owned(),
    }) {
      Some(ChatAnswer::LinkedMessage { maybe_linked }) => maybe_linked,
      _ => None,
    }
  }

  fn mark_unread(&self, conversation: ConversationId, from: MessageId) {
    self.act(ChatAction::MarkUnread { conversation, from });
  }

  fn is_waiting_for_device(&self, conversation: ConversationId) -> bool {
    matches!(
      self.ask(ChatQuery::IsWaitingForDevice { conversation }),
      Some(ChatAnswer::Flag { value: true })
    )
  }

  fn first_sync(&self) -> Option<teamcraft_client_api::model::FirstSync> {
    match self.ask(ChatQuery::FirstSync) {
      Some(ChatAnswer::FirstSync { maybe_progress }) => maybe_progress,
      _ => None,
    }
  }

  fn key_catch_up(&self) -> Option<teamcraft_client_api::model::KeyCatchUp> {
    match self.ask(ChatQuery::KeyCatchUp) {
      Some(ChatAnswer::KeyCatchUp { maybe_progress }) => maybe_progress,
      _ => None,
    }
  }

  fn mentionable_groups(
    &self,
    conversation: ConversationId,
  ) -> Vec<teamcraft_client_api::model::MentionableGroup> {
    match self.ask(ChatQuery::MentionableGroups { conversation }) {
      Some(ChatAnswer::MentionableGroups { groups }) => groups,
      _ => Vec::new(),
    }
  }

  fn membership_issues(
    &self,
    conversation: ConversationId,
  ) -> Vec<teamcraft_client_api::model::MembershipIssue> {
    match self.ask(ChatQuery::MembershipIssues { conversation }) {
      Some(ChatAnswer::MembershipIssues { issues }) => issues,
      _ => Vec::new(),
    }
  }

  fn retry_removal(&self, conversation: ConversationId, user: UserId) {
    self.act(ChatAction::RetryRemoval { conversation, user });
  }

  fn retry_dm_setup(&self, conversation: ConversationId) {
    self.act(ChatAction::RetryDmSetup { conversation });
  }

  fn set_up_meeting_chat(&self, conversation: ConversationId, invitees: Vec<UserId>) {
    self.act(ChatAction::SetUpMeetingChat {
      conversation,
      invitees,
    });
  }

  fn update_meeting_chat_members(
    &self,
    conversation: ConversationId,
    invitees: Vec<UserId>,
    removed: Vec<UserId>,
  ) {
    self.act(ChatAction::UpdateMeetingChatMembers {
      conversation,
      invitees,
      removed,
    });
  }

  fn add_people_to_call(
    &self,
    from: ConversationId,
    in_call: Vec<UserId>,
    added: Vec<UserId>,
    maybe_title: Option<String>,
    key: teamcraft_proto::IdempotencyKey,
  ) {
    self.act(ChatAction::AddPeopleToCall {
      from,
      in_call,
      added,
      maybe_title,
      key,
    });
  }

  fn let_into_meeting_chat(&self, conversation: ConversationId, user: UserId) {
    self.act(ChatAction::LetIntoMeetingChat { conversation, user });
  }

  fn prepare_guest_link(&self, conversation: ConversationId, meeting: teamcraft_proto::MeetingId) {
    self.act(ChatAction::PrepareGuestLink {
      conversation,
      meeting,
    });
  }

  fn guest_link_fragment(&self, meeting: teamcraft_proto::MeetingId) -> Option<String> {
    match self.ask(ChatQuery::GuestLinkFragment { meeting }) {
      Some(ChatAnswer::Link { maybe_link }) => maybe_link,
      _ => None,
    }
  }

  fn let_guest_into_meeting_chat(
    &self,
    conversation: ConversationId,
    guest: UserId,
    knock_fingerprint: String,
    access_ends_at_ms: i64,
  ) {
    self.act(ChatAction::LetGuestIntoMeetingChat {
      conversation,
      guest,
      knock_fingerprint,
      access_ends_at_ms,
    });
  }

  fn prepare_guest_key_code(&self, guest: UserId) {
    self.act(ChatAction::PrepareGuestKeyCode { guest });
  }

  fn guest_key_code(&self, guest: UserId) -> Option<String> {
    match self.ask(ChatQuery::GuestKeyCode { guest }) {
      Some(ChatAnswer::Link { maybe_link }) => maybe_link,
      _ => None,
    }
  }

  fn note_meeting_lobby(&self, meeting: teamcraft_proto::MeetingId, has_lobby: bool) {
    self.act(ChatAction::NoteMeetingLobby { meeting, has_lobby });
  }

  fn is_open_by_own_record(&self, meeting: teamcraft_proto::MeetingId) -> bool {
    matches!(
      self.ask(ChatQuery::IsOpenByOwnRecord { meeting }),
      Some(ChatAnswer::Flag { value: true })
    )
  }

  fn sync_meeting_admitters(
    &self,
    meeting: teamcraft_proto::MeetingId,
    conversation: ConversationId,
    expires_at_ms: i64,
  ) {
    self.act(ChatAction::SyncMeetingAdmitters {
      meeting,
      conversation,
      expires_at_ms,
    });
  }

  fn check_meeting_admission(&self, conversation: ConversationId) {
    self.act(ChatAction::CheckMeetingAdmission { conversation });
  }

  fn admits_link_arrivals(&self, conversation: ConversationId, now_ms: i64) -> bool {
    matches!(
      self.ask(ChatQuery::AdmitsLinkArrivals { conversation, now_ms }),
      Some(ChatAnswer::Flag { value: true })
    )
  }

  fn change_meeting_hosts(&self, change: teamcraft_client_api::meeting_hosts::MeetingHostsChange) {
    self.act(ChatAction::ChangeMeetingHosts { change });
  }

  fn meeting_hosts_change(
    &self,
    conversation: ConversationId,
  ) -> Option<teamcraft_client_api::meeting_hosts::HostsChangeState> {
    match self.ask(ChatQuery::MeetingHostsChange { conversation }) {
      Some(ChatAnswer::HostsChange { maybe_state }) => maybe_state,
      _ => None,
    }
  }

  fn discard_removal(&self, conversation: ConversationId, user: UserId) {
    self.act(ChatAction::DiscardRemoval { conversation, user });
  }

  fn unconfirmed_removals(&self) -> Vec<(ConversationId, UserId)> {
    match self.ask(ChatQuery::UnconfirmedRemovals) {
      Some(ChatAnswer::UnconfirmedRemovals { removals }) => removals,
      _ => Vec::new(),
    }
  }

  fn group_leftovers(&self) -> Vec<(ConversationId, UserId)> {
    match self.ask(ChatQuery::GroupLeftovers) {
      Some(ChatAnswer::GroupLeftovers { leftovers }) => leftovers,
      _ => Vec::new(),
    }
  }

  fn first_unread(&self, conversation: ConversationId) -> Option<MessageId> {
    match self.ask(ChatQuery::FirstUnread { conversation }) {
      Some(ChatAnswer::Message { maybe_message }) => maybe_message,
      _ => None,
    }
  }

  fn unseen_reactions(&self, conversation: ConversationId) -> UnseenReactions {
    match self.ask(ChatQuery::UnseenReactions { conversation }) {
      Some(ChatAnswer::UnseenReactions { unseen }) => unseen,
      _ => UnseenReactions::default(),
    }
  }

  fn expiring_guests(&self) -> Vec<teamcraft_proto::ExpiringGuestResponse> {
    match self.ask(ChatQuery::ExpiringGuests) {
      Some(ChatAnswer::ExpiringGuests { guests }) => guests,
      _ => Vec::new(),
    }
  }

  fn extend_guest(&self, space: SpaceId, user: UserId, expires_at_ms: i64) {
    self.act(ChatAction::ExtendGuest {
      space,
      user,
      expires_at_ms,
    });
  }

  fn mark_reactions_seen(&self, conversation: ConversationId, messages: Vec<MessageId>) {
    self.act(ChatAction::MarkReactionsSeen {
      conversation,
      messages,
    });
  }

  fn newest_message(&self, conversation: ConversationId) -> Option<MessageId> {
    match self.ask(ChatQuery::NewestMessage { conversation }) {
      Some(ChatAnswer::Message { maybe_message }) => maybe_message,
      _ => None,
    }
  }

  fn draft(&self, conversation: ConversationId, maybe_thread: Option<ThreadId>) -> Option<String> {
    match self.ask(ChatQuery::Draft {
      conversation,
      maybe_thread,
    }) {
      Some(ChatAnswer::Draft { maybe_body }) => maybe_body,
      _ => None,
    }
  }

  fn draft_files(
    &self,
    conversation: ConversationId,
    maybe_thread: Option<ThreadId>,
  ) -> Vec<teamcraft_client_api::drafts::DraftFileItem> {
    match self.ask(ChatQuery::DraftFiles {
      conversation,
      maybe_thread,
    }) {
      Some(ChatAnswer::DraftFiles { files }) => files,
      _ => Vec::new(),
    }
  }

  fn attach_draft_files(
    &self,
    conversation: ConversationId,
    maybe_thread: Option<ThreadId>,
    files: Vec<teamcraft_client_api::actions::OutgoingFile>,
    travel: teamcraft_client_api::drafts::DraftFileTravel,
  ) {
    self.act(ChatAction::AttachDraftFiles {
      conversation,
      maybe_thread,
      files,
      travel,
    });
  }

  fn discard_draft(&self, conversation: ConversationId, maybe_thread: Option<ThreadId>) {
    self.act(ChatAction::DiscardDraft {
      conversation,
      maybe_thread,
    });
  }

  fn remove_draft_file(&self, conversation: ConversationId, maybe_thread: Option<ThreadId>, id: &str) {
    self.act(ChatAction::RemoveDraftFile {
      conversation,
      maybe_thread,
      id: id.to_owned(),
    });
  }

  fn drafts(&self, maybe_space: Option<SpaceId>) -> Vec<teamcraft_client_api::drafts::DraftItem> {
    match self.ask(ChatQuery::Drafts { maybe_space }) {
      Some(ChatAnswer::Drafts { drafts }) => drafts,
      _ => Vec::new(),
    }
  }

  fn sent_messages(&self, maybe_space: Option<SpaceId>) -> Vec<teamcraft_client_api::drafts::SentItem> {
    match self.ask(ChatQuery::SentMessages { maybe_space }) {
      Some(ChatAnswer::SentMessages { sent }) => sent,
      _ => Vec::new(),
    }
  }

  fn set_draft(&self, conversation: ConversationId, maybe_thread: Option<ThreadId>, body: &str) {
    self.act(ChatAction::SetDraft {
      conversation,
      maybe_thread,
      body: body.to_owned(),
    });
  }

  fn scheduled_overview(&self) -> ScheduledOverview {
    match self.ask(ChatQuery::ScheduledMessages) {
      Some(ChatAnswer::ScheduledMessages { overview }) => overview,
      _ => ScheduledOverview::default(),
    }
  }

  fn schedule_message(&self, message: OutgoingMessage, due_at: teamcraft_client_api::model::TimestampMs) {
    self.act(ChatAction::ScheduleMessage { message, due_at });
  }

  fn update_scheduled(
    &self,
    id: ScheduledMessageId,
    body: &str,
    due_at: teamcraft_client_api::model::TimestampMs,
  ) {
    self.act(ChatAction::UpdateScheduled {
      id,
      body: body.to_owned(),
      due_at,
    });
  }

  fn cancel_scheduled(&self, id: ScheduledMessageId) {
    self.act(ChatAction::CancelScheduled { id });
  }

  fn send_scheduled_now(&self, id: ScheduledMessageId) {
    self.act(ChatAction::SendScheduledNow { id });
  }

  fn send_bug_report(&self, report: teamcraft_client_api::bug_report::BugReport) {
    self.act(ChatAction::SendBugReport { report });
  }

  fn bug_report_recipients(&self) -> Vec<UserId> {
    self
      .mirror
      .borrow()
      .snapshot
      .security
      .bug_report_recipients
      .clone()
  }

  fn acknowledge(&self, conversation: ConversationId, message: MessageId) {
    self.act(ChatAction::Acknowledge {
      conversation,
      message,
    });
  }

  fn retry_send(&self, conversation: ConversationId, message: MessageId) {
    self.act(ChatAction::RetrySend {
      conversation,
      message,
    });
  }

  fn cancel_upload(&self, conversation: ConversationId, message: MessageId, file_id: &str) {
    self.act(ChatAction::CancelUpload {
      conversation,
      message,
      file_id: file_id.to_owned(),
    });
  }

  fn cancel_message_upload(&self, conversation: ConversationId, message: MessageId) {
    self.act(ChatAction::CancelMessageUpload {
      conversation,
      message,
    });
  }

  fn discard_unsent(&self, conversation: ConversationId, message: MessageId) {
    self.act(ChatAction::DiscardUnsent {
      conversation,
      message,
    });
  }

  fn conversation_files(&self, conversation: ConversationId) -> Vec<FileInfo> {
    match self.ask(ChatQuery::ConversationFiles { conversation }) {
      Some(ChatAnswer::Files { files }) => files,
      _ => Vec::new(),
    }
  }

  fn files_page(&self, query: &FilesQuery<'_>) -> Option<FilesPage> {
    match self.ask(ChatQuery::FilesPage {
      scope: query.scope,
      filter: query.kind,
      sort: query.sort,
      starred: query.starred.to_vec(),
      max_files: query.max_files,
    })? {
      ChatAnswer::FilesPage { files, has_more } => Some(FilesPage { files, has_more }),
      // An answer of another kind (an older core): an empty list.
      _ => Some(FilesPage::default()),
    }
  }

  fn conversation_files_page(&self, conversation: ConversationId, limit: usize) -> Option<FilesPage> {
    match self.ask(ChatQuery::ConversationFilesPage { conversation, limit })? {
      ChatAnswer::FilesPage { files, has_more } => Some(FilesPage { files, has_more }),
      // An answer of another kind (an older core): its whole list, as it served it before.
      _ => Some(FilesPage::of_found(self.conversation_files(conversation), limit)),
    }
  }

  fn conversation_media(
    &self,
    conversation: ConversationId,
    anchor: MediaAnchor,
    max_messages: usize,
  ) -> Option<MediaPage> {
    match self.ask(ChatQuery::ConversationMedia {
      conversation,
      anchor,
      max_messages,
    })? {
      ChatAnswer::MediaPage { page } => Some(page),
      // An answer of another kind (an older core): nothing to page through.
      _ => Some(MediaPage::default()),
    }
  }

  fn conversation_links(&self, conversation: ConversationId, limit: usize) -> Option<LinkPage> {
    match self.ask(ChatQuery::ConversationLinks { conversation, limit })? {
      ChatAnswer::LinkPage { page } => Some(page),
      // An answer of another kind (an older core): the scan through the messages it serves.
      _ => Some(links_page(self, conversation, limit)),
    }
  }

  fn download_file(&self, conversation: ConversationId, message: MessageId) {
    self.act(ChatAction::DownloadFile {
      conversation,
      message,
    });
  }

  fn create_share_link(
    &self,
    conversation: ConversationId,
    message: MessageId,
    file_id: &str,
    options: ShareLinkOptions,
  ) {
    self.act(ChatAction::CreateShareLink {
      conversation,
      message,
      file_id: file_id.to_owned(),
      options,
    });
  }

  fn revoke_share_link(
    &self,
    conversation: ConversationId,
    message: MessageId,
    file_id: &str,
    share_id: &str,
  ) {
    self.act(ChatAction::RevokeShareLink {
      conversation,
      message,
      file_id: file_id.to_owned(),
      share_id: share_id.to_owned(),
    });
  }

  fn refresh_share_links(&self, conversation: ConversationId, message: MessageId, file_id: &str) {
    self.act(ChatAction::RefreshShareLinks {
      conversation,
      message,
      file_id: file_id.to_owned(),
    });
  }

  fn share_links(&self, conversation: ConversationId, message: MessageId, file_id: &str) -> Vec<ShareLink> {
    match self.ask(ChatQuery::ShareLinks {
      conversation,
      message,
      file_id: file_id.to_owned(),
    }) {
      Some(ChatAnswer::ShareLinks { links }) => links,
      _ => Vec::new(),
    }
  }

  fn max_upload_bytes(&self) -> Option<u64> {
    match self.ask(ChatQuery::MaxUploadBytes) {
      Some(ChatAnswer::Bytes { maybe_bytes }) => maybe_bytes,
      _ => None,
    }
  }

  fn downloads(&self) -> Option<Vec<DownloadEntry>> {
    match self.ask(ChatQuery::Downloads)? {
      ChatAnswer::Downloads { entries } => Some(entries),
      _ => None,
    }
  }

  fn control_download(&self, file_id: &str, control: DownloadControl) {
    self.act(ChatAction::ControlDownload {
      file_id: file_id.to_owned(),
      control,
    });
  }

  fn create_space(&self, space: NewSpace) {
    self.act(ChatAction::CreateSpace { space });
  }

  fn update_space(&self, space: SpaceId, changes: SpaceChanges) {
    self.act(ChatAction::UpdateSpace { space, changes });
  }

  fn create_channel_with(&self, channel: NewChannel) {
    self.act(ChatAction::CreateChannelWith { channel });
  }

  fn update_channel(&self, conversation: ConversationId, changes: ChannelChanges) {
    self.act(ChatAction::UpdateChannel {
      conversation,
      changes,
    });
  }

  fn browse_channels(&self, space: SpaceId) -> Vec<BrowsableChannel> {
    match self.ask(ChatQuery::BrowseChannels { space }) {
      Some(ChatAnswer::Channels { channels }) => channels,
      _ => Vec::new(),
    }
  }

  fn join_channel(&self, conversation: ConversationId) {
    self.act(ChatAction::JoinChannel { conversation });
  }

  fn joinable_spaces(&self) -> Vec<JoinableSpace> {
    match self.ask(ChatQuery::JoinableSpaces) {
      Some(ChatAnswer::JoinableSpaces { spaces }) => spaces,
      _ => Vec::new(),
    }
  }

  fn join_space(&self, space: SpaceId) {
    self.act(ChatAction::JoinSpace { space });
  }

  fn leave_channel(&self, conversation: ConversationId, deletes_if_last: bool) {
    self.act(ChatAction::LeaveChannel {
      conversation,
      deletes_if_last,
    });
  }

  fn suspect_deletions(&self) -> u32 {
    match self.ask(ChatQuery::SuspectDeletions) {
      Some(ChatAnswer::SuspectDeletions { count }) => count,
      _ => 0,
    }
  }

  fn dismiss_suspect_deletions(&self) {
    self.act(ChatAction::DismissSuspectDeletions);
  }

  fn delete_channel(&self, conversation: ConversationId) {
    self.act(ChatAction::DeleteChannel { conversation });
  }

  fn delete_space(&self, space: SpaceId) {
    self.act(ChatAction::DeleteSpace { space });
  }

  fn categories(&self, space: SpaceId) -> Vec<CategoryInfo> {
    match self.ask(ChatQuery::Categories { space }) {
      Some(ChatAnswer::Categories { categories }) => categories,
      _ => Vec::new(),
    }
  }

  fn create_category(&self, space: SpaceId, name: &str) {
    self.act(ChatAction::CreateCategory {
      space,
      name: name.to_owned(),
    });
  }

  fn rename_category(&self, space: SpaceId, category: CategoryId, name: &str) {
    self.act(ChatAction::RenameCategory {
      space,
      category,
      name: name.to_owned(),
    });
  }

  fn delete_category(&self, space: SpaceId, category: CategoryId) {
    self.act(ChatAction::DeleteCategory { space, category });
  }

  fn add_member_with_history(&self, conversation: ConversationId, user: UserId, history: HistoryPolicy) {
    self.act(ChatAction::AddMemberWithHistory {
      conversation,
      user,
      history,
    });
  }

  fn invite_to_space(&self, invite: SpaceInvite) {
    self.act(ChatAction::InviteToSpace { invite });
  }

  fn space_members(&self, space: SpaceId) -> SpaceRoster {
    match self.ask(ChatQuery::SpaceMembers { space }) {
      Some(ChatAnswer::SpaceMembers { roster }) => roster,
      _ => SpaceRoster::Loading,
    }
  }

  fn integration_sources(
    &self,
    conversation: ConversationId,
  ) -> Vec<teamcraft_client_api::model::IntegrationSource> {
    match self.ask(ChatQuery::IntegrationSources { conversation }) {
      Some(ChatAnswer::IntegrationSources { sources }) => sources,
      _ => Vec::new(),
    }
  }

  fn can_manage_integration_sources(&self) -> bool {
    matches!(
      self.ask(ChatQuery::CanManageIntegrationSources),
      Some(ChatAnswer::CanManageIntegrationSources { can_manage: true })
    )
  }

  fn add_integration_source(
    &self,
    conversation: ConversationId,
    source: teamcraft_client_api::model::NewIntegrationSource,
  ) {
    self.act(ChatAction::AddIntegrationSource { conversation, source });
  }

  fn revoke_integration_source(&self, conversation: ConversationId, source: teamcraft_proto::SourceId) {
    self.act(ChatAction::RevokeIntegrationSource { conversation, source });
  }

  fn rename_integration_source(
    &self,
    conversation: ConversationId,
    source: teamcraft_proto::SourceId,
    name: String,
  ) {
    self.act(ChatAction::RenameIntegrationSource {
      conversation,
      source,
      name,
    });
  }

  fn set_integration_source_picture(
    &self,
    conversation: ConversationId,
    source: teamcraft_proto::SourceId,
    maybe_picture: Option<Vec<u8>>,
  ) {
    self.act(ChatAction::SetIntegrationSourcePicture {
      conversation,
      source,
      maybe_picture: maybe_picture.map(teamcraft_proto::Base64Bytes::new),
    });
  }

  fn integration_source_picture(
    &self,
    conversation: ConversationId,
    sha256: teamcraft_proto::SourcePictureHash,
  ) -> Option<Vec<u8>> {
    match self.ask(ChatQuery::IntegrationSourcePicture { conversation, sha256 }) {
      Some(ChatAnswer::IntegrationSourcePicture { maybe_picture }) => {
        maybe_picture.map(teamcraft_proto::Base64Bytes::into_vec)
      },
      _ => None,
    }
  }

  fn notetaker_policy(&self) -> Option<teamcraft_proto::NotetakerPolicy> {
    self.mirror.borrow().snapshot.security.maybe_notetaker.clone()
  }

  fn request_notetaker(&self, conversation: ConversationId, request: teamcraft_proto::NotetakerRequest) {
    self.act(ChatAction::RequestNotetaker {
      conversation,
      request,
    });
  }

  fn channel_roles(&self, conversation: ConversationId) -> Vec<teamcraft_client_api::model::ChannelMember> {
    match self.ask(ChatQuery::ChannelRoles { conversation }) {
      Some(ChatAnswer::ChannelRoles { members }) => members,
      _ => Vec::new(),
    }
  }

  fn channel_roles_review(
    &self,
    conversation: ConversationId,
  ) -> Option<teamcraft_client_api::model::ChannelRolesReview> {
    match self.ask(ChatQuery::ChannelRolesReview { conversation }) {
      Some(ChatAnswer::ChannelRolesReview { maybe_review }) => maybe_review,
      _ => None,
    }
  }

  fn channel_role_alarms(&self) -> Vec<ConversationId> {
    match self.ask(ChatQuery::ChannelRoleAlarms) {
      Some(ChatAnswer::ChannelRoleAlarms { conversations }) => conversations,
      _ => Vec::new(),
    }
  }

  fn sign_reviewed_channel_roles(
    &self,
    conversation: ConversationId,
    roles: Vec<(UserId, teamcraft_client_api::model::ChannelRole)>,
    at_cursor: u64,
  ) {
    self.act(ChatAction::SignReviewedChannelRoles {
      conversation,
      roles,
      at_cursor,
    });
  }

  fn refresh_channel_roles(&self, conversation: ConversationId) {
    self.act(ChatAction::RefreshChannelRoles { conversation });
  }

  fn set_channel_role(
    &self,
    conversation: ConversationId,
    user: UserId,
    role: teamcraft_client_api::model::ChannelRole,
  ) {
    self.act(ChatAction::SetChannelRole {
      conversation,
      user,
      role,
    });
  }

  fn space_invites(&self, space: SpaceId) -> Vec<PendingInvite> {
    match self.ask(ChatQuery::SpaceInvites { space }) {
      Some(ChatAnswer::SpaceInvites { invites }) => invites,
      _ => Vec::new(),
    }
  }

  fn add_people_to_channel(
    &self,
    conversation: ConversationId,
    people: Vec<(UserId, bool)>,
    history: HistoryPolicy,
    joins_space: bool,
  ) {
    self.act(ChatAction::AddPeopleToChannel {
      conversation,
      people,
      history,
      joins_space,
    });
  }

  fn refresh_space_roster(&self, space: SpaceId) {
    self.act(ChatAction::RefreshSpaceRoster { space });
  }

  fn set_space_role(&self, space: SpaceId, user: UserId, role: SpaceRole) {
    self.act(ChatAction::SetSpaceRole { space, user, role });
  }

  fn remove_space_member(&self, space: SpaceId, user: UserId) {
    self.act(ChatAction::RemoveSpaceMember { space, user });
  }

  fn revoke_space_invite(&self, space: SpaceId, invite: InviteId) {
    self.act(ChatAction::RevokeSpaceInvite { space, invite });
  }

  fn resend_space_invite(&self, space: SpaceId, invite: InviteId) {
    self.act(ChatAction::ResendSpaceInvite { space, invite });
  }

  fn add_space_members(&self, space: SpaceId, users: Vec<UserId>, is_guest: bool) {
    self.act(ChatAction::AddSpaceMembers {
      space,
      users,
      is_guest,
    });
  }

  fn call_move_offer(&self, from: ConversationId, to: ConversationId) -> Option<CallMoveOffer> {
    match self.ask(ChatQuery::CallMoveOffer { from, to }) {
      Some(ChatAnswer::CallMoveOffer { maybe_offer }) => maybe_offer,
      _ => None,
    }
  }

  fn accept_call_move(&self, from: ConversationId, to: ConversationId) {
    self.act(ChatAction::AcceptCallMove { from, to });
  }

  /// Asked again whenever the core changed: a `typing_start` is a change, and so is an indicator
  /// running out (the core wakes up for it).
  fn typing_users(&self, conversation: ConversationId) -> Vec<UserId> {
    match self.ask(ChatQuery::TypingUsers { conversation }) {
      Some(ChatAnswer::Users { users }) => users,
      _ => Vec::new(),
    }
  }

  /// One question for every conversation and thread (#882), however many rows show it: asked
  /// again whenever the core changed, like [`Self::typing_users`].
  fn typing_now(&self) -> Vec<crate::model::TypingNow> {
    match self.ask(ChatQuery::TypingNow) {
      Some(ChatAnswer::Typing { typing }) => typing,
      _ => Vec::new(),
    }
  }

  /// Not an [`Self::act`]: typing changes nothing to read again, and a failure is nothing for the
  /// user to act on (the indicator is passing; the next keystrokes announce it again).
  fn notify_typing(&self, conversation: ConversationId, maybe_thread: Option<crate::model::ThreadId>) {
    spawn_local(async move {
      let action = ChatAction::Typing {
        conversation,
        maybe_thread,
      };
      if let Err(error) = core_action(action).await {
        tracing::debug!(%error, "typing not announced");
      }
    });
  }

  fn has_external_people(&self, conversation: ConversationId) -> bool {
    matches!(
      self.ask(ChatQuery::HasExternalPeople { conversation }),
      Some(ChatAnswer::Flag { value: true })
    )
  }

  fn activity(&self) -> teamcraft_client_api::activity::ActivityFeed {
    match self.ask(ChatQuery::Activity) {
      Some(ChatAnswer::Activity { feed }) => feed,
      _ => Default::default(),
    }
  }

  fn mark_activity_read(&self) {
    self.act(ChatAction::MarkActivityRead);
  }

  fn last_seen(&self, user: UserId) -> Option<teamcraft_client_api::model::TimestampMs> {
    match self.ask(ChatQuery::LastSeen { user }) {
      Some(ChatAnswer::LastSeen { maybe_at }) => maybe_at,
      _ => None,
    }
  }

  fn privacy(&self) -> teamcraft_client_api::privacy::PrivacySettings {
    match self.ask(ChatQuery::Privacy) {
      Some(ChatAnswer::Privacy { settings }) => settings,
      _ => Default::default(),
    }
  }

  fn set_last_seen_visibility(&self, visibility: teamcraft_client_api::privacy::LastSeenVisibility) {
    self.act(ChatAction::SetLastSeenVisibility { visibility });
  }

  fn call_history(&self) -> Vec<teamcraft_client_api::calls::CallHistoryItem> {
    match self.ask(ChatQuery::CallHistory) {
      Some(ChatAnswer::CallHistory { calls }) => calls,
      _ => Vec::new(),
    }
  }

  fn refresh_call_history(&self) {
    self.act(ChatAction::RefreshCallHistory);
  }

  fn list_load(
    &self,
    list: &teamcraft_client_api::list_load::LoadingList,
  ) -> teamcraft_client_api::list_load::ListLoad {
    use teamcraft_client_api::list_load::ListLoad;
    match self.ask(ChatQuery::ListLoad { list: list.clone() }) {
      Some(ChatAnswer::ListLoad { load }) => load,
      // An older core has no status: the list shows what it holds, as it did then.
      Some(ChatAnswer::Unsupported) => ListLoad::Ready,
      // Until the first answer arrives the list counts as loading.
      _ => ListLoad::Loading,
    }
  }

  fn retry_list(&self, list: &teamcraft_client_api::list_load::LoadingList) {
    self.act(ChatAction::RetryList { list: list.clone() });
  }

  fn own_devices(&self) -> Vec<teamcraft_client_api::admin::DeviceInfo> {
    match self.ask(ChatQuery::OwnDevices) {
      Some(ChatAnswer::Devices { devices }) => devices,
      _ => Vec::new(),
    }
  }

  fn refresh_own_devices(&self) {
    self.act(ChatAction::RefreshOwnDevices);
  }

  fn revoke_own_device(&self, device: teamcraft_proto::DeviceId) {
    self.act(ChatAction::RevokeOwnDevice { device });
  }

  fn rename_own_device(&self, device: teamcraft_proto::DeviceId, name: String) {
    self.act(ChatAction::RenameOwnDevice { device, name });
  }

  fn test_push_own_device(&self, device: teamcraft_proto::DeviceId) {
    self.act(ChatAction::TestPushOwnDevice { device });
  }

  fn security_keys(&self) -> teamcraft_client_api::remote::SecuritySnapshot {
    self.mirror.borrow().snapshot.security.clone()
  }

  fn owner_roster(&self) -> Option<teamcraft_client_api::admin::OwnerRosterStatus> {
    self.mirror.borrow().snapshot.security.maybe_owners.clone()
  }

  fn push_content_mode(&self) -> teamcraft_proto::PushContentMode {
    self
      .mirror
      .borrow()
      .snapshot
      .security
      .maybe_push_content
      .clone()
      .unwrap_or(teamcraft_proto::PushContentMode::Generic)
  }

  fn accept_key_change(&self, user: UserId) {
    self.act(ChatAction::AcceptKeyChange { user });
    // The pin changed in the core; read it back right away so the banner goes.
    self.queue_refresh();
  }

  fn publish_policy(
    &self,
    base: teamcraft_proto::WorkspacePolicy,
    policy: teamcraft_proto::WorkspacePolicy,
    owners: Vec<UserId>,
    admins: Vec<UserId>,
  ) {
    self.act(ChatAction::PublishPolicy {
      base,
      policy,
      owners,
      admins,
    });
  }

  fn publish_roster(&self, base_version: u64, members: Vec<teamcraft_client_api::admin::RosterEntry>) {
    self.act(ChatAction::PublishRoster {
      base_version,
      members,
    });
  }

  fn admin_add_self(&self, conversation: ConversationId) {
    self.act(ChatAction::AdminJoinChannel { conversation });
  }

  fn request_admin_join(&self, conversation: ConversationId) {
    self.act(ChatAction::RequestAdminJoin { conversation });
  }

  fn withdraw_admin_join(&self, conversation: ConversationId) {
    self.act(ChatAction::WithdrawAdminJoin { conversation });
  }

  fn approve_admin_join(
    &self,
    conversation: ConversationId,
    user: UserId,
    history: teamcraft_proto::HistoryPolicy,
  ) {
    self.act(ChatAction::ApproveAdminJoin {
      conversation,
      user,
      history,
    });
  }

  fn decline_admin_join(&self, conversation: ConversationId, user: UserId) {
    self.act(ChatAction::DeclineAdminJoin { conversation, user });
  }

  fn admin_remove_from_all_chats(&self, user: UserId) {
    self.act(ChatAction::AdminRemoveFromAllChats { user });
  }

  fn admin_delete_user(&self, user: UserId) {
    self.act(ChatAction::AdminDeleteUser { user });
  }

  fn admin_remove_group_leftover(&self, conversation: ConversationId, user: UserId) {
    self.act(ChatAction::AdminRemoveGroupLeftover { conversation, user });
  }

  fn last_action_error(&self) -> Option<String> {
    self.maybe_error.borrow().clone()
  }

  fn set_change_notifier(&self, notify: Rc<dyn Fn()>) {
    *self.notify.borrow_mut() = Some(notify);
  }

  fn me(&self) -> User {
    self.mirror.borrow().snapshot.me.clone()
  }

  fn user(&self, id: UserId) -> Option<User> {
    self
      .mirror
      .borrow()
      .snapshot
      .users
      .iter()
      .find(|user| user.id == id)
      .cloned()
  }

  fn users(&self) -> Vec<User> {
    self.mirror.borrow().snapshot.users.clone()
  }

  fn search_people(&self, query: &str) -> Vec<User> {
    match self.ask(ChatQuery::SearchPeople {
      query: query.to_owned(),
    }) {
      Some(ChatAnswer::People { people }) => people,
      // Until the core answers: who the mirror knows.
      _ => teamcraft_client_api::backend::filter_people(self.users(), query),
    }
  }

  fn meetings_revision(&self) -> u64 {
    self
      .mirror
      .borrow()
      .snapshot
      .maybe_live
      .as_ref()
      .map_or(0, |live| live.marks.meetings)
  }

  fn link_request_mark(&self) -> u64 {
    self
      .mirror
      .borrow()
      .snapshot
      .maybe_live
      .as_ref()
      .map_or(0, |live| live.marks.link_requests)
  }

  fn settings_mark(&self) -> u64 {
    self
      .mirror
      .borrow()
      .snapshot
      .maybe_live
      .as_ref()
      .map_or(0, |live| live.marks.settings)
  }

  fn key_bag_refused_mark(&self) -> u64 {
    self
      .mirror
      .borrow()
      .snapshot
      .maybe_live
      .as_ref()
      .map_or(0, |live| live.marks.key_bag_refused)
  }

  fn write_settings(&self, writes: Vec<teamcraft_proto::synced_settings::SettingWrite>) {
    self.act(ChatAction::WriteSettings { writes });
  }

  fn live_snapshot(&self) -> Option<teamcraft_client_api::live::LiveSnapshot> {
    self.mirror.borrow().snapshot.maybe_live.clone()
  }

  fn spaces(&self) -> Vec<Space> {
    self.mirror.borrow().snapshot.spaces.clone()
  }

  fn space_conversations(&self, space: SpaceId) -> Vec<Conversation> {
    let mirror = self.mirror.borrow();
    mirror
      .snapshot
      .conversations
      .iter()
      .filter(|conversation| conversation.maybe_space == Some(space))
      .cloned()
      .collect()
  }

  fn favorite_channels(&self) -> Vec<Conversation> {
    let mirror = self.mirror.borrow();
    let spaces: HashSet<SpaceId> = mirror.snapshot.spaces.iter().map(|space| space.id).collect();
    mirror
      .snapshot
      .conversations
      .iter()
      .filter(|conversation| {
        conversation.is_favorite
          && !conversation.kind.is_direct_like()
          && conversation
            .maybe_space
            .is_some_and(|space| spaces.contains(&space))
      })
      .cloned()
      .collect()
  }

  fn listed_conversations(&self) -> Vec<Conversation> {
    let mirror = self.mirror.borrow();
    let spaces: HashSet<SpaceId> = mirror.snapshot.spaces.iter().map(|space| space.id).collect();
    mirror
      .snapshot
      .conversations
      .iter()
      .filter(|conversation| {
        conversation.kind.is_direct_like()
          || conversation
            .maybe_space
            .is_some_and(|space| spaces.contains(&space))
      })
      .cloned()
      .collect()
  }

  fn direct_conversations(&self) -> Vec<Conversation> {
    let mirror = self.mirror.borrow();
    mirror
      .snapshot
      .conversations
      .iter()
      .filter(|conversation| conversation.kind.is_direct_like())
      .cloned()
      .collect()
  }

  fn conversation(&self, id: ConversationId) -> Option<Conversation> {
    self.mirror.borrow().snapshot.conversation(id).cloned()
  }

  fn message_count(&self, conversation: ConversationId) -> usize {
    let mirror = self.mirror.borrow();
    let maybe_feed = mirror.feeds.get(&conversation);
    if let Some(feed) = maybe_feed.filter(|feed| feed.is_moving) {
      return feed.total;
    }
    mirror
      .snapshot
      .message_count(conversation)
      .unwrap_or(0)
      .max(maybe_feed.map_or(0, |feed| feed.total))
  }

  fn rows_prepended(&self, conversation: ConversationId) -> usize {
    let mirror = self.mirror.borrow();
    mirror.feeds.get(&conversation).map_or(0, |feed| feed.prepended)
  }

  fn older_history(&self, conversation: ConversationId) -> teamcraft_client_api::model::OlderHistory {
    let is_asking = self
      .mirror
      .borrow()
      .feeds
      .get(&conversation)
      .is_some_and(|feed| feed.is_asking_older);
    if is_asking {
      return teamcraft_client_api::model::OlderHistory::Loading;
    }
    match self.ask(ChatQuery::OlderHistory { conversation }) {
      // The page is in the core, its rows not here yet: asking again now, with the list's old
      // position, would read on and on.
      Some(ChatAnswer::OlderHistory { total, .. }) if total > self.feed_total(conversation) => {
        teamcraft_client_api::model::OlderHistory::Loading
      },
      Some(ChatAnswer::OlderHistory { older_history, .. }) => older_history,
      // Not read yet: nothing to show and nothing to ask for until it is.
      _ => teamcraft_client_api::model::OlderHistory::Complete,
    }
  }

  fn load_older_history(&self, conversation: ConversationId) {
    // One page at a time: the list asks again on every change while the reader is near the top.
    if !matches!(
      self.older_history(conversation),
      OlderHistory::Available | OlderHistory::Failed
    ) {
      return;
    }
    if let Some(feed) = self.mirror.borrow_mut().feeds.get_mut(&conversation) {
      feed.is_expecting_older = true;
      feed.is_asking_older = true;
    }
    let key = conversation.to_string();
    crate::perf::begin_older_page(&key);
    let backend = self.clone();
    spawn_local(async move {
      let action = ChatAction::LoadOlderHistory { conversation };
      let asked_ms = crate::perf::now_ms();
      let outcome = core_action(action).await;
      crate::perf::with_pending(PerfOperation::OlderHistoryPage, &key, |measure| {
        let status = if outcome.is_ok() {
          PerfStatus::Ok
        } else {
          PerfStatus::Error
        };
        measure.span("core.action", "load_older_history", asked_ms, status);
      });
      match outcome {
        Ok(()) => {
          // Read after the core took the action, so it says how this request stands.
          let query = ChatQuery::OlderHistory { conversation };
          if let Ok(key) = serde_json::to_string(&query) {
            backend.fetch_answer(key, query).await;
          }
        },
        Err(error) => report_failure(ActionFailure::from_wire(&error)),
      }
      if let Some(feed) = backend.mirror.borrow_mut().feeds.get_mut(&conversation) {
        feed.is_asking_older = false;
      }
      // The refresh that moves the page's rows in tells the UI; telling it here as well re-read
      // everything shown once more per page, while the reader scrolls (#1595). Told here when no
      // rows come: no more history, a failure, a page whose messages wait for their keys.
      let status = backend.older_history(conversation);
      if status != OlderHistory::Loading {
        // No rows come: the measure ends here, a failure as one.
        let perf_status = match status {
          OlderHistory::Failed => PerfStatus::Error,
          _ => PerfStatus::Ok,
        };
        crate::perf::end_pending(PerfOperation::OlderHistoryPage, &key, perf_status);
        backend.changed();
      }
    });
  }

  fn request_history_at(&self, conversation: ConversationId, at: i64) {
    self.act(ChatAction::RequestHistoryAt { conversation, at });
  }

  fn is_history_loaded(&self, conversation: ConversationId, at: i64) -> bool {
    matches!(
      self.ask(ChatQuery::IsHistoryLoaded { conversation, at }),
      Some(ChatAnswer::Flag { value: true })
    )
  }

  fn history_gaps(&self, conversation: ConversationId) -> Vec<HistoryGap> {
    let Some(ChatAnswer::HistoryGaps { gaps, total }) = self.ask(ChatQuery::HistoryGaps { conversation })
    else {
      return Vec::new();
    };
    let mut mirror = self.mirror.borrow_mut();
    let Some(feed) = mirror.feeds.get_mut(&conversation) else {
      return gaps;
    };
    // As for older history: asked and not answered, or the page is in the core and its rows not
    // here yet.
    let is_waiting = feed.is_asking_gap || total > feed.total;
    if !is_waiting && gaps.iter().all(|gap| gap.status != OlderHistory::Loading) {
      // Answered without rows: none will move the rows held.
      feed.maybe_gap_rows = None;
    }
    gaps
      .into_iter()
      .map(|gap| HistoryGap {
        status: if is_waiting {
          OlderHistory::Loading
        } else {
          gap.status
        },
        ..gap
      })
      .collect()
  }

  fn fill_history_gap(&self, conversation: ConversationId, gap: HistoryGap, edge: GapEdge) {
    // One page at a time, as for older history: asked for, or its rows on their way.
    let is_waiting = self
      .history_gaps(conversation)
      .iter()
      .any(|gap| gap.status == OlderHistory::Loading);
    if is_waiting {
      return;
    }
    if let Some(feed) = self.mirror.borrow_mut().feeds.get_mut(&conversation) {
      feed.maybe_gap_rows = Some(GapRows {
        index: gap.index,
        is_before_reader: edge == GapEdge::Newer,
      });
      feed.is_asking_gap = true;
    }
    let backend = self.clone();
    spawn_local(async move {
      let action = ChatAction::FillHistoryGap {
        conversation,
        after: gap.after,
        edge,
      };
      match core_action(action).await {
        Ok(()) => {
          // Read after the core took the action, so it says how this request stands.
          let query = ChatQuery::HistoryGaps { conversation };
          if let Ok(key) = serde_json::to_string(&query) {
            backend.fetch_answer(key, query).await;
          }
        },
        Err(error) => report_failure(ActionFailure::from_wire(&error)),
      }
      if let Some(feed) = backend.mirror.borrow_mut().feeds.get_mut(&conversation) {
        feed.is_asking_gap = false;
      }
      // As for older history: the refresh that moves the rows in tells the UI.
      let is_rows_coming = backend
        .history_gaps(conversation)
        .iter()
        .any(|gap| gap.status == OlderHistory::Loading);
      if !is_rows_coming {
        backend.changed();
      }
    });
  }

  fn last_activity(&self, conversation: ConversationId) -> Option<teamcraft_client_api::model::TimestampMs> {
    self.mirror.borrow().snapshot.last_activity_of(conversation)
  }

  fn prefetch_messages(&self, conversation: ConversationId, start: usize, len: usize) {
    let total = self.message_count(conversation);
    let missing = {
      let mut mirror = self.mirror.borrow_mut();
      let end = start.saturating_add(len).min(total);
      let feed = mirror.feeds.entry(conversation).or_default();
      feed.note_read(start, end);
      feed.missing_pages(start, end)
    };
    for page in missing {
      // A page already in flight is not asked for twice: `fetch_page` only marks it outdated, and
      // nothing changed that it could predate.
      if !self
        .mirror
        .borrow()
        .loading
        .contains(&LoadKey::Page(conversation, page))
      {
        self.load_page_ahead(conversation, page);
      }
    }
  }

  fn messages(&self, conversation: ConversationId, start: usize, len: usize) -> Vec<Message> {
    let total = self.message_count(conversation);
    let end = total.min(start.saturating_add(len));
    if start >= end {
      return Vec::new();
    }
    let mut missing_pages = HashSet::new();
    let mut found = Vec::with_capacity(end - start);
    let mut stale_pages = Vec::new();
    {
      let mut mirror = self.mirror.borrow_mut();
      let feed = mirror.feeds.get_mut(&conversation).map(|feed| {
        feed.is_used = true;
        feed.note_shown(start, end);
        if std::mem::take(&mut feed.is_stale) {
          stale_pages = feed.held_pages();
        }
        &*feed
      });
      for index in start..end {
        match feed.and_then(|feed| feed.rows.get(&index)) {
          Some(message) => found.push(message.clone()),
          None => {
            missing_pages.insert(index / PAGE);
          },
        }
      }
    }
    for page in missing_pages.iter().chain(&stale_pages) {
      self.load_page(conversation, *page);
    }
    // All or nothing: a window with holes would pair messages with the wrong neighbours.
    if missing_pages.is_empty() {
      found
    } else {
      Vec::new()
    }
  }

  fn thread(&self, conversation: ConversationId, thread: ThreadId) -> Vec<Message> {
    let cached = {
      let mut mirror = self.mirror.borrow_mut();
      mirror.used_threads.insert((conversation, thread));
      mirror.threads.get(&(conversation, thread)).cloned()
    };
    match cached {
      Some(messages) => messages,
      None => {
        self.load_thread(conversation, thread);
        Vec::new()
      },
    }
  }

  fn followed_threads(&self) -> Vec<ThreadSummary> {
    Vec::new()
  }

  fn post_message(&self, conversation: ConversationId, maybe_thread: Option<ThreadId>, body: &str) {
    self.act(ChatAction::PostMessage {
      conversation,
      maybe_thread,
      body: body.to_owned(),
    });
  }

  fn toggle_reaction(&self, conversation: ConversationId, message: MessageId, emoji: &str) {
    self.act(ChatAction::ToggleReaction {
      conversation,
      message,
      emoji: emoji.to_owned(),
    });
  }

  fn mark_read(&self, conversation: ConversationId) {
    self.update_conversation(conversation, |found| {
      found.unread_count = 0;
      found.mention_count = 0;
    });
    self.act(ChatAction::MarkRead { conversation });
  }

  fn mark_read_through(&self, conversation: ConversationId, message: MessageId) {
    self.act(ChatAction::MarkReadThrough {
      conversation,
      message,
    });
  }

  fn mark_thread_read(&self, conversation: ConversationId, thread: ThreadId) {
    self.act(ChatAction::MarkThreadRead { conversation, thread });
  }

  fn set_bookmarks(
    &self,
    conversation: ConversationId,
    bookmarks: Vec<teamcraft_client_api::model::Bookmark>,
  ) {
    self.update_conversation(conversation, |found| found.bookmarks = bookmarks.clone());
    self.act(ChatAction::SetBookmarks {
      conversation,
      bookmarks,
    });
  }

  fn canvas(&self, conversation: ConversationId) -> teamcraft_client_api::canvas::CanvasState {
    match self.ask(ChatQuery::Canvas { conversation }) {
      Some(ChatAnswer::Canvas { state }) => state,
      _ => teamcraft_client_api::canvas::CanvasState::default(),
    }
  }

  fn edit_canvas(&self, conversation: ConversationId, update: Vec<u8>, is_snapshot: bool) {
    self.act(ChatAction::EditCanvas {
      conversation,
      update: update.into(),
      is_snapshot,
    });
  }

  fn compact_canvas(
    &self,
    conversation: ConversationId,
    revision: u64,
    merged: Vec<u8>,
    history: Vec<teamcraft_client_api::canvas::CanvasMark>,
  ) {
    self.act(ChatAction::CompactCanvas {
      conversation,
      revision,
      merged: merged.into(),
      history,
    });
  }

  fn remove_member(&self, conversation: ConversationId, user: UserId) {
    self.act(ChatAction::RemoveMember { conversation, user });
  }

  fn open_dm(&self, user: UserId) {
    self.act(ChatAction::OpenDm { user });
  }

  fn open_group_dm(&self, users: Vec<UserId>) {
    self.act(ChatAction::OpenGroupDm { users });
  }

  fn set_favorite(&self, conversation: ConversationId, is_favorite: bool) {
    self.update_conversation(conversation, |found| found.is_favorite = is_favorite);
    self.act(ChatAction::SetFavorite {
      conversation,
      is_favorite,
    });
  }

  fn set_presence(&self, presence: PresenceStatus) {
    self.set_presence_until(presence, None);
  }

  fn set_presence_until(
    &self,
    presence: PresenceStatus,
    maybe_until: Option<teamcraft_client_api::model::TimestampMs>,
  ) {
    self.mirror.borrow_mut().snapshot.me.presence = presence;
    // Choosing "Online" is activity on this device: it ends an away timeout the server applied
    // (the server can't tell which device a presence request came from).
    if presence == PresenceStatus::Online {
      self.act(ChatAction::SetActivity { is_idle: false });
    }
    self.act(ChatAction::SetPresence {
      presence,
      maybe_until,
    });
  }

  fn screen_share_policy(&self) -> teamcraft_proto::ScreenSharePolicy {
    self
      .mirror
      .borrow()
      .snapshot
      .security
      .maybe_screen_share_policy
      .clone()
      .unwrap_or_default()
  }

  fn voice_message_policy(&self) -> teamcraft_proto::VoiceMessagePolicy {
    self
      .mirror
      .borrow()
      .snapshot
      .security
      .maybe_voice_message_policy
      .unwrap_or_default()
  }

  fn presence_policy(&self) -> teamcraft_proto::PresencePolicy {
    self
      .mirror
      .borrow()
      .snapshot
      .security
      .maybe_presence_policy
      .clone()
      .unwrap_or_default()
  }

  fn set_custom_status(&self, maybe_status: Option<CustomStatus>) {
    self.mirror.borrow_mut().snapshot.me.maybe_custom_status = maybe_status.clone();
    self.act(ChatAction::SetCustomStatus { maybe_status });
  }
}

impl SearchBackend for RemoteChat {
  fn search(&self, query: &ParsedQuery) -> SearchResults {
    let raw = query
      .tokens
      .iter()
      .map(|token| token.raw.as_str())
      .collect::<Vec<_>>()
      .join(" ");
    match self.ask(ChatQuery::Search { query: raw }) {
      Some(ChatAnswer::SearchResults { results }) => results,
      // Asked; the UI reads again when the answer arrives.
      None => SearchResults {
        is_pending: true,
        ..SearchResults::default()
      },
      Some(_) => SearchResults::default(),
    }
  }

  fn recent_mentions(&self) -> Vec<SearchHit> {
    match self.ask(ChatQuery::RecentMentions) {
      Some(ChatAnswer::SearchHits { hits }) => hits,
      _ => Vec::new(),
    }
  }

  fn saved_searches(&self) -> Vec<SavedSearch> {
    match self.ask(ChatQuery::SavedSearches) {
      Some(ChatAnswer::SavedSearches { searches }) => searches,
      _ => Vec::new(),
    }
  }

  fn save_search(&self, name: &str, query: &str) {
    self.act(ChatAction::SaveSearch {
      name: name.to_owned(),
      query: query.to_owned(),
    });
  }

  fn delete_saved_search(&self, id: u64) {
    self.act(ChatAction::DeleteSavedSearch { id });
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::{MessageKind, id_from_raw};

  mod event_route {
    use super::*;

    /// #344: a reconnect (a new sign-in) must not stack a second listener on the shell's events.
    #[test]
    fn a_reconnect_takes_the_events_over_without_subscribing_again() {
      let mut route = EventRoute::new();
      assert!(route.route_to("first"), "the first connect subscribes");
      assert!(!route.route_to("second"), "a reconnect does not subscribe again");
      assert_eq!(
        route.target(),
        Some("second"),
        "events go to the newest backend only"
      );
    }

    #[test]
    fn after_sign_out_events_reach_no_backend_until_the_next_connect() {
      let mut route = EventRoute::new();
      route.route_to("first");
      route.clear();
      assert_eq!(route.target(), None);
      assert!(!route.route_to("second"), "still subscribed");
      assert_eq!(route.target(), Some("second"));
    }
  }

  mod feed {
    use super::*;

    #[test]
    fn the_same_page_again_is_no_change() {
      let mut feed = Feed::default();
      assert!(feed.apply(window(3, &["a", "b", "c"])));
      assert!(!feed.apply(window(3, &["a", "b", "c"])));
    }

    #[test]
    fn an_edited_row_or_a_new_total_is_a_change() {
      let mut feed = Feed::default();
      feed.apply(window(3, &["a", "b", "c"]));
      assert!(feed.apply(window(3, &["a", "B", "c"])));
      assert!(feed.apply(window(4, &["a", "B", "c"])));
    }

    #[test]
    fn held_pages_include_the_last_page() {
      let mut feed = Feed::default();
      feed.apply(window(PAGE * 3, &["a"]));
      assert_eq!(feed.held_pages(), vec![0, 2]);
    }

    #[test]
    fn a_refresh_keeps_the_pages_read_since_the_last_change() {
      let mut feed = Feed::default();
      let bodies: Vec<String> = (0..PAGE * 5).map(|index| index.to_string()).collect();
      let bodies: Vec<&str> = bodies.iter().map(String::as_str).collect();
      feed.apply(window(PAGE * 5, &bodies));
      feed.note_read(PAGE * 2 + 10, PAGE * 2 + 40);

      assert_eq!(
        feed.pages_to_refresh(),
        vec![2, 4],
        "the read page and the newest"
      );
      assert_eq!(feed.rows.len(), PAGE * 2, "the others are dropped");
      assert_eq!(feed.missing_pages(0, PAGE * 5), vec![0, 1, 3]);
    }

    #[test]
    fn missing_pages_are_the_ones_with_a_row_not_held() {
      let mut feed = Feed::default();
      let held: Vec<String> = (0..PAGE).map(|index| index.to_string()).collect();
      let held: Vec<&str> = held.iter().map(String::as_str).collect();
      feed.apply(MessageWindow {
        start: PAGE,
        ..window(PAGE * 3 + 10, &held)
      });
      assert_eq!(feed.missing_pages(0, PAGE * 4), vec![0, 2, 3]);
      assert!(
        feed.missing_pages(PAGE + 5, PAGE + 50).is_empty(),
        "inside a held page"
      );
      assert_eq!(feed.missing_pages(PAGE * 3 + 5, PAGE * 3 + 10), vec![3]);
      assert!(feed.missing_pages(10, 10).is_empty());
    }
  }

  mod pending_rows {
    use teamcraft_client_api::actions::OutgoingMessage;
    use teamcraft_client_api::model::{is_pending, pending_message_id};

    use super::refresh_plan::backend;
    use super::*;

    const CONVERSATION: u64 = 7;

    #[test]
    fn a_sent_message_is_in_the_feed_before_the_core_answers() {
      let backend = backend_with_feed(&["a", "b"]);
      let id = ClientMessageId::from_bytes([3; 16]);

      let maybe_pending = backend.show_pending(&outgoing("hello", id), id);

      assert!(maybe_pending.is_some());
      assert_eq!(backend.message_count(id_from_raw(CONVERSATION)), 3);
      let rows = backend.messages(id_from_raw(CONVERSATION), 0, 10);
      let last = rows.last().expect("the new row");
      assert_eq!(last.body, "hello");
      assert_eq!(last.id, pending_message_id(id));
      assert!(is_pending(last.id));
      assert_eq!(last.author, backend.mirror.borrow().snapshot.me.id);
    }

    #[test]
    fn the_cores_row_replaces_it_in_place() {
      let mut feed = feed_with(&["a", "b"]);
      let id = ClientMessageId::from_bytes([3; 16]);
      let mut shown = window(3, &["a", "b", "hello"]).messages.remove(2);
      shown.id = pending_message_id(id);
      assert!(feed.push_pending(shown.clone()));

      let mut from_core = shown;
      from_core.sent_at = 5;
      let mut answer = window(3, &["a", "b"]);
      answer.messages.push(from_core.clone());

      assert!(feed.apply(answer), "the row differs in its time");
      assert_eq!(feed.total, 3);
      assert_eq!(feed.rows.get(&2), Some(&from_core));
    }

    #[test]
    fn a_message_at_the_end_of_an_empty_feed_is_the_first_row() {
      let backend = backend();
      backend
        .mirror
        .borrow_mut()
        .feeds
        .entry(id_from_raw(CONVERSATION))
        .or_default();
      let id = ClientMessageId::from_bytes([3; 16]);

      assert!(backend.show_pending(&outgoing("hello", id), id).is_some());
      assert_eq!(backend.message_count(id_from_raw(CONVERSATION)), 1);
    }

    #[test]
    fn a_message_needing_more_than_its_text_waits_for_the_core() {
      let backend = backend_with_feed(&["a"]);
      let id = ClientMessageId::from_bytes([3; 16]);
      let mut reply = outgoing("hello", id);
      reply.maybe_reply_to = Some(id_from_raw(1));
      let mut urgent = outgoing("hello", id);
      urgent.priority = teamcraft_client_api::actions::MessagePriority::Urgent;
      let mut file = outgoing("hello", id);
      file.files.push(teamcraft_client_api::actions::OutgoingFile {
        file_name: "a.txt".into(),
        media_type: "text/plain".into(),
        source: teamcraft_client_api::actions::FileSource::Bytes { bytes: vec![1] },
        maybe_thumbnail: None,
        maybe_duration_ms: None,
        maybe_waveform: None,
        maybe_draft_id: None,
      });
      let blank = outgoing("   ", id);

      for message in [reply, urgent, file, blank] {
        assert!(backend.show_pending(&message, id).is_none());
      }
      assert_eq!(backend.message_count(id_from_raw(CONVERSATION)), 1);
    }

    #[test]
    fn a_feed_not_at_its_newest_end_shows_nothing_early() {
      // The reader jumped back in time: the newest rows are not held, so a row at the end would be
      // alone among rows of another time.
      let mut feed = Feed::default();
      feed.apply(window(500, &["a", "b"]));
      let id = ClientMessageId::from_bytes([3; 16]);
      let row = window(1, &["x"]).messages.remove(0);

      assert!(!feed.push_pending(row));
      assert_eq!(feed.total, 500);
      let _ = id;
    }

    #[test]
    fn a_feed_in_motion_shows_nothing_early() {
      let mut feed = feed_with(&["a", "b"]);
      feed.is_moving = true;
      let row = window(1, &["x"]).messages.remove(0);

      assert!(!feed.push_pending(row));
    }

    #[test]
    fn a_refused_message_goes_and_the_rows_after_it_move_up() {
      let mut feed = feed_with(&["a", "b"]);
      let mut first = window(1, &["x"]).messages.remove(0);
      first.id = pending_message_id(ClientMessageId::from_bytes([1; 16]));
      let mut second = window(1, &["y"]).messages.remove(0);
      second.id = pending_message_id(ClientMessageId::from_bytes([2; 16]));
      feed.push_pending(first.clone());
      feed.push_pending(second.clone());

      feed.withdraw_pending(first.id);

      assert_eq!(feed.total, 3);
      assert_eq!(feed.rows.get(&2), Some(&second));
      assert_eq!(feed.rows.len(), 3);
    }

    #[test]
    fn a_reply_goes_into_a_thread_that_is_held() {
      let backend = backend();
      let (conversation, thread) = (
        id_from_raw(CONVERSATION),
        crate::model::thread_id_of(id_from_raw(70)),
      );
      backend
        .mirror
        .borrow_mut()
        .threads
        .insert((conversation, thread), Vec::new());
      let id = ClientMessageId::from_bytes([3; 16]);
      let mut message = outgoing("in the thread", id);
      message.maybe_thread = Some(thread);

      let pending = backend.show_pending(&message, id).expect("shown");
      assert_eq!(backend.thread(conversation, thread).len(), 1);

      backend.withdraw_pending(pending);
      assert!(backend.thread(conversation, thread).is_empty());
    }

    fn outgoing(body: &str, id: ClientMessageId) -> OutgoingMessage {
      OutgoingMessage {
        conversation: id_from_raw(CONVERSATION),
        maybe_client_message_id: Some(id),
        maybe_thread: None,
        body: body.to_owned(),
        maybe_reply_to: None,
        maybe_reply_attachment: None,
        priority: Default::default(),
        wants_acknowledgement: false,
        is_silent: false,
        also_send_to_channel: false,
        files: Vec::new(),
      }
    }

    fn feed_with(bodies: &[&str]) -> Feed {
      let mut feed = Feed::default();
      feed.apply(window(bodies.len(), bodies));
      feed
    }

    fn backend_with_feed(bodies: &[&str]) -> RemoteChat {
      let backend = backend();
      backend
        .mirror
        .borrow_mut()
        .feeds
        .insert(id_from_raw(CONVERSATION), feed_with(bodies));
      backend
    }
  }

  mod refresh_plan {
    use super::*;

    /// A thread reply that stays in its thread changes no conversation entry; the shown thread and
    /// feed (the root's reply count) are read again all the same.
    #[test]
    fn a_changed_feed_reloads_the_shown_thread_and_pages() {
      let (conversation, thread) = (id_from_raw(7), crate::model::thread_id_of(id_from_raw(70)));
      let backend = backend();
      {
        let mut mirror = backend.mirror.borrow_mut();
        mirror.threads.insert((conversation, thread), Vec::new());
        mirror.used_threads.insert((conversation, thread));
        let feed = mirror.feeds.entry(conversation).or_default();
        feed.apply(window(3, &["a", "b", "c"]));
        feed.is_used = true;
      }

      let plan = backend.plan_refresh(
        SnapshotChanges {
          feeds: vec![conversation],
          ..SnapshotChanges::default()
        },
        0.0,
      );

      assert!(!plan.is_snapshot_changed, "the sidebar has nothing to re-render");
      assert_eq!(plan.threads, vec![(conversation, thread)]);
      assert_eq!(plan.pages, vec![(conversation, 0)]);
    }

    #[test]
    fn a_changed_feed_elsewhere_reloads_nothing_here() {
      let (conversation, thread) = (id_from_raw(7), crate::model::thread_id_of(id_from_raw(70)));
      let backend = backend();
      {
        let mut mirror = backend.mirror.borrow_mut();
        mirror.threads.insert((conversation, thread), Vec::new());
        mirror.used_threads.insert((conversation, thread));
      }

      let plan = backend.plan_refresh(
        SnapshotChanges {
          feeds: vec![id_from_raw(8)],
          ..SnapshotChanges::default()
        },
        0.0,
      );

      assert!(plan.threads.is_empty() && plan.pages.is_empty());
    }

    mod requery {
      use super::*;

      #[test]
      fn a_query_asked_within_the_interval_is_asked_once_it_is_over() {
        let backend = backend();
        insert_slot(&backend, false, 1_000.0);

        let plan = backend.plan_refresh(SnapshotChanges::default(), 1_500.0);
        assert!(plan.queries.is_empty() && plan.has_deferred_queries);
        assert_eq!(backend.take_due_queries().len(), 1);

        insert_slot(&backend, false, 1_000.0);
        let plan = backend.plan_refresh(SnapshotChanges::default(), 2_000.0);
        assert_eq!(plan.queries.len(), 1);
        assert!(!plan.has_deferred_queries);
      }

      /// A slow answer and a change every second asked it back to back.
      #[test]
      fn a_query_in_flight_waits_for_the_interval_instead_of_being_asked_on_arrival() {
        let backend = backend();
        insert_slot(&backend, true, 1_000.0);

        let plan = backend.plan_refresh(SnapshotChanges::default(), 1_200.0);
        assert!(plan.has_deferred_queries);
        let answers = backend.answers.borrow();
        let slot = answers.values().next().unwrap();
        assert!(slot.is_requery_due && !slot.is_outdated);
      }

      #[test]
      fn a_due_query_still_in_flight_is_asked_again_when_it_arrives() {
        let backend = backend();
        insert_slot(&backend, true, 1_000.0);
        backend.plan_refresh(SnapshotChanges::default(), 1_200.0);

        assert!(backend.take_due_queries().is_empty());
        assert!(backend.answers.borrow().values().all(|slot| slot.is_outdated));
      }

      fn insert_slot(backend: &RemoteChat, is_loading: bool, asked_ms: f64) {
        backend.answers.borrow_mut().insert(
          "activity".to_owned(),
          Slot {
            maybe_query: Some(ChatQuery::Activity),
            is_used: true,
            is_loading,
            maybe_asked_ms: Some(asked_ms),
            ..Slot::default()
          },
        );
      }
    }

    pub(super) fn backend() -> RemoteChat {
      let snapshot = ChatSnapshot::read(&crate::mock::MockBackend::new(0), Default::default());
      RemoteChat {
        mirror: Rc::new(RefCell::new(Mirror {
          snapshot,
          cursor: SnapshotCursor::default(),
          feeds: HashMap::new(),
          threads: HashMap::new(),
          used_threads: HashSet::new(),
          loading: HashSet::new(),
          outdated: HashSet::new(),
        })),
        notify: Rc::new(RefCell::new(None)),
        maybe_error: Rc::new(RefCell::new(None)),
        refresh_queued: Rc::new(Cell::new(false)),
        requery_queued: Rc::new(Cell::new(false)),
        answers: Rc::new(RefCell::new(HashMap::new())),
      }
    }
  }

  mod favorite_channels {
    use super::refresh_plan::backend;
    use super::*;

    /// #1152: Chats lists the starred channels of the spaces the user is in, nothing else.
    #[test]
    fn only_starred_channels_of_spaces_the_user_is_in_are_listed() {
      let backend = backend();
      let (kept, left_space, unstarred, direct) = {
        let mut mirror = backend.mirror.borrow_mut();
        let spaces: Vec<SpaceId> = mirror.snapshot.spaces.iter().map(|space| space.id).collect();
        assert!(spaces.len() >= 2, "the mock has several spaces");
        let in_space = |mirror: &Mirror, space: SpaceId| {
          mirror
            .snapshot
            .conversations
            .iter()
            .position(|conversation| conversation.maybe_space == Some(space))
            .expect("a channel in the space")
        };
        let kept = in_space(&mirror, spaces[0]);
        let left_space = in_space(&mirror, spaces[1]);
        let unstarred = (0..mirror.snapshot.conversations.len())
          .find(|index| {
            *index != kept && mirror.snapshot.conversations[*index].maybe_space == Some(spaces[0])
          })
          .expect("a second channel");
        let direct = mirror
          .snapshot
          .conversations
          .iter()
          .position(|conversation| conversation.kind.is_direct_like())
          .expect("a DM");
        for conversation in &mut mirror.snapshot.conversations {
          conversation.is_favorite = false;
        }
        for index in [kept, left_space, direct] {
          mirror.snapshot.conversations[index].is_favorite = true;
        }
        // The user left the second space: its channels may still be in the snapshot for a moment.
        mirror.snapshot.spaces.retain(|space| space.id != spaces[1]);
        let id = |index: usize| mirror.snapshot.conversations[index].id;
        (id(kept), id(left_space), id(unstarred), id(direct))
      };

      let listed: Vec<ConversationId> = backend.favorite_channels().iter().map(|c| c.id).collect();

      assert_eq!(listed, vec![kept]);
      assert!(!listed.contains(&left_space) && !listed.contains(&unstarred) && !listed.contains(&direct));
    }

    #[test]
    fn removing_the_star_removes_the_entry() {
      let backend = backend();
      let id = {
        let mut mirror = backend.mirror.borrow_mut();
        let conversation = mirror
          .snapshot
          .conversations
          .iter_mut()
          .find(|conversation| conversation.maybe_space.is_some())
          .expect("a channel");
        conversation.is_favorite = true;
        conversation.id
      };
      assert!(backend.favorite_channels().iter().any(|c| c.id == id));

      backend
        .mirror
        .borrow_mut()
        .snapshot
        .conversations
        .iter_mut()
        .filter(|conversation| conversation.id == id)
        .for_each(|conversation| conversation.is_favorite = false);

      assert!(backend.favorite_channels().iter().all(|c| c.id != id));
    }
  }

  mod older_history {
    use teamcraft_client_api::model::OlderHistory;

    use super::*;

    #[test]
    fn asking_says_loading_whatever_the_cached_answer_says() {
      let (backend, conversation) = backend_with_answer(OlderHistory::Available, 3);
      backend
        .mirror
        .borrow_mut()
        .feeds
        .entry(conversation)
        .or_default()
        .is_asking_older = true;
      assert_eq!(backend.older_history(conversation), OlderHistory::Loading);
    }

    #[test]
    fn a_page_the_feed_has_not_got_yet_still_counts_as_loading() {
      let (backend, conversation) = backend_with_answer(OlderHistory::Available, 53);
      assert_eq!(backend.older_history(conversation), OlderHistory::Loading);
    }

    #[test]
    fn with_the_rows_here_the_answer_stands() {
      let (backend, conversation) = backend_with_answer(OlderHistory::Available, 3);
      assert_eq!(backend.older_history(conversation), OlderHistory::Available);
      let (backend, conversation) = backend_with_answer(OlderHistory::Failed, 3);
      assert_eq!(backend.older_history(conversation), OlderHistory::Failed);
    }

    #[test]
    fn a_hole_says_loading_until_its_rows_are_here() {
      let gap = HistoryGap {
        after: id_from_raw(2),
        index: 2,
        status: OlderHistory::Available,
      };
      let (backend, conversation) = backend_with_answer(OlderHistory::Available, 3);
      let answer = |total: usize| ChatAnswer::HistoryGaps {
        gaps: vec![gap],
        total,
      };
      let key = serde_json::to_string(&ChatQuery::HistoryGaps { conversation }).unwrap();
      let settle = |answer: ChatAnswer| {
        backend
          .answers
          .borrow_mut()
          .entry(key.clone())
          .or_default()
          .settle(answer);
      };
      settle(answer(3));
      assert_eq!(backend.history_gaps(conversation), vec![gap]);

      backend
        .mirror
        .borrow_mut()
        .feeds
        .get_mut(&conversation)
        .unwrap()
        .is_asking_gap = true;
      assert_eq!(
        backend.history_gaps(conversation)[0].status,
        OlderHistory::Loading
      );
      backend
        .mirror
        .borrow_mut()
        .feeds
        .get_mut(&conversation)
        .unwrap()
        .is_asking_gap = false;

      settle(answer(53));
      assert_eq!(
        backend.history_gaps(conversation)[0].status,
        OlderHistory::Loading,
        "the page is in the core, its rows not here"
      );
    }

    #[test]
    fn an_answer_without_rows_expects_none() {
      let (backend, conversation) = backend_with_answer(OlderHistory::Available, 3);
      backend
        .mirror
        .borrow_mut()
        .feeds
        .get_mut(&conversation)
        .unwrap()
        .maybe_gap_rows = Some(GapRows {
        index: 2,
        is_before_reader: false,
      });
      let key = serde_json::to_string(&ChatQuery::HistoryGaps { conversation }).unwrap();
      let answer = ChatAnswer::HistoryGaps {
        gaps: Vec::new(),
        total: 3,
      };
      backend
        .answers
        .borrow_mut()
        .entry(key)
        .or_default()
        .settle(answer);

      assert!(backend.history_gaps(conversation).is_empty());
      assert_eq!(backend.mirror.borrow().feeds[&conversation].maybe_gap_rows, None);
    }

    /// A feed of 3 rows, and a fresh answer of the core with `total` rows.
    fn backend_with_answer(older_history: OlderHistory, total: usize) -> (RemoteChat, ConversationId) {
      let backend = super::refresh_plan::backend();
      let conversation = id_from_raw(1);
      backend
        .mirror
        .borrow_mut()
        .feeds
        .entry(conversation)
        .or_default()
        .apply(window(3, &["a", "b", "c"]));
      let query = ChatQuery::OlderHistory { conversation };
      let key = serde_json::to_string(&query).unwrap();
      let answer = ChatAnswer::OlderHistory { older_history, total };
      backend
        .answers
        .borrow_mut()
        .entry(key)
        .or_default()
        .settle(answer);
      (backend, conversation)
    }
  }

  mod moved_rows {
    use super::*;

    #[test]
    fn older_history_moves_the_rows_held_and_the_pages_read() {
      let mut feed = feed_of(&["a", "b", "c"]);
      feed.read_pages.insert(0);
      feed.shown_pages.insert(0);
      // 200 older rows: the newest rows are now c at 202.
      let newest = MessageWindow {
        start: 202,
        total: 203,
        ..window(203, &[])
      };
      let newest = with_rows(newest, &[3]);
      assert!(feed.take_newest(newest));
      assert_eq!(feed.prepended, 200);
      assert_eq!(feed.total, 203);
      assert_eq!(feed.rows.keys().copied().collect::<Vec<_>>(), vec![200, 201, 202]);
      assert_eq!(feed.read_pages, BTreeSet::from([2]));
      assert_eq!(feed.shown_pages, BTreeSet::from([2]));
    }

    #[test]
    fn a_shift_within_a_page_keeps_both_pages_it_now_spans() {
      assert_eq!(moved_pages(&BTreeSet::from([0]), 0, 50), BTreeSet::from([0, 1]));
      assert_eq!(
        moved_pages(&BTreeSet::from([0, 1, 3]), 150, 200),
        BTreeSet::from([0, 1, 3, 5]),
        "rows before the place they arrived at stay"
      );
    }

    #[test]
    fn a_hole_read_from_its_older_side_moves_only_the_rows_after_it() {
      // Rows a, b (the jumped page) and c (held); 200 rows of the hole arrive between b and c.
      let mut feed = feed_of(&["a", "b", "c"]);
      feed.read_pages.insert(0);
      feed.maybe_gap_rows = Some(GapRows {
        index: 2,
        is_before_reader: false,
      });
      let newest = MessageWindow {
        start: 202,
        ..with_rows(window(203, &[]), &[3])
      };
      assert!(feed.take_newest(newest));
      assert_eq!(feed.prepended, 0, "the reader above the hole stays");
      assert_eq!(feed.rows.keys().copied().collect::<Vec<_>>(), vec![0, 1, 202]);
      assert_eq!(feed.read_pages, BTreeSet::from([0, 2]));
      assert_eq!(feed.maybe_gap_rows, None);
    }

    #[test]
    fn a_hole_read_from_its_newer_side_counts_as_rows_before_the_reader() {
      let mut feed = feed_of(&["a", "b", "c"]);
      feed.maybe_gap_rows = Some(GapRows {
        index: 2,
        is_before_reader: true,
      });
      let newest = MessageWindow {
        start: 52,
        ..with_rows(window(53, &[]), &[3])
      };
      assert!(feed.take_newest(newest));
      assert_eq!(feed.prepended, 50, "the reader below the hole stays");
      assert_eq!(feed.rows.keys().copied().collect::<Vec<_>>(), vec![0, 1, 52]);
    }

    #[test]
    fn new_messages_at_the_end_move_nothing() {
      let mut feed = feed_of(&["a", "b", "c"]);
      let newest = with_rows(window(5, &[]), &[1, 2, 3, 4, 5]);
      assert!(feed.take_newest(newest));
      assert_eq!(feed.prepended, 0);
      assert_eq!(feed.rows.len(), 5);
    }

    #[test]
    fn a_page_from_after_the_jump_that_shows_no_held_row_is_left_out() {
      let mut feed = feed_of(&["a", "b", "c"]);
      let elsewhere = MessageWindow {
        start: 100,
        ..with_rows(window(203, &[]), &[50])
      };
      assert!(!feed.apply(elsewhere));
      assert_eq!(feed.total, 3);
      assert_eq!(feed.rows.len(), 3);
    }

    #[test]
    fn a_page_from_after_the_jump_that_shows_a_held_row_moves_them() {
      let mut feed = feed_of(&["a", "b", "c"]);
      let newest = MessageWindow {
        start: 202,
        ..with_rows(window(203, &[]), &[3])
      };
      assert!(feed.apply(newest));
      assert_eq!(feed.prepended, 200);
    }

    #[test]
    fn the_first_page_of_a_feed_is_no_jump() {
      let mut feed = Feed::default();
      assert!(feed.apply(window(500, &["a"])));
      assert_eq!(feed.prepended, 0);
      assert_eq!(feed.total, 500);
    }

    #[test]
    fn one_new_row_is_no_jump() {
      let mut feed = feed_of(&["a", "b", "c"]);
      let elsewhere = MessageWindow {
        start: 3,
        ..with_rows(window(4, &[]), &[4])
      };
      assert!(feed.apply(elsewhere));
      assert_eq!(feed.total, 4);
      assert_eq!(feed.prepended, 0);
    }

    fn feed_of(bodies: &[&str]) -> Feed {
      let mut feed = Feed::default();
      feed.apply(window(bodies.len(), bodies));
      feed
    }

    /// `window` with rows that have these IDs (the helper numbers its rows from 1).
    fn with_rows(mut window: MessageWindow, ids: &[u64]) -> MessageWindow {
      let template = self::window(1, &["x"]).messages.remove(0);
      window.messages = ids
        .iter()
        .map(|id| Message {
          id: id_from_raw(*id),
          ..template.clone()
        })
        .collect();
      window
    }

    use super::window;
  }

  mod core_read {
    use super::*;

    /// The desktop shell and a tab that runs the core answer these commands (#1699): their names
    /// and arguments are what they always were.
    #[test]
    fn reads_keep_the_shells_commands_and_arguments() {
      let conversation: ConversationId = id_from_raw(7);
      assert_eq!(CoreRead::Snapshot.to_json(), ("core_snapshot", json!({})));
      assert_eq!(
        CoreRead::Changes(None).to_json(),
        ("core_snapshot_changes", json!({ "since": null }))
      );
      assert_eq!(
        CoreRead::Messages {
          conversation,
          start: 100,
          len: PAGE
        }
        .to_json(),
        (
          "core_messages",
          json!({ "conversation": conversation, "start": 100, "len": PAGE })
        )
      );
      let thread: ThreadId = id_from_raw(9);
      assert_eq!(
        CoreRead::Thread { conversation, thread }.to_json(),
        (
          "core_thread",
          json!({ "conversation": conversation, "thread": thread })
        )
      );
    }

    #[test]
    fn an_answer_reads_back_from_its_json() {
      let read = CoreRead::Messages {
        conversation: id_from_raw(7),
        start: 0,
        len: PAGE,
      };
      let json = CoreReadAnswer::Messages(window(2, &["a", "b"]))
        .to_json()
        .unwrap();
      match read.answer_from_json(json) {
        Ok(CoreReadAnswer::Messages(back)) => assert_eq!(back, window(2, &["a", "b"])),
        _ => panic!("a feed's page reads back as one"),
      }
    }

    #[test]
    fn an_answer_of_another_shape_is_an_error_naming_the_command() {
      let error = CoreRead::Thread {
        conversation: id_from_raw(7),
        thread: id_from_raw(9),
      }
      .answer_from_json(json!({ "not": "a thread" }))
      .err()
      .unwrap();
      assert!(error.contains("core_thread"), "{error}");
    }
  }

  mod slot {
    use super::*;

    #[test]
    fn an_equal_answer_is_no_change() {
      let mut slot = Slot::default();
      assert!(slot.settle(draft("x")));
      assert!(!slot.settle(draft("x")));
      assert!(slot.settle(draft("y")));
      assert!(!slot.is_loading && !slot.is_stale);
    }
  }

  fn draft(body: &str) -> ChatAnswer {
    ChatAnswer::Draft {
      maybe_body: Some(body.to_owned()),
    }
  }

  fn window(total: usize, bodies: &[&str]) -> MessageWindow {
    MessageWindow {
      conversation: id_from_raw(7),
      start: 0,
      total,
      undecrypted: 0,
      messages: bodies
        .iter()
        .zip(1_u64..)
        .map(|(body, id)| Message {
          id: id_from_raw(id),
          conversation: id_from_raw(7),
          author: id_from_raw(2),
          kind: MessageKind::User,
          sent_at: 0,
          body: (*body).to_owned(),
          is_edited: false,
          reactions: Vec::new(),
          reply_count: 0,
          maybe_thread: None,
          maybe_imported: None,
          maybe_source: None,
          maybe_placeholder: None,
          group_mentions: Vec::new(),
          custom_emoji: Vec::new(),
          mentions_me: false,
          maybe_poll: None,
        })
        .collect(),
    }
  }
}
