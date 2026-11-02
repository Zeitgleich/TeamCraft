//! The client core inside the browser page: the web build is a full client like the desktop app.
//!
//! The desktop shell runs `teamcraft-core` on a thread and the UI reaches it through Tauri commands
//! (`core_*`). The browser has no shell, so the same commands are answered here, in the page: a local
//! task owns the [`Client`] (encrypted IndexedDB store, fetch transport, WebSocket gateway) and
//! serves the requests of [`call`]. Everything above this module (the remote backend, the sign-in and
//! enrollment screens) is shared with the desktop build.
//!
//! The access token is the page's own (the browser flow keeps tokens in memory, see `auth::browser`);
//! a small loop keeps the copy the core reads fresh.
//!
//! Only one tab per account runs the core (`web_tabs`, e2ee.md §9.4) and shows the app; the others
//! show a notice and take over only when the user asks (#744, `tab_handover`).

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::future::Future;
use std::pin::{Pin, pin};
use std::rc::Rc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use fluent_bundle::FluentArgs;
use futures_channel::{mpsc, oneshot};
use futures_util::future::{Either, pending, poll_immediate, select};
use futures_util::{FutureExt, StreamExt};
use serde::de::DeserializeOwned;
use serde_json::Value;
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::calls::{BackgroundEffect, OrgBackgrounds};
use teamcraft_client_api::model::{Message, ThreadId};
use teamcraft_client_api::remote::{
  AttachmentPriority, ChatAction, ChatAnswer, ChatQuery, ChatSnapshot, ContactKeys, CoreConnection,
  CoreStatus, EnrollRequest, EnrollSecret, LinkRequestView, LinkStatus, MessageWindow, PhoneLinkStatus,
  SecuritySnapshot, StoreHealth, StoreProblem,
};
use teamcraft_client_api::snapshot_changes::{SnapshotChanges, SnapshotCursor};
use teamcraft_core::api::{AccessTokenSource, ApiError, HttpServerApi, ServerApi};
use teamcraft_core::client::files::BlobUpload;
use teamcraft_core::client::{
  CatchUpFocus, LinkAnnouncement, LinkApproval, LinkReceipt, LinkWait, LinkWaitCancel, LinkedSecrets,
  ReceiptCancel, SIGN_OUT_REVOKE_PATIENCE,
};
use teamcraft_core::clock::{Clock, SystemClock};
use teamcraft_core::connection::{ConnectionSettings, ConnectionStatus, ConnectionSupervisor};
use teamcraft_core::downloads::{Downloads, Finished};
use teamcraft_core::push_preview::{PushSession, save_push_session};
use teamcraft_core::push_text::{NAME_PLACEHOLDER, NotificationLabels};
use teamcraft_core::records;
use teamcraft_core::store::{IndexedDbStore, StoreError, TrackedStore, browser_database_key};
use teamcraft_core::transfers::Transfers;
use teamcraft_core::typing::TypingAnnouncer;
use teamcraft_core::ui_backend::{CoreChatBackend, CoreCommand};
use teamcraft_core::ui_snapshot::SnapshotCache;
use teamcraft_core::upkeep::{self, Upkeep, UploadMode};
use teamcraft_core::{
  CallPreparation, Client, ClientSettings, CoreError, EnrollmentOptions, EnrollmentSecret, LinkOffer,
  PendingEnrollment, UnreadableStore, parse_recovery_key, platform,
};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_crypto::identity::{IdentityPublicKey, VerifiedDevice};
use teamcraft_crypto::secret::SecretBytes;
use teamcraft_proto::DeviceLinkPlatform;
use teamcraft_proto::{
  ConversationId, DeviceActivity, GatewayClientMessage, GatewayServerMessage, MessageId, UserId,
};
use wasm_bindgen::JsCast;

use crate::auth::{AuthError, Authenticator};
use crate::settings::{load_string, store_string};
use crate::sync_reads::{StoredWorkspace, SyncRead, SyncReadAnswer, answer_during_sync};
use crate::tab_handover::{
  Blocker, OnTaken, PERSIST_DRAFTS_EVENT, TabEvent, TabState, TakeoverAnswer, answer_takeover, keeps_lock,
  on_taken,
};
use crate::web_tabs::{self, Follower, OwnerChannel, OwnerLock, Ownership};

/// Wrong short codes allowed before a link approval is dropped.
const MAX_APPROVAL_MISSES: u8 = 3;
/// Decoded messages of the conversations read last: the store decrypts a record only when it is
/// asked for (issue #155), so the open conversation is kept instead of decrypted on every change.
const MESSAGE_CACHE_MESSAGES: usize = 60_000;
/// How often the loop wakes up on its own: outbox retries, heartbeats, staggered commits.
const TICK: Duration = Duration::from_secs(3);
/// How often the UI is told to read again while a long sync fills the store (issue #247).
const SYNC_PROGRESS: Duration = Duration::from_secs(1);
/// How often the UI is told to read again while a file goes up or down, for its progress.
const TRANSFER_PROGRESS: Duration = Duration::from_millis(500);
/// How long a failed account data sync waits before it is retried; also how often the roster's age
/// is checked locally. Account data itself is read after each connection and on `key_bag_update`
/// (issue #252), not on a timer.
const UPKEEP_INTERVAL: Duration = Duration::from_secs(60);
/// How often the core's copy of the access token is renewed from the page's session.
const TOKEN_REFRESH: Duration = Duration::from_secs(20);
/// How long the access token stored for the service worker is used. Tokens are renewed every
/// [`TOKEN_REFRESH`]; the stored one only has to outlive a push that arrives while no page is open.
const PUSH_SESSION_LIFETIME_MS: i64 = 4 * 60 * 1000;
/// Local storage key (per user) remembering that this browser had the account set up, so a store
/// the browser deleted is told apart from a first visit (e2ee.md §9.4).
/// Local storage key (this browser) naming the user of the last guest device's database, so a later
/// guest page deletes what a page closed without "Leave" left behind (e2ee.md §4.6).
const GUEST_STORE_KEY: &str = "teamcraft.guest_store_user";
const STORE_READY_PREFIX: &str = "teamcraft.store_ready.";
/// How long to wait before checking the push subscription again after the check failed (offline).
const PUSH_CHECK_RETRY: Duration = Duration::from_secs(60);
/// What the service worker posts when the browser changed the push subscription.
const SUBSCRIPTION_CHANGED: &str = "push_subscription_changed";
/// Session storage key (this tab, survives a reload) naming the user this tab was active for, so a
/// reload of the active tab doesn't ask "use here?" (#744).
const ACTIVE_TAB_KEY: &str = "teamcraft.active_tab_user";
/// How long a reloaded active tab waits for the old page's lock before it shows the notice.
const RELOAD_GRACE: Duration = Duration::from_secs(3);
/// How long "Use here" waits for the active tab to let go: it persists the outbox first.
const TAKEOVER_TIMEOUT: Duration = Duration::from_secs(15);
/// How long the active tab waits for IndexedDB before it hands over anyway.
const HANDOVER_FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

type Api = HttpServerApi<WebTokens>;
/// Notes what each write changes, so the UI's snapshot is rebuilt per conversation (issue #246).
type Store = TrackedStore<Rc<IndexedDbStore>>;
type Listener = Rc<RefCell<dyn FnMut()>>;
/// A wait for IndexedDB to have written everything ([`IndexedDbStore::flush`]).
type Flush = Pin<Box<dyn Future<Output = Result<(), StoreError>>>>;

thread_local! {
  static SETUP: RefCell<Option<Setup>> = const { RefCell::new(None) };
  static RUNNING: RefCell<Option<Running>> = const { RefCell::new(None) };
  static LISTENERS: RefCell<Vec<Listener>> = const { RefCell::new(Vec::new()) };
  static IS_WATCHING_WORKER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
  static IS_WATCHING_LIFECYCLE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
  static TAB: Cell<TabState> = Cell::new(TabState::default());
  static TAB_LISTENERS: RefCell<Vec<Listener>> = const { RefCell::new(Vec::new()) };
  /// Takeovers waiting for this tab to become the active one.
  static ACTIVATIONS: RefCell<Vec<oneshot::Sender<()>>> = const { RefCell::new(Vec::new()) };
  static IS_WATCHING_FOCUS: Cell<bool> = const { Cell::new(false) };
  /// The store whose queued writes the page lifecycle listener issues.
  static LIFECYCLE_STORE: RefCell<std::rc::Weak<IndexedDbStore>> =
    const { RefCell::new(std::rc::Weak::new()) };
}

/// What the page tells the core once: who signs the requests and where the server is.
struct Setup {
  auth: Rc<dyn Authenticator>,
  server_url: String,
}

/// The core of the signed-in account: in this tab, or in the tab that owns the store.
enum Running {
  Owner {
    user: UserId,
    requests: mpsc::UnboundedSender<Request>,
    /// `None` without Web Locks: then no other tab can find this one.
    maybe_channel: Option<OwnerChannel>,
    /// Read without asking the loop, which may be busy connecting.
    connection: ConnectionStatus,
  },
  Follower {
    user: UserId,
    follower: Rc<Follower>,
  },
}

impl Running {
  fn user(&self) -> UserId {
    match self {
      Self::Owner { user, .. } | Self::Follower { user, .. } => *user,
    }
  }
}

/// The access token the core reads synchronously; renewed by [`keep_token_fresh`].
#[derive(Clone, Default)]
struct WebTokens(Rc<RefCell<Option<String>>>);

impl AccessTokenSource for WebTokens {
  fn access_token(&self) -> Result<String, ApiError> {
    self
      .0
      .borrow()
      .clone()
      .ok_or_else(|| ApiError::Token("not signed in".to_owned()))
  }
}

/// Called once at startup by the sign-in code. `server_url` empty means the page's own origin.
pub fn configure(auth: Rc<dyn Authenticator>, server_url: &str) {
  let server_url = if server_url.is_empty() {
    web_sys::window()
      .and_then(|window| window.location().origin().ok())
      .unwrap_or_default()
  } else {
    server_url.to_owned()
  };
  SETUP.with(|setup| *setup.borrow_mut() = Some(Setup { auth, server_url }));
  teamcraft_core::thumbnail::set_thumbnailer(Rc::new(thumbnail_in_chunk));
}

/// The core's thumbnails, made in the chunk with the image codecs (areas.rs), which loads with the
/// first picture. A chunk that can't load (offline before it was cached, a deploy in between)
/// leaves the picture without a thumbnail, like one the codecs can't read.
fn thumbnail_in_chunk<'a>(
  bytes: &'a [u8],
  media_type: &'a str,
) -> Pin<Box<dyn Future<Output = Option<Vec<u8>>> + 'a>> {
  Box::pin(async move {
    match crate::areas::thumbnail(bytes, media_type).await {
      Ok(maybe_thumbnail) => maybe_thumbnail,
      Err(error) => {
        tracing::warn!(%error, "the image codecs did not load: the picture goes without a thumbnail");
        None
      },
    }
  })
}

/// Whether this page can run the core (the page was configured for it).
pub fn is_available() -> bool {
  SETUP.with(|setup| setup.borrow().is_some())
}

/// Whether this tab shows the account's notifications: the store's owner, or a tab running its own
/// core (no Web Locks). Followers stay quiet, so several tabs notify once (e2ee.md §9.4).
pub fn is_notifying_tab() -> bool {
  RUNNING.with(|running| !matches!(running.borrow().as_ref(), Some(Running::Follower { .. })))
}

/// Calls `callback` whenever the core's data changed.
pub fn listen(callback: impl FnMut() + 'static) {
  LISTENERS.with(|listeners| listeners.borrow_mut().push(Rc::new(RefCell::new(callback))));
}

/// Whether this tab shows the app or the notice that another tab runs it (#744).
pub fn tab_state() -> TabState {
  TAB.with(Cell::get)
}

/// Calls `callback` whenever [`tab_state`] changed.
pub fn listen_tab(callback: impl FnMut() + 'static) {
  TAB_LISTENERS.with(|listeners| listeners.borrow_mut().push(Rc::new(RefCell::new(callback))));
}

/// "Use here": asks the active tab to hand the app over and runs the core here once it let go.
/// After a refusal because of a call, the next press ends that call (the user saw the warning).
pub async fn use_here() {
  let Some(follower) = current_follower() else {
    return;
  };
  let ends_call = tab_state().takeover.ends_call();
  update_tab(TabEvent::UseHere);
  // A tab that asked goes first when the lock comes free (`take_over_when_free`).
  follower.set_claiming(true);
  let activated = Box::pin(activation());
  let refused = Box::pin(follower.request_takeover(ends_call));
  let timeout = Box::pin(platform::sleep(TAKEOVER_TIMEOUT));
  let outcome = select(activated, select(refused, timeout)).await;
  follower.set_claiming(false);
  match outcome {
    Either::Left(_) => {},
    Either::Right((Either::Left((Some(blocker), _)), _)) => update_tab(TabEvent::Refused(blocker)),
    // The refusal channel closed or the time ran out: the active tab doesn't answer.
    Either::Right(_) => update_tab(TabEvent::TimedOut),
  }
}

/// Resolves when this tab becomes the active one.
fn activation() -> impl Future<Output = ()> {
  let (activated, receiver) = oneshot::channel();
  ACTIVATIONS.with(|waiting| waiting.borrow_mut().push(activated));
  async move {
    // A dropped sender (never, the list lives as long as the page) also ends the wait.
    let _ = receiver.await;
  }
}

/// The user closed the warning about the call in the active tab.
pub fn cancel_takeover() {
  update_tab(TabEvent::Cancel);
}

fn update_tab(event: TabEvent) {
  let (before, after) = TAB.with(|tab| {
    let before = tab.get();
    let after = before.apply(event);
    tab.set(after);
    (before, after)
  });
  if before == after {
    return;
  }
  let listeners: Vec<_> = TAB_LISTENERS.with(|listeners| listeners.borrow().clone());
  for listener in listeners {
    if let Ok(mut callback) = listener.try_borrow_mut() {
      (*callback)();
    }
  }
}

fn became_active(user: UserId) {
  session_storage_set(ACTIVE_TAB_KEY, Some(&user.to_string()));
  update_tab(TabEvent::BecameActive);
  let waiting: Vec<_> = ACTIVATIONS.with(|waiting| waiting.borrow_mut().drain(..).collect());
  for activated in waiting {
    // The waiting takeover may have timed out already.
    let _ = activated.send(());
  }
}

fn session_storage_set(key: &str, maybe_value: Option<&str>) {
  let Some(storage) = web_sys::window().and_then(|window| window.session_storage().ok().flatten()) else {
    return;
  };
  let result = match maybe_value {
    Some(value) => storage.set_item(key, value),
    None => storage.remove_item(key),
  };
  if result.is_err() {
    tracing::debug!(key, "could not write the session storage");
  }
}

fn session_storage_get(key: &str) -> Option<String> {
  web_sys::window()
    .and_then(|window| window.session_storage().ok().flatten())
    .and_then(|storage| storage.get_item(key).ok().flatten())
}

/// Whether the core runs in this tab (not in another tab this one follows). Calls need it: the
/// device key that signs a call's keys stays with the core.
pub fn runs_here() -> bool {
  RUNNING.with(|running| matches!(running.borrow().as_ref(), Some(Running::Owner { .. })))
}

/// A join token, the signer and the verified devices for the conversation's call (e2ee.md §8).
/// Only in the tab that runs the core.
pub async fn prepare_call(conversation: ConversationId) -> Result<CallPreparation, String> {
  ask(|reply| Request::PrepareCall(conversation, reply)).await?
}

/// The running call's verified devices afresh, after catching up the conversation's group: a
/// device added during the call joins it (e2ee.md §8.2). Only in the tab that runs the core.
pub async fn call_devices(conversation: ConversationId) -> Result<Vec<VerifiedDevice>, String> {
  ask(|reply| Request::CallDevices(conversation, reply)).await?
}

/// Whether the workspace requires an organization background behind the camera image for this
/// user (admin.md §3.5). Unknown restricts nothing, as on desktop.
pub async fn requires_org_background() -> bool {
  org_backgrounds().await.is_required()
}

/// The organization's call backgrounds and their rule for this user; unknown restricts nothing,
/// as on desktop.
pub async fn org_backgrounds() -> OrgBackgrounds {
  match ask(|reply| Request::Query(ChatQuery::OrgBackgrounds, reply)).await {
    Ok(Ok(ChatAnswer::OrgBackgrounds { backgrounds })) => backgrounds,
    _ => OrgBackgrounds::default(),
  }
}

/// The picture file behind the camera for `effect` (#439): the user's own, decrypted, or the
/// organization's, hash-checked. Only in the tab that runs the core.
pub async fn background_picture(effect: BackgroundEffect) -> Result<Vec<u8>, String> {
  ask(|reply| Request::BackgroundPicture(effect, reply)).await?
}

/// Deletes the database of a guest device an earlier page left behind (closed without "Leave").
/// Best effort: a database another tab holds open stays until that tab closes.
pub async fn delete_stale_guest_store() {
  let Some(user) = load_string(GUEST_STORE_KEY) else {
    return;
  };
  match IndexedDbStore::delete(&format!("teamcraft-{user}")).await {
    Ok(()) => store_string(GUEST_STORE_KEY, ""),
    Err(error) => tracing::debug!(%error, "could not delete an earlier guest device's database"),
  }
}

/// Answers one `core_*` command the way the desktop shell does.
pub async fn call(command: &str, args: Value) -> Result<Value, String> {
  match command {
    "core_start" => {
      let user: UserId = field(&args, "user")?;
      to_value(start(user).await?)
    },
    "core_stop" => {
      // Signing out of a tab that isn't set up yet leaves no keys of the attempt behind (#168).
      if current_follower().is_none() {
        // An error means the core isn't running here: nothing to forget.
        let _ = ask(Request::ForgetUnfinishedSetup).await;
      }
      stop();
      Ok(Value::Null)
    },
    _ => match current_follower() {
      Some(follower) => follower.call(command, args).await,
      None => serve(command, args).await,
    },
  }
}

/// A call from a follower tab. Starting and stopping stay with the tab that asks: a follower's
/// start only asks where the core stands, its stop only ends its own link.
async fn serve_follower(command: String, args: Value) -> Result<Value, String> {
  match command.as_str() {
    "core_start" => to_value(ask(Request::Status).await?),
    "core_stop" => Ok(Value::Null),
    _ => serve(&command, args).await,
  }
}

/// Runs a command on this tab's core.
async fn serve(command: &str, args: Value) -> Result<Value, String> {
  match command {
    "core_status" => to_value(ask(Request::Status).await?),
    "core_connection" => to_value(connection_state()?),
    "core_network_changed" => {
      // Calls run in this tab too and hear the same signal (as on desktop).
      crate::web_calls::network_changed();
      send(Request::NetworkChanged)?;
      Ok(Value::Null)
    },
    "core_focus" => {
      send(Request::Focus(field(&args, "conversation")?))?;
      Ok(Value::Null)
    },
    "core_store_health" => to_value(ask(Request::StoreHealth).await?),
    "core_enroll" => {
      let request: EnrollRequest = field(&args, "request")?;
      ask(|reply| Request::Enroll(request, reply))
        .await?
        .map(|()| Value::Null)
    },
    "core_wipe_guest" => {
      ask(Request::WipeGuestStore).await??;
      Ok(Value::Null)
    },
    "core_own_key" => {
      let (fingerprint, code) = ask(Request::OwnKey).await??;
      Ok(serde_json::json!({ "fingerprint": fingerprint, "code": code }))
    },
    "core_guest_anchor" => {
      let (conversation, fingerprint) = (field(&args, "conversation")?, field(&args, "fingerprint")?);
      ask(|reply| Request::GuestAnchor {
        conversation,
        fingerprint,
        reply,
      })
      .await??;
      Ok(Value::Null)
    },
    "core_snapshot" => to_value(ask(Request::Snapshot).await??),
    "core_snapshot_changes" => {
      let maybe_since: Option<SnapshotCursor> = field(&args, "since")?;
      to_value(ask(|reply| Request::SnapshotChanges(maybe_since, reply)).await??)
    },
    "core_messages" => {
      let (conversation, start, len) = (
        field(&args, "conversation")?,
        field(&args, "start")?,
        field(&args, "len")?,
      );
      let window = ask(|reply| Request::Messages {
        conversation,
        start,
        len,
        reply,
      })
      .await??;
      to_value(window)
    },
    "core_thread" => {
      let (conversation, thread) = (field(&args, "conversation")?, field(&args, "thread")?);
      to_value(
        ask(|reply| Request::Thread {
          conversation,
          thread,
          reply,
        })
        .await??,
      )
    },
    "core_action" => {
      let action: ChatAction = field(&args, "action")?;
      ask(|reply| Request::Action(Box::new(action), reply)).await??;
      Ok(Value::Null)
    },
    "core_query" => {
      let query: ChatQuery = field(&args, "query")?;
      to_value(ask(|reply| Request::Query(query, reply)).await??)
    },
    // Only a follower tab asks for a file by command; it gets the bytes as base64 over the tab channel.
    "core_attachment" => {
      let (conversation, message, file_id) = (
        field(&args, "conversation")?,
        field(&args, "message")?,
        field(&args, "file_id")?,
      );
      // An older leader tab's follower sends none: a preview.
      let priority = field::<Option<AttachmentPriority>>(&args, "priority")?.unwrap_or_default();
      let plaintext = ask_attachment(conversation, message, file_id, priority).await?;
      Ok(Value::String(BASE64.encode(plaintext.expose_secret())))
    },
    "core_link_start" => {
      let device_name: String = field(&args, "device_name")?;
      ask(|reply| Request::LinkStart { device_name, reply }).await??;
      Ok(Value::Null)
    },
    "core_link_status" => to_value(ask(Request::LinkStatus).await?),
    "core_enable_push" => {
      let vapid_key: String = field(&args, "vapid_key")?;
      ask(|reply| Request::EnablePush { vapid_key, reply }).await??;
      Ok(Value::Null)
    },
    "core_link_cancel" => {
      send(Request::LinkCancel)?;
      Ok(Value::Null)
    },
    "core_reset_store" => {
      ask(Request::ResetStore).await??;
      Ok(Value::Null)
    },
    "core_approval_start" => {
      let code: String = field(&args, "code")?;
      ask(|reply| Request::ApprovalStart { code, reply }).await??;
      Ok(Value::Null)
    },
    "core_approval_confirm" => {
      let short_code: String = field(&args, "short_code")?;
      ask(|reply| Request::ApprovalConfirm { short_code, reply }).await??;
      Ok(Value::Null)
    },
    "core_approval_cancel" => {
      send(Request::ApprovalCancel)?;
      Ok(Value::Null)
    },
    "core_phone_link_start" => {
      ask(Request::PhoneLinkStart).await??;
      Ok(Value::Null)
    },
    "core_phone_link_status" => to_value(ask(Request::PhoneLinkStatus).await?),
    "core_link_announce" => {
      ask(Request::LinkAnnounce).await??;
      Ok(Value::Null)
    },
    "core_link_requests" => to_value(ask(Request::LinkRequests).await?),
    "core_link_request_review" => {
      let id: String = field(&args, "id")?;
      ask(|reply| Request::LinkRequestReview { id, reply }).await??;
      Ok(Value::Null)
    },
    "core_link_request_reject" => {
      let id: String = field(&args, "id")?;
      ask(|reply| Request::LinkRequestReject { id, reply }).await??;
      Ok(Value::Null)
    },
    other => Err(format!("{other} is not available in the browser")),
  }
}

/// One file of a message, downloaded and decrypted in memory, for the image viewer. A follower tab
/// gets it from the tab that runs the core.
pub async fn attachment(
  conversation: ConversationId,
  message: MessageId,
  file_id: String,
  priority: AttachmentPriority,
) -> Result<SecretBytes, String> {
  let Some(follower) = current_follower() else {
    return ask_attachment(conversation, message, file_id, priority).await;
  };
  let args = serde_json::json!({
    "conversation": conversation,
    "message": message,
    "file_id": file_id,
    "priority": priority,
  });
  let answer = follower.call("core_attachment", args).await?;
  let encoded = answer.as_str().ok_or("unreadable answer of core_attachment")?;
  BASE64
    .decode(encoded)
    .map(SecretBytes::new)
    .map_err(|_| "unreadable answer of core_attachment".to_owned())
}

async fn ask_attachment(
  conversation: ConversationId,
  message: MessageId,
  file_id: String,
  priority: AttachmentPriority,
) -> Result<SecretBytes, String> {
  ask(|reply| Request::Attachment {
    conversation,
    message,
    file_id,
    priority,
    reply,
  })
  .await?
}

fn field<T: DeserializeOwned>(args: &Value, name: &str) -> Result<T, String> {
  let value = args.get(name).cloned().unwrap_or(Value::Null);
  serde_json::from_value(value).map_err(|error| format!("bad argument {name}: {error}"))
}

fn to_value<T: serde::Serialize>(value: T) -> Result<Value, String> {
  serde_json::to_value(value).map_err(|error| error.to_string())
}

/// Starts (or reuses) the core for `user` and reports where it stands. The core runs in this tab
/// when it gets the store's lock; otherwise this tab is inactive and follows the one that has it,
/// unless it is the active tab coming back from a reload.
async fn start(user: UserId) -> Result<CoreStatus, String> {
  let reuse = RUNNING.with(|running| {
    running
      .borrow()
      .as_ref()
      .is_some_and(|running| running.user() == user)
  });
  if !reuse {
    match web_tabs::try_own(user).await {
      Ownership::Owned(lock) => {
        start_owner(user, Some(lock)).await?;
        became_active(user);
      },
      Ownership::Unsupported => start_owner(user, None).await?,
      Ownership::Taken => start_where_taken(user).await?,
    }
  }
  match current_follower() {
    Some(follower) => {
      let status = follower.call("core_start", Value::Null).await?;
      serde_json::from_value(status).map_err(|error| error.to_string())
    },
    None => ask(Request::Status).await,
  }
}

/// Runs the core in this tab. With a lock, other tabs of the account can reach it.
async fn start_owner(user: UserId, maybe_lock: Option<OwnerLock>) -> Result<(), String> {
  {
    let (auth, server_url) = SETUP
      .with(|setup| {
        setup
          .borrow()
          .as_ref()
          .map(|setup| (setup.auth.clone(), setup.server_url.clone()))
      })
      .ok_or("the web client is not configured")?;
    let tokens = WebTokens::default();
    // The first token must be there before the core makes its first request.
    let first = auth
      .access_token()
      .await
      .map_err(|error: AuthError| error.to_string())?;
    *tokens.0.borrow_mut() = Some(first);
    let (sender, receiver) = mpsc::unbounded();
    let maybe_channel = match maybe_lock.is_some() {
      true => Some(OwnerChannel::open(user, serve_follower, on_takeover_request)?),
      false => None,
    };
    let connection = ConnectionStatus::new();
    let core = Core::new(server_url, user, tokens.clone(), connection.clone(), maybe_lock);
    watch_worker();
    let generation = std::rc::Rc::new(());
    keep_token_fresh(auth, tokens, Rc::downgrade(&generation));
    wasm_bindgen_futures::spawn_local(async move {
      core.run(receiver).await;
      drop(generation);
    });
    // Replacing a running core (another account signed in) drops its sender, which stops it.
    // async-ok: a thread-local, not a signal.
    RUNNING.with(|running| {
      *running.borrow_mut() = Some(Running::Owner {
        user,
        requests: sender,
        connection,
        maybe_channel,
      })
    });
  }
  Ok(())
}

/// Another tab holds the store at this tab's start: this tab follows it, and takes over by itself
/// when the lock comes free. A reloaded active tab claims it, so it wins that moment over the other
/// inactive tabs, and waits briefly for it; any other tab shows the notice.
async fn start_where_taken(user: UserId) -> Result<(), String> {
  let follower = start_follower(user)?;
  let maybe_was_active_for = session_storage_get(ACTIVE_TAB_KEY);
  if on_taken(maybe_was_active_for.as_deref(), &user.to_string()) == OnTaken::WaitForOwnLock {
    follower.set_claiming(true);
    let waited = select(Box::pin(activation()), Box::pin(platform::sleep(RELOAD_GRACE))).await;
    follower.set_claiming(false);
    if matches!(waited, Either::Left(_)) {
      return Ok(());
    }
  }
  session_storage_set(ACTIVE_TAB_KEY, None);
  update_tab(TabEvent::FoundActiveElsewhere);
  Ok(())
}

/// Follows the tab that owns the store; takes over when the user asks ([`use_here`]) or the owner
/// goes away ([`take_over_when_free`]).
fn start_follower(user: UserId) -> Result<Rc<Follower>, String> {
  tracing::info!("TeamCraft is open in another tab");
  let follower = Rc::new(Follower::join(user, notify_local)?);
  RUNNING.with(|running| {
    *running.borrow_mut() = Some(Running::Follower {
      user,
      follower: follower.clone(),
    })
  });
  watch_focus();
  wasm_bindgen_futures::spawn_local(take_over_when_free(user));
  Ok(follower)
}

/// Waits for the store's lock; once the owner let go (closed, crashed, discarded, or handed over),
/// the tab that gets it asks the other inactive tabs and keeps it only if it is the one that
/// should run the app: the one where "Use here" was pressed, else the one focused last. Otherwise
/// it passes the lock on and waits again behind them.
async fn take_over_when_free(user: UserId) {
  let mut yields_in_row = 0;
  loop {
    let Some(lock) = web_tabs::wait_to_own(user).await else {
      return;
    };
    // Signed out, another account, or active already: dropping the lock hands it on.
    let Some(follower) = follower_of(user) else {
      return;
    };
    let others = follower.roll_call().await;
    if follower_of(user).is_none() {
      return;
    }
    if !keeps_lock(&follower.candidate(), &others, yields_in_row) {
      yields_in_row += 1;
      drop(lock);
      // A moment for the tab it should go to, which waits in the queue already.
      let jitter_ms = 50.0 + js_sys::Math::random() * 100.0;
      platform::sleep(Duration::from_millis(jitter_ms as u64)).await;
      continue;
    }
    match start_owner(user, Some(lock)).await {
      Ok(()) => {
        tracing::info!("TeamCraft runs in this tab now");
        became_active(user);
        notify_local();
      },
      Err(error) => tracing::error!(%error, "could not take over the local data"),
    }
    return;
  }
}

/// The follower of `user`'s owner, while this tab is one.
fn follower_of(user: UserId) -> Option<Rc<Follower>> {
  RUNNING.with(|running| match &*running.borrow() {
    Some(Running::Follower {
      user: following,
      follower,
    }) if *following == user => Some(follower.clone()),
    _ => None,
  })
}

/// Notes when this tab was last focused or shown, for the choice of the next active tab. Once per
/// page.
fn watch_focus() {
  if IS_WATCHING_FOCUS.with(|is_watching| is_watching.replace(true)) {
    return;
  }
  let Some(window) = web_sys::window() else {
    return;
  };
  let on_focus = wasm_bindgen::closure::Closure::<dyn Fn()>::new(|| {
    let is_visible = web_sys::window()
      .and_then(|window| window.document())
      .is_some_and(|document| !document.hidden());
    if is_visible && let Some(follower) = current_follower() {
      follower.mark_focused();
    }
  });
  let mut is_listening = window
    .add_event_listener_with_callback("focus", on_focus.as_ref().unchecked_ref())
    .is_ok();
  if let Some(document) = window.document() {
    is_listening &= document
      .add_event_listener_with_callback("visibilitychange", on_focus.as_ref().unchecked_ref())
      .is_ok();
  }
  if !is_listening {
    tracing::debug!("could not watch the tab's focus");
  }
  // The listeners live as long as the page.
  on_focus.forget();
}

/// Another tab asks for the app. A running call refuses it unless the user there agreed to end it.
fn on_takeover_request(ends_call: bool) -> Option<Blocker> {
  match answer_takeover(crate::web_calls::is_in_call(), ends_call) {
    TakeoverAnswer::Refuse(blocker) => Some(blocker),
    TakeoverAnswer::HandOver { ends_call } => {
      wasm_bindgen_futures::spawn_local(hand_over(ends_call));
      None
    },
  }
}

/// Lets another tab run the app: ends the call (when the user agreed), stores the drafts and waits
/// for IndexedDB to have the outbox, then stops the core, whose lock goes to the waiting tab. This
/// tab then follows it and says so.
async fn hand_over(ends_call: bool) {
  let Some(user) = RUNNING.with(|running| match &*running.borrow() {
    Some(Running::Owner { user, .. }) => Some(*user),
    _ => None,
  }) else {
    return;
  };
  if ends_call {
    crate::web_calls::end_call().await;
  }
  persist_drafts().await;
  let flushed = Box::pin(ask(Request::Flush));
  match select(flushed, Box::pin(platform::sleep(HANDOVER_FLUSH_TIMEOUT))).await {
    Either::Left((Ok(Ok(())), _)) => {},
    Either::Left((Ok(Err(error)), _)) => tracing::warn!(%error, "local data not saved before handing over"),
    Either::Left((Err(error), _)) => tracing::debug!(%error, "no core to save before handing over"),
    Either::Right(_) => tracing::warn!("local data still being saved; handing over anyway"),
  }
  if !is_owner_of(user) {
    return;
  }
  tracing::info!("TeamCraft moved to another tab");
  session_storage_set(ACTIVE_TAB_KEY, None);
  // Replacing the owner drops the core's requests, which stops it and releases the lock.
  if let Err(error) = start_follower(user).map(drop) {
    tracing::warn!(%error, "could not follow the other tab");
    stop();
  }
  update_tab(TabEvent::HandedAway);
  notify_local();
}

/// Whether this tab still runs the core of `user`.
fn is_owner_of(user: UserId) -> bool {
  RUNNING
    .with(|running| matches!(&*running.borrow(), Some(Running::Owner { user: owner, .. }) if *owner == user))
}

/// Asks the composers to store their drafts now, then gives their writes a moment to reach the
/// core's queue ahead of the flush.
async fn persist_drafts() {
  if let Some(window) = web_sys::window()
    && let Ok(event) = web_sys::Event::new(PERSIST_DRAFTS_EVENT)
    && window.dispatch_event(&event).is_err()
  {
    tracing::debug!("could not ask the composers for their drafts");
  }
  platform::sleep(Duration::from_millis(100)).await;
}

fn current_follower() -> Option<Rc<Follower>> {
  RUNNING.with(|running| match &*running.borrow() {
    Some(Running::Follower { follower, .. }) => Some(follower.clone()),
    _ => None,
  })
}

/// The owner's connection; a follower's calls are answered by the owner.
fn connection_state() -> Result<CoreConnection, String> {
  RUNNING.with(|running| match &*running.borrow() {
    Some(Running::Owner { connection, .. }) => Ok(connection.get().into()),
    Some(Running::Follower { .. }) => Err("the core runs in another tab".to_owned()),
    None => Err("the core has not been started".to_owned()),
  })
}

fn stop() {
  RUNNING.with(|running| *running.borrow_mut() = None);
}

fn send(request: Request) -> Result<(), String> {
  RUNNING.with(|running| match &*running.borrow() {
    Some(Running::Owner { requests, .. }) => requests.unbounded_send(request).map_err(|_| stopped()),
    Some(Running::Follower { .. }) => Err("the core runs in another tab".to_owned()),
    None => Err("the core has not been started".to_owned()),
  })
}

async fn ask<T>(build: impl FnOnce(oneshot::Sender<T>) -> Request) -> Result<T, String> {
  let (reply, answer) = oneshot::channel();
  send(build(reply))?;
  answer.await.map_err(|_| stopped())
}

fn stopped() -> String {
  "the core is not running".to_owned()
}

/// Renews the core's token copy from the page's session until the core stops.
fn keep_token_fresh(auth: Rc<dyn Authenticator>, tokens: WebTokens, alive: std::rc::Weak<()>) {
  wasm_bindgen_futures::spawn_local(async move {
    loop {
      platform::sleep(TOKEN_REFRESH).await;
      if alive.upgrade().is_none() {
        return;
      }
      match auth.access_token().await {
        Ok(token) => *tokens.0.borrow_mut() = Some(token),
        Err(error) => tracing::warn!(%error, "could not renew the access token"),
      }
    }
  });
}

/// Tells this tab's UI, and the follower tabs, that the core's data changed.
fn notify_changed() {
  RUNNING.with(|running| {
    if let Some(Running::Owner {
      maybe_channel: Some(channel),
      ..
    }) = &*running.borrow()
    {
      channel.announce_change();
    }
  });
  notify_local();
}

fn notify_local() {
  let listeners: Vec<_> = LISTENERS.with(|listeners| listeners.borrow().clone());
  for listener in listeners {
    if let Ok(mut callback) = listener.try_borrow_mut() {
      (*callback)();
    }
  }
}

enum Request {
  Status(oneshot::Sender<CoreStatus>),
  StoreHealth(oneshot::Sender<StoreHealth>),
  /// Waits until IndexedDB has everything written so far (before another tab takes over).
  Flush(oneshot::Sender<Result<(), String>>),
  /// The service worker saw the browser change the push subscription.
  CheckPush,
  Enroll(EnrollRequest, oneshot::Sender<Result<(), String>>),
  /// A guest leaves: this device's database is deleted (guest devices only).
  WipeGuestStore(oneshot::Sender<Result<(), String>>),
  /// This device's identity key fingerprint and key code (a guest's page shows the code and reports
  /// the fingerprint with its device).
  OwnKey(oneshot::Sender<Result<(String, String), String>>),
  /// A guest's page keeps the organizer's key from the link for a conversation (e2ee.md §4.6).
  GuestAnchor {
    conversation: ConversationId,
    fingerprint: String,
    reply: oneshot::Sender<Result<(), String>>,
  },
  Snapshot(oneshot::Sender<Result<ChatSnapshot, String>>),
  /// What changed in the snapshot since the reader's cursor (issue #246).
  SnapshotChanges(
    Option<SnapshotCursor>,
    oneshot::Sender<Result<SnapshotChanges, String>>,
  ),
  Messages {
    conversation: ConversationId,
    start: usize,
    len: usize,
    reply: oneshot::Sender<Result<MessageWindow, String>>,
  },
  Thread {
    conversation: ConversationId,
    thread: ThreadId,
    reply: oneshot::Sender<Result<Vec<Message>, String>>,
  },
  Action(Box<ChatAction>, oneshot::Sender<Result<(), String>>),
  Query(ChatQuery, oneshot::Sender<Result<ChatAnswer, String>>),
  Attachment {
    conversation: ConversationId,
    message: MessageId,
    file_id: String,
    priority: AttachmentPriority,
    reply: oneshot::Sender<Result<SecretBytes, String>>,
  },
  LinkStart {
    device_name: String,
    reply: oneshot::Sender<Result<(), String>>,
  },
  LinkStatus(oneshot::Sender<LinkStatus>),
  EnablePush {
    vapid_key: String,
    reply: oneshot::Sender<Result<(), String>>,
  },
  LinkCancel,
  /// The local database is unreadable: delete it and start this device afresh (issue #160).
  ResetStore(oneshot::Sender<Result<(), String>>),
  /// The user signs out: drop what an unfinished enrollment kept (issue #168).
  ForgetUnfinishedSetup(oneshot::Sender<()>),
  ApprovalStart {
    code: String,
    reply: oneshot::Sender<Result<(), String>>,
  },
  ApprovalConfirm {
    short_code: String,
    reply: oneshot::Sender<Result<(), String>>,
  },
  ApprovalCancel,
  /// Signed-in device: show a code for a new phone to scan (issue #998).
  PhoneLinkStart(oneshot::Sender<Result<(), String>>),
  PhoneLinkStatus(oneshot::Sender<PhoneLinkStatus>),
  LinkAnnounce(oneshot::Sender<Result<(), String>>),
  LinkRequests(oneshot::Sender<Vec<LinkRequestView>>),
  LinkRequestReview {
    id: String,
    reply: oneshot::Sender<Result<(), String>>,
  },
  LinkRequestReject {
    id: String,
    reply: oneshot::Sender<Result<(), String>>,
  },
  /// The browser went online, the tab became visible or was resumed: check the connection now.
  NetworkChanged,
  /// What joining a conversation's call needs (`web_calls`).
  PrepareCall(ConversationId, oneshot::Sender<Result<CallPreparation, String>>),
  CallDevices(
    ConversationId,
    oneshot::Sender<Result<Vec<VerifiedDevice>, String>>,
  ),
  /// The picture behind a camera: the user's own or the organization's (#439).
  BackgroundPicture(BackgroundEffect, oneshot::Sender<Result<Vec<u8>, String>>),
  /// The user opened a conversation; a running catch-up takes it next (issue #247).
  Focus(ConversationId),
}

/// What woke the loop.
enum Wake {
  Request(Option<Request>),
  Frame(Option<Result<GatewayServerMessage, ApiError>>),
  Linked(Result<LinkedSecrets, String>),
  /// Storage answered the running blob upload.
  Uploaded(Result<(), ApiError>),
  /// A download beside the loop finished.
  Downloaded(Finished),
  Tick,
}

struct Core {
  /// Held while the core runs; other tabs of the account follow this one (e2ee.md §9.4).
  _maybe_lock: Option<OwnerLock>,
  server_url: String,
  user: UserId,
  tokens: WebTokens,
  store: Option<Store>,
  client: Option<Client<Api, Store>>,
  backend: Option<CoreChatBackend<Store>>,
  connection: ConnectionSupervisor<<Api as ServerApi>::Gateway>,
  status: CoreStatus,
  /// When the next tick is due. A deadline rather than a fresh sleep per wake-up, so a busy
  /// connection or a chatty UI can't postpone heartbeats until the server drops the connection.
  next_tick_ms: i64,
  upkeep: Upkeep,
  link: Option<LinkRun>,
  /// The access token last written to the push session, so it is only stored when it changes.
  last_session_token: Option<String>,
  /// Why the last link failed, until the user tries again.
  link_failure: LinkStatus,
  approval: Option<LinkApproval<<Api as ServerApi>::Rendezvous>>,
  approval_misses: u8,
  /// A code this signed-in device shows for a new phone (issue #998).
  maybe_phone_link: Option<PhoneLinkRun>,
  /// Stops the wait for the receipt of the keys this device sent, which runs on a task of its own
  /// (issue #415).
  maybe_receipt: Option<ReceiptCancel>,
  /// The last attention report, repeated after a reconnect (the server forgets it with the
  /// connection).
  maybe_attention: Option<bool>,
  /// Whether this device last said it is idle, repeated after a reconnect: a resumed session keeps
  /// what the server last knew, which may be an away timeout that ended meanwhile.
  is_idle: bool,
  /// The user's own typing, sent on the gateway at most every few seconds (#733).
  typing: TypingAnnouncer,
  /// Whether IndexedDB keeps up with the cache, as of the last flush.
  store_health: StoreHealth,
  /// The flush [`Self::check_store`] waits for, while IndexedDB is still writing.
  maybe_flush: Option<Flush>,
  /// When to compare the browser's push subscription with the registration next; `None` when
  /// nothing is due (e2ee.md §7).
  maybe_push_check_at_ms: Option<i64>,
  /// The security part of the last snapshot, for snapshots taken while a sync holds the client.
  maybe_last_security: Option<SecuritySnapshot>,
  /// The UI's snapshot, rebuilt per conversation from what the store's writes touched (#246).
  snapshots: RefCell<SnapshotCache>,
  /// The conversation the UI reads, which a catch-up takes first (issue #247); set also while a
  /// sync holds the client.
  catch_up_focus: CatchUpFocus,
  /// Requests that arrived during a connect and need the client; handled once it is done.
  deferred: VecDeque<Request>,
  /// The UI side hung up during a connect: the loop ends after it.
  is_closed: bool,
  /// Messages were written to the outbox and answered at once; sending them is still to do.
  has_queued_sends: Cell<bool>,
  /// The file going up beside the loop, so reads and sends are answered meanwhile.
  maybe_upload: Option<BlobUpload>,
  /// A file may be waiting for its upload: one was queued, a retry came due, or one finished.
  is_upload_due: bool,
  /// The bytes the transfers in flight had moved when the UI was last told.
  maybe_reported_bytes: Option<u64>,
  /// Attachment downloads, running beside the loop (issue #491).
  downloads: Downloads,
}

/// A link this signed-in device approves, waiting for the code the new device shows.
type Approval = LinkApproval<<Api as ServerApi>::Rendezvous>;

/// A code shown for a new phone, waiting beside the loop until the phone scanned it. Dropping it
/// closes the relay.
struct PhoneLinkRun {
  code: String,
  maybe_expires_at_ms: Option<i64>,
  scanned: Rc<RefCell<Option<Result<Approval, String>>>>,
  /// The outcome once taken: `Scanned` moved the approval to [`Core::approval`].
  maybe_outcome: Option<PhoneLinkStatus>,
  _cancel: LinkWaitCancel,
}

/// A link in progress on this (new) device.
struct LinkRun {
  device_name: String,
  view: Rc<RefCell<LinkStatus>>,
  future: Pin<Box<dyn Future<Output = Result<LinkedSecrets, String>>>>,
}

impl Core {
  fn new(
    server_url: String,
    user: UserId,
    tokens: WebTokens,
    status: ConnectionStatus,
    maybe_lock: Option<OwnerLock>,
  ) -> Self {
    let mut connection = ConnectionSupervisor::new(ConnectionSettings::default(), status);
    connection.set_observer(Box::new(|_| notify_changed()));
    Self {
      _maybe_lock: maybe_lock,
      server_url,
      user,
      tokens,
      store: None,
      client: None,
      backend: None,
      connection,
      status: CoreStatus::NotStarted,
      next_tick_ms: i64::MIN,
      upkeep: Upkeep::new(UPKEEP_INTERVAL).with_uploads(UploadMode::Host),
      link: None,
      last_session_token: None,
      link_failure: LinkStatus::Idle,
      approval: None,
      approval_misses: 0,
      maybe_phone_link: None,
      maybe_receipt: None,
      maybe_attention: None,
      is_idle: false,
      typing: TypingAnnouncer::default(),
      store_health: StoreHealth::Saved,
      maybe_flush: None,
      maybe_push_check_at_ms: None,
      maybe_last_security: None,
      snapshots: RefCell::new(SnapshotCache::new(now_ms().unsigned_abs())),
      catch_up_focus: CatchUpFocus::default(),
      deferred: VecDeque::new(),
      is_closed: false,
      has_queued_sends: Cell::new(false),
      maybe_upload: None,
      is_upload_due: true,
      maybe_reported_bytes: None,
      downloads: Downloads::default(),
    }
  }

  async fn run(mut self, mut requests: mpsc::UnboundedReceiver<Request>) {
    self.open().await;
    loop {
      self.connect(&mut requests).await;
      while let Some(request) = self.deferred.pop_front() {
        self.handle(request).await;
      }
      if self.is_closed {
        return;
      }
      // Messages answered meanwhile (also during a connect) go out now; then the next file.
      if self.has_queued_sends.take() {
        self.send_queued(&mut requests).await;
      }
      self.start_upload(&mut requests).await;
      self.start_downloads().await;
      match self.next_wake(&mut requests).await {
        Wake::Request(None) => return,
        // A large file must not hold up the loop while it downloads.
        Wake::Request(Some(Request::Attachment {
          conversation,
          message,
          file_id,
          priority,
          reply,
        })) => self.queue_attachment(conversation, message, &file_id, priority, reply),
        Wake::Request(Some(request)) => self.handle(request).await,
        Wake::Frame(frame) => self.on_frame(frame).await,
        Wake::Linked(result) => self.on_linked(result).await,
        Wake::Uploaded(sent) => self.on_uploaded(sent, &mut requests).await,
        Wake::Downloaded(finished) => {
          if let Some(client) = self.client.as_mut()
            && Downloads::finish(client, finished)
          {
            notify_changed();
          }
        },
        Wake::Tick => self.on_tick().await,
      }
      self.report_progress();
      self.check_store();
      self.check_push().await;
      self.report_background_failures();
    }
  }

  /// Refusals the core found while it sent queued actions in the background (a custom status the
  /// workspace no longer allows): shown, since no button waits for them (issue #322).
  fn report_background_failures(&mut self) {
    let Some(client) = self.client.as_mut() else {
      return;
    };
    for error in client.take_background_failures() {
      crate::feedback::report_background_failure(error.to_failure());
    }
  }

  /// Registers the browser's push subscription again when the browser rotated or dropped it.
  async fn check_push(&mut self) {
    let (Some(client), Some(due_at)) = (self.client.as_mut(), self.maybe_push_check_at_ms) else {
      return;
    };
    if now_ms() < due_at {
      return;
    }
    self.maybe_push_check_at_ms = match client.check_web_push().await {
      Ok(is_renewed) => {
        if is_renewed {
          tracing::info!("the browser changed the push subscription; registered the new one");
          self.last_session_token = None;
          self.save_push_session();
        }
        None
      },
      Err(error) => {
        tracing::warn!(%error, "could not check the push subscription");
        Some(now_ms().saturating_add(millis(PUSH_CHECK_RETRY)))
      },
    };
  }

  /// Tells the UI when IndexedDB stops (or starts again) keeping up with what was written: what
  /// is not saved is lost on reload (e2ee.md §9.4). Never waits: after a large catch-up IndexedDB
  /// can take minutes to write everything, and a loop waiting for that sends no heartbeats, so the
  /// server drops the connection. The flush runs on and is looked at again on the next turn.
  fn check_store(&mut self) {
    let Some(store) = self.store.as_ref() else {
      return;
    };
    let flush = self.maybe_flush.get_or_insert_with(|| {
      let store = store.clone();
      Box::pin(async move { store.inner().flush().await })
    });
    let Some(outcome) = flush.as_mut().now_or_never() else {
      return;
    };
    self.maybe_flush = None;
    let health = match outcome {
      Ok(()) => StoreHealth::Saved,
      Err(StoreError::QuotaExceeded) => StoreHealth::QuotaExceeded,
      Err(StoreError::Closed) => StoreHealth::Closed,
      Err(error) => {
        tracing::warn!(%error, "local data not saved");
        StoreHealth::Failed
      },
    };
    if health != self.store_health {
      tracing::info!(?health, "local storage state changed");
      self.store_health = health;
      notify_changed();
    }
  }

  async fn next_wake(&mut self, requests: &mut mpsc::UnboundedReceiver<Request>) -> Wake {
    // The connection's deadlines count only once there is a client to connect.
    let deadline = match (&self.client, self.connection.next_deadline_ms()) {
      (Some(_), Some(connection)) => connection.min(self.next_tick_ms),
      _ => self.next_tick_ms,
    };
    // While a file goes up, the UI reads its progress again every so often.
    let deadline = match self.maybe_reported_bytes {
      Some(_) => deadline.min(now_ms().saturating_add(millis(TRANSFER_PROGRESS))),
      None => deadline,
    };
    // Someone stopped typing: no event says so, the indicator runs out ([`Self::on_tick`]).
    let deadline = match self.client.as_ref().and_then(Client::typing_change_at_ms) {
      Some(typing_change) => deadline.min(typing_change),
      None => deadline,
    };
    let until_tick_ms = deadline.saturating_sub(now_ms()).max(0);
    let Self {
      connection,
      link,
      maybe_upload,
      downloads,
      ..
    } = self;
    let frame = Box::pin(connection.receive());
    let linked = Box::pin(async {
      match link {
        Some(run) => run.future.as_mut().await,
        None => std::future::pending().await,
      }
    });
    let uploaded = Box::pin(async {
      match maybe_upload {
        Some(upload) => upload.sent().await,
        None => std::future::pending().await,
      }
    });
    let tick = Box::pin(platform::sleep(Duration::from_millis(
      until_tick_ms.unsigned_abs(),
    )));
    let downloaded = Box::pin(downloads.next_finished());
    let transfers = select(uploaded, downloaded);
    let other = select(select(frame, linked), select(transfers, tick));
    match select(requests.next(), other).await {
      Either::Left((request, _)) => Wake::Request(request),
      Either::Right((Either::Left((Either::Left((frame, _)), _)), _)) => Wake::Frame(frame),
      Either::Right((Either::Left((Either::Right((linked, _)), _)), _)) => Wake::Linked(linked),
      Either::Right((Either::Right((Either::Left((Either::Left((sent, _)), _)), _)), _)) => {
        Wake::Uploaded(sent)
      },
      Either::Right((Either::Right((Either::Left((Either::Right((finished, _)), _)), _)), _)) => {
        Wake::Downloaded(finished)
      },
      Either::Right((Either::Right((Either::Right(((), _)), _)), _)) => Wake::Tick,
    }
  }

  /// Opens the database and the client of an already enrolled device.
  async fn open(&mut self) {
    match self.try_open().await {
      Ok(status) => self.status = status,
      Err(message) => {
        tracing::error!(%message, "could not open the local client");
        self.status = CoreStatus::Failed { message };
      },
    }
  }

  async fn try_open(&mut self) -> Result<CoreStatus, String> {
    let name = format!("teamcraft-{}", self.user);
    announce_database(&name);
    request_persistent_storage();
    let key = browser_database_key(&name)
      .await
      .map_err(|error| error.to_string())?;
    let opened = match IndexedDbStore::open(&name, &key).await {
      Ok(opened) => opened,
      Err(error) => return unreadable_status(error.into()),
    };
    let store = TrackedStore::with_message_cache(Rc::new(opened), MESSAGE_CACHE_MESSAGES);
    self.store = Some(store.clone());
    watch_page_lifecycle(store.inner());
    let maybe_client = match Client::open(
      self.api(),
      store.clone(),
      Box::new(SystemClock),
      ClientSettings::default().with_notes_to_self(),
    ) {
      Ok(maybe_client) => maybe_client,
      Err(error) => return unreadable_status(error),
    };
    match maybe_client {
      Some(client) => Ok(self.adopt(client)),
      None => {
        if let Some(client) = self.resume_link(store).await? {
          return Ok(self.adopt(client));
        }
        self.drop_abandoned_setup().await;
        // The browser has no OS keychain (e2ee.md §6.5.6): the recovery kit is the way back.
        Ok(CoreStatus::NeedsEnrollment {
          has_existing_identity: self.has_existing_identity().await?,
          was_data_lost: load_string(&self.store_ready_key()).is_some(),
          can_restore_on_device: false,
        })
      },
    }
  }

  /// A device link whose keys arrived before the page went away finishes enrolling now (#168).
  async fn resume_link(&self, store: Store) -> Result<Option<Client<Api, Store>>, String> {
    let resumed = PendingEnrollment::resume_link(
      self.api(),
      store,
      Box::new(SystemClock),
      ClientSettings::default().with_notes_to_self(),
      Argon2Settings::default(),
    )
    .await;
    match resumed {
      Ok(maybe_client) => Ok(maybe_client),
      // Kept for the next start; "Try again" opens the core again.
      Err(error) if error.is_transient() || error.is_sign_in_required() => Err(describe(error)),
      // Dropped if they can't work; the user links again.
      Err(error) => {
        tracing::warn!(%error, "could not finish the enrollment of an interrupted device link");
        Ok(None)
      },
    }
  }

  /// Deletes the unreadable database and opens a new one (issue #160). A browser can't keep the
  /// old one aside: its key is wrapped in the same database, and the space would stay used.
  async fn reset_store(&mut self) -> Result<(), String> {
    match self.status {
      CoreStatus::StoreUnreadable {
        problem: StoreProblem::NewerVersion,
      } => {
        // A newer bundle (after a reload, or in another tab) reads it: never wiped from here.
        return Err("the local data is from a newer version; reload the page instead".to_owned());
      },
      CoreStatus::StoreUnreadable { .. } => {},
      _ => return Err("the local data is readable; nothing to reset".to_owned()),
    }
    self.client = None;
    self.backend = None;
    // Dropping the last handle closes the database, so it can be deleted.
    self.store = None;
    let name = format!("teamcraft-{}", self.user);
    IndexedDbStore::delete(&name)
      .await
      .map_err(|error| error.to_string())?;
    tracing::warn!("deleted the unreadable local database");
    self.open().await;
    notify_changed();
    Ok(())
  }

  async fn has_existing_identity(&self) -> Result<bool, String> {
    match self.api().identity_key(self.user).await {
      Ok(maybe_key) => Ok(maybe_key.is_some()),
      Err(error) => Err(error.to_string()),
    }
  }

  /// Another device may have enrolled the account since the database was opened; the enrollment
  /// screen must then offer recovery and linking instead of a new identity. A page left on that
  /// screen also drops a setup given up on meanwhile.
  async fn refresh_enrollment_hint(&mut self) {
    if self.client.is_none()
      && self.link.is_none()
      && let CoreStatus::NeedsEnrollment { was_data_lost, .. } = self.status
    {
      self.drop_abandoned_setup().await;
      if let Ok(has_existing_identity) = self.has_existing_identity().await {
        self.status = CoreStatus::NeedsEnrollment {
          has_existing_identity,
          was_data_lost,
          can_restore_on_device: false,
        };
      }
    }
  }

  /// Drops the device key pair of a setup the user gave up on and revokes its registration (issue
  /// #415). Called only while this device is not set up and no link runs: enrolling runs to its end
  /// inside one request, so nothing else is in progress then. A failure (offline) is retried on the
  /// next start or status request.
  async fn drop_abandoned_setup(&self) {
    let Some(store) = &self.store else { return };
    if self.link.is_some() {
      return;
    }
    let settings = ClientSettings::default();
    if let Err(error) = PendingEnrollment::drop_abandoned(&self.api(), store, &SystemClock, &settings).await {
      tracing::info!(%error, "could not revoke the device of a setup that was given up yet");
    }
  }

  fn adopt(&mut self, client: Client<Api, Store>) -> CoreStatus {
    let status = CoreStatus::Ready {
      identity_fingerprint: client.identity_fingerprint(),
    };
    self.backend = Some(
      CoreChatBackend::new(client.store().clone(), client.user_id(), Box::new(SystemClock))
        .with_typing(client.typing_state())
        .with_transfers(client.transfers()),
    );
    self.catch_up_focus = client.catch_up_focus();
    let mut client = client;
    // Calls ring and show their roster in the tab that runs the core.
    client.set_call_event_sink(Box::new(crate::web_calls::on_gateway_event));
    self.client = Some(client);
    self.maybe_push_check_at_ms = Some(i64::MIN);
    store_string(&self.store_ready_key(), "1");
    status
  }

  fn store_ready_key(&self) -> String {
    format!("{STORE_READY_PREFIX}{}", self.user)
  }

  /// The server API; with the store open, every request that changes server state waits until
  /// the store has persisted what was written before it.
  fn api(&self) -> Api {
    let api = HttpServerApi::new(self.server_url.clone(), self.tokens.clone());
    match &self.store {
      Some(store) => {
        let store = Rc::downgrade(store.inner());
        api.with_write_barrier(Rc::new(move || {
          let maybe_store = store.upgrade();
          Box::pin(async move {
            let Some(store) = maybe_store else { return };
            // Not saved (quota, a newer version): the store health tells the user; the request
            // goes out anyway rather than blocking the app.
            if let Err(error) = store.flush().await {
              tracing::warn!(%error, "sending before the local store saved everything");
            }
          })
        }))
      },
      None => api,
    }
  }

  async fn handle(&mut self, request: Request) {
    match request {
      Request::Status(reply) => {
        self.refresh_enrollment_hint().await;
        drop(reply.send(self.status.clone()));
      },
      Request::StoreHealth(reply) => drop(reply.send(self.store_health.clone())),
      Request::Flush(reply) => {
        let outcome = match self.store.as_ref() {
          Some(store) => store.inner().flush().await.map_err(|error| error.to_string()),
          None => Ok(()),
        };
        drop(reply.send(outcome));
      },
      Request::CheckPush => self.maybe_push_check_at_ms = Some(i64::MIN),
      Request::Enroll(request, reply) => {
        let result = self.enroll(request).await;
        if result.is_ok() {
          notify_changed();
        }
        drop(reply.send(result));
      },
      Request::WipeGuestStore(reply) => drop(reply.send(self.wipe_guest_store().await)),
      Request::OwnKey(reply) => {
        let result = match self.client.as_ref() {
          Some(client) => Ok((client.own_fingerprint(), client.own_key_code())),
          None => Err(not_set_up()),
        };
        drop(reply.send(result));
      },
      Request::GuestAnchor {
        conversation,
        fingerprint,
        reply,
      } => {
        let result = match self.client.as_ref() {
          Some(client) => client
            .set_guest_organizer_anchor(conversation, &fingerprint)
            .map_err(describe),
          None => Err(not_set_up()),
        };
        drop(reply.send(result));
      },
      Request::LinkStart { device_name, reply } => drop(reply.send(self.link_start(device_name))),
      Request::LinkStatus(reply) => {
        let view = self
          .link
          .as_ref()
          .map_or(self.link_failure.clone(), |run| run.view.borrow().clone());
        drop(reply.send(view));
      },
      Request::EnablePush { vapid_key, reply } => {
        let result = self.enable_push(&vapid_key).await;
        drop(reply.send(result));
      },
      Request::LinkCancel => {
        self.link = None;
        self.link_failure = LinkStatus::Idle;
        // The user gave up on linking: keys from an earlier attempt aren't used any more.
        if self.client.is_none()
          && let Some(store) = &self.store
          && let Err(error) = records::delete_pending_link(store)
        {
          tracing::warn!(%error, "could not drop the keys of an abandoned device link");
        }
      },
      Request::ResetStore(reply) => drop(reply.send(self.reset_store().await)),
      Request::ForgetUnfinishedSetup(reply) => {
        if self.client.is_none()
          && let Some(store) = &self.store
        {
          self.link = None;
          // The keys go at once; the server gets a few seconds to revoke the device they registered
          // (issue #415), the next start of this account does it otherwise. Flushed: the core stops
          // right after, and queued writes would be lost with it.
          let forgotten =
            PendingEnrollment::forget_unfinished(&self.api(), store, &SystemClock, SIGN_OUT_REVOKE_PATIENCE)
              .await;
          let flushed = store.inner().flush().await.map_err(CoreError::from);
          if let Err(error) = forgotten.and(flushed) {
            tracing::warn!(%error, "could not drop the keys of an unfinished setup");
          }
        }
        // Deliberately ignored: the caller may have stopped waiting; nothing is left to tell.
        let _ = reply.send(());
      },
      Request::ApprovalStart { code, reply } => drop(reply.send(self.approval_start(&code).await)),
      Request::ApprovalConfirm { short_code, reply } => match self.approval_confirm(&short_code).await {
        Ok(receipt) => self.wait_for_receipt(receipt, reply),
        Err(message) => drop(reply.send(Err(message))),
      },
      Request::ApprovalCancel => {
        self.approval = None;
        self.maybe_phone_link = None;
        // A receipt still awaited: its wait ends, and the waiting call hears that it was cancelled.
        self.maybe_receipt = None;
      },
      Request::PhoneLinkStart(reply) => drop(reply.send(self.phone_link_start().await)),
      Request::PhoneLinkStatus(reply) => drop(reply.send(self.phone_link_status())),
      Request::LinkAnnounce(reply) => drop(reply.send(self.link_announce().await)),
      Request::LinkRequests(reply) => drop(reply.send(self.link_requests())),
      Request::LinkRequestReview { id, reply } => drop(reply.send(self.link_request_review(&id).await)),
      Request::LinkRequestReject { id, reply } => drop(reply.send(self.link_request_reject(&id).await)),
      Request::NetworkChanged => self.connection.network_changed(now_ms()).await,
      Request::PrepareCall(conversation, reply) => {
        let result = match self.client.as_mut() {
          Some(client) => client.prepare_call(conversation).await.map_err(describe_failure),
          None => Err(not_set_up()),
        };
        drop(reply.send(result));
      },
      Request::CallDevices(conversation, reply) => {
        let result = match self.client.as_mut() {
          Some(client) => client
            .refresh_call_devices(conversation)
            .await
            .map_err(describe_failure),
          None => Err(not_set_up()),
        };
        drop(reply.send(result));
      },
      Request::BackgroundPicture(effect, reply) => {
        let result = match (self.client.as_ref(), effect) {
          (Some(client), BackgroundEffect::Custom(id)) => client
            .background_picture(id)
            .await
            .map(|picture| picture.expose_secret().to_vec())
            .map_err(describe_failure),
          (Some(client), BackgroundEffect::Org(id)) => {
            client.org_background_picture(id).await.map_err(describe_failure)
          },
          (Some(_), _) => Err("this background has no picture".into()),
          (None, _) => Err(not_set_up()),
        };
        drop(reply.send(result));
      },
      // async-ok: the core's catch-up focus (a shared cell), not a signal.
      Request::Focus(conversation) => self.catch_up_focus.set(conversation),
      Request::Snapshot(reply) => drop(reply.send(self.snapshot())),
      Request::SnapshotChanges(maybe_since, reply) => drop(reply.send(self.snapshot_changes(maybe_since))),
      Request::Messages {
        conversation,
        start,
        len,
        reply,
      } => {
        // async-ok: the core's catch-up focus (a shared cell), not a signal.
        self.catch_up_focus.set(conversation);
        drop(reply.send(self.window(conversation, start, len)));
      },
      Request::Attachment {
        conversation,
        message,
        file_id,
        priority,
        reply,
      } => self.queue_attachment(conversation, message, &file_id, priority, reply),
      Request::Thread {
        conversation,
        thread,
        reply,
      } => {
        let result = self
          .backend
          .as_ref()
          .map(|backend| backend.thread(conversation, thread))
          .ok_or_else(not_set_up);
        drop(reply.send(result));
      },
      // A message shows at once: it is answered when it is in the outbox, and sent afterwards
      // while the UI reads it (`send_queued`).
      Request::Action(action, reply) if action.is_new_message() => {
        let result = match self.backend.as_ref() {
          Some(backend) => {
            queue_send(backend, *action);
            // async-ok: the core's own flag (a cell), not a signal.
            self.has_queued_sends.set(true);
            Ok(())
          },
          None => Err(not_set_up()),
        };
        notify_changed();
        drop(reply.send(result));
      },
      Request::Action(action, reply) => {
        // Typing changes nothing shown: it goes out at once, and the UI is not told of a change.
        if let Some((conversation, maybe_thread)) = action.typing_place() {
          self.announce_typing(conversation, maybe_thread).await;
          drop(reply.send(Ok(())));
          return;
        }
        let result = self.action(*action).await;
        notify_changed();
        drop(reply.send(result));
      },
      Request::Query(query, reply) => {
        let result = self
          .backend
          .as_ref()
          .map(|backend| query.answer(backend))
          .ok_or_else(not_set_up);
        drop(reply.send(result));
        // A read can ask for data it doesn't have yet (messages found only in shared index
        // segments, last seen, the call history): tell the UI to read again when it arrived.
        // A failed read command still changed what there is to read (it was answered as failed),
        // and its failure goes to the UI too, which nobody there waits for (issue #322).
        match self.run_queued_commands().await {
          Ok(0) => {},
          Ok(_) => notify_changed(),
          Err(error) => {
            notify_changed();
            crate::feedback::report_background_failure(error.to_failure());
          },
        }
      },
    }
  }

  async fn enroll(&mut self, request: EnrollRequest) -> Result<(), String> {
    if self.client.is_some() {
      return Ok(());
    }
    let secret = match request.secret {
      EnrollSecret::Guest => return self.enroll_guest(request.device_name).await,
      EnrollSecret::RecoveryKey { recovery_key } => {
        EnrollmentSecret::recovery_key(&recovery_key).map_err(describe_failure)?
      },
      EnrollSecret::Passphrase { passphrase } => EnrollmentSecret::passphrase(passphrase),
      EnrollSecret::KeptOnDevice => return Err("the browser keeps no key for the account".to_owned()),
    };
    let options = EnrollmentOptions {
      device_name: request.device_name,
      secret,
      argon2: Argon2Settings::default(),
      maybe_attestation: None,
    };
    self.complete_enrollment(options).await
  }

  /// A meeting guest's ephemeral device: no passphrase, no backup (e2ee.md §4.6, "Meeting guests").
  async fn enroll_guest(&mut self, device_name: String) -> Result<(), String> {
    let store = self.store.clone().ok_or("the local database is not open")?;
    tracing::info!("enrolling a guest device");
    let client = PendingEnrollment::complete_as_guest(
      self.api(),
      store,
      Box::new(SystemClock),
      ClientSettings::default(),
      device_name,
      // The key bag is wrapped with a random secret nobody keeps: the cheapest setting is plenty.
      Argon2Settings {
        memory_kib: 64,
        iterations: 1,
        parallelism: 1,
      },
    )
    .await
    .map_err(describe)?;
    self.status = self.adopt(client);
    store_string(GUEST_STORE_KEY, &self.user.to_string());
    Ok(())
  }

  /// Deletes the database of this guest device: its keys, messages and state. Refuses on any other
  /// device. The core stays closed afterwards.
  async fn wipe_guest_store(&mut self) -> Result<(), String> {
    let is_guest = self
      .client
      .as_ref()
      .is_some_and(|client| client.is_guest_device().unwrap_or(false));
    if !is_guest {
      return Err("this is not a guest device".to_owned());
    }
    self.client = None;
    self.backend = None;
    // Dropping the last handle closes the database, so it can be deleted.
    self.store = None;
    IndexedDbStore::delete(&format!("teamcraft-{}", self.user))
      .await
      .map_err(|error| error.to_string())?;
    store_string(GUEST_STORE_KEY, "");
    notify_changed();
    Ok(())
  }

  async fn complete_enrollment(&mut self, options: EnrollmentOptions) -> Result<(), String> {
    let store = self.store.clone().ok_or("the local database is not open")?;
    tracing::info!("enrolling this device");
    let client = PendingEnrollment::new()
      .complete(
        self.api(),
        store,
        Box::new(SystemClock),
        ClientSettings::default().with_notes_to_self(),
        options,
      )
      .await
      .map_err(describe_failure)?;
    tracing::info!("device enrolled");
    self.status = self.adopt(client);
    Ok(())
  }

  /// Subscribes this browser to Web Push and registers the subscription with a fresh push key. The
  /// service worker opens wake-ups with that key, also when no page is open.
  async fn enable_push(&mut self, vapid_key: &str) -> Result<(), String> {
    let client = self.client.as_mut().ok_or_else(not_set_up)?;
    client.register_web_push(vapid_key).await.map_err(describe)?;
    self.save_push_session();
    Ok(())
  }

  /// Lets the service worker fetch the message a wake-up names while no page is open: the server and
  /// the current access token, in the encrypted store (push_preview.rs). Written when the token changed.
  fn save_push_session(&mut self) {
    let (Some(client), Ok(token)) = (self.client.as_ref(), self.tokens.access_token()) else {
      return;
    };
    if self.last_session_token.as_deref() == Some(token.as_str()) {
      return;
    }
    let session = PushSession {
      server_url: self.server_url.clone(),
      access_token: token.clone(),
      expires_at_ms: now_ms().saturating_add(PUSH_SESSION_LIFETIME_MS),
    };
    match save_push_session(client.store(), &session) {
      Ok(()) => self.last_session_token = Some(token),
      Err(error) => tracing::debug!(%error, "could not store the push session"),
    }
  }

  /// New device: shows a code and waits for an existing device to scan it.
  fn link_start(&mut self, device_name: String) -> Result<(), String> {
    if self.client.is_some() {
      return Err("this device is already set up".to_owned());
    }
    let offer = LinkOffer::generate();
    let view = Rc::new(RefCell::new(LinkStatus::WaitingForScan {
      code: offer.to_code(),
      maybe_expires_at_ms: None,
    }));
    let store = self
      .store
      .clone()
      .ok_or_else(|| "the local database is not open".to_owned())?;
    let future = Box::pin(link_task(
      self.api(),
      store,
      device_name.clone(),
      offer,
      view.clone(),
    ));
    self.link_failure = LinkStatus::Idle;
    self.link = Some(LinkRun {
      device_name,
      view,
      future,
    });
    Ok(())
  }

  /// The existing device handed over the keys: enroll with them.
  async fn on_linked(&mut self, result: Result<LinkedSecrets, String>) {
    let Some(run) = self.link.take() else { return };
    let outcome = match result {
      Ok(secrets) => {
        let options = EnrollmentOptions {
          device_name: run.device_name,
          // Linked devices get the backup key from the existing device; no passphrase is involved.
          secret: EnrollmentSecret::Linked(secrets),
          argon2: Argon2Settings::default(),
          maybe_attestation: None,
        };
        self.complete_enrollment(options).await
      },
      Err(message) => Err(message),
    };
    self.link_failure = match outcome {
      Ok(()) => LinkStatus::Idle,
      Err(message) => LinkStatus::Failed { message },
    };
    notify_changed();
  }

  /// Existing device: joins the new device's link; the user compares the short code.
  async fn approval_start(&mut self, code: &str) -> Result<(), String> {
    let client = self.client.as_ref().ok_or_else(not_set_up)?;
    let approval = client.start_link_approval(code).await.map_err(describe_failure)?;
    self.approval = Some(approval);
    self.approval_misses = 0;
    Ok(())
  }

  /// Signed-in device: shows a code a new phone scans (*Link a phone*, issue #998). The wait runs
  /// beside the loop; once the phone answered, the user types the code it shows
  /// ([`Self::approval_confirm`]). A new start or a cancel closes the earlier code.
  async fn phone_link_start(&mut self) -> Result<(), String> {
    if self.client.is_none() {
      return Err(not_set_up());
    }
    self.maybe_phone_link = None;
    let offer = LinkOffer::generate_for_new_phone();
    let wait = LinkWait::join(&self.api(), &offer)
      .await
      .map_err(describe_failure)?;
    let maybe_expires_at_ms = expiry_at_ms(wait.maybe_expires_in());
    let (waiting, cancel) = wait.detach_approval();
    let scanned = Rc::new(RefCell::new(None));
    let slot = scanned.clone();
    wasm_bindgen_futures::spawn_local(async move {
      *slot.borrow_mut() = Some(waiting.await.map_err(describe_failure));
      notify_changed();
    });
    self.maybe_phone_link = Some(PhoneLinkRun {
      code: offer.to_code(),
      maybe_expires_at_ms,
      scanned,
      maybe_outcome: None,
      _cancel: cancel,
    });
    Ok(())
  }

  /// Where the code for a new phone stands; once scanned, the approval waits for the typed code.
  fn phone_link_status(&mut self) -> PhoneLinkStatus {
    let Some(run) = self.maybe_phone_link.as_mut() else {
      return PhoneLinkStatus::Idle;
    };
    if let Some(outcome) = &run.maybe_outcome {
      return outcome.clone();
    }
    let status = match run.scanned.borrow_mut().take() {
      None => {
        return PhoneLinkStatus::WaitingForScan {
          code: run.code.clone(),
          maybe_expires_at_ms: run.maybe_expires_at_ms,
        };
      },
      Some(Ok(approval)) => {
        self.approval = Some(approval);
        self.approval_misses = 0;
        PhoneLinkStatus::Scanned
      },
      Some(Err(message)) => PhoneLinkStatus::Failed { message },
    };
    run.maybe_outcome = Some(status.clone());
    status
  }

  /// New device: asks the other devices to approve this one. Hands the code's key to the server, so
  /// only when the user chooses it (e2ee.md §5).
  async fn link_announce(&mut self) -> Result<(), String> {
    let run = self.link.as_ref().ok_or("no device link in progress")?;
    let code = match &*run.view.borrow() {
      LinkStatus::WaitingForScan { code, .. } | LinkStatus::Confirm { code, .. } => code.clone(),
      _ => return Err("no device link in progress".to_owned()),
    };
    let announcement = LinkAnnouncement {
      device_name: run.device_name.clone(),
      platform: DeviceLinkPlatform::Web,
    };
    let offer = LinkOffer::parse(&code).map_err(describe_failure)?;
    offer.announce(&self.api(), &announcement).await.map_err(describe)
  }

  fn link_requests(&mut self) -> Vec<LinkRequestView> {
    let Some(client) = self.client.as_mut() else {
      return Vec::new();
    };
    client
      .link_requests()
      .into_iter()
      .map(|request| LinkRequestView {
        id: request.id,
        device_name: request.device_name,
        platform: request.platform,
        seconds_left: request.seconds_left,
      })
      .collect()
  }

  /// Existing device: takes up a request from a prompt; the typed-code check is the same as for a
  /// scanned code ([`Self::approval_confirm`]).
  async fn link_request_review(&mut self, id: &str) -> Result<(), String> {
    let client = self.client.as_mut().ok_or_else(not_set_up)?;
    let approval = client.review_link_request(id).await.map_err(describe_failure)?;
    self.approval = Some(approval);
    self.approval_misses = 0;
    notify_changed();
    Ok(())
  }

  async fn link_request_reject(&mut self, id: &str) -> Result<(), String> {
    let client = self.client.as_mut().ok_or_else(not_set_up)?;
    client.reject_link_request(id).await.map_err(describe_failure)?;
    notify_changed();
    Ok(())
  }

  /// Compares what the user typed with the short code of the handshake; only a match hands over the
  /// keys. A few wrong guesses end the attempt, so the code cannot be guessed by trial. The new
  /// device's receipt is still to come.
  async fn approval_confirm(
    &mut self,
    typed: &str,
  ) -> Result<LinkReceipt<<Api as ServerApi>::Rendezvous>, String> {
    let approval = self
      .approval
      .as_ref()
      .ok_or("there is no device waiting for approval")?;
    let digits = |text: &str| text.chars().filter(char::is_ascii_digit).collect::<String>();
    if digits(typed) != digits(&approval.short_code()) {
      self.approval_misses += 1;
      if self.approval_misses >= MAX_APPROVAL_MISSES {
        self.approval = None;
        return Err("the code did not match; start again on the new device".to_owned());
      }
      return Err("the code did not match the one on the new device".to_owned());
    }
    let approval = self
      .approval
      .take()
      .ok_or("there is no device waiting for approval")?;
    let client = self.client.as_mut().ok_or_else(not_set_up)?;
    client
      .send_link_transfer(approval)
      .await
      .map_err(describe_failure)
  }

  /// Waits for the new device's receipt on a task of its own (issue #415): it can take up to a minute
  /// when the new device went away, and the loop answers everything else meanwhile. `reply` gets
  /// the outcome, so the dialog shows its progress until then. One receipt is awaited at a time; a
  /// wait still running for an earlier link ends as cancelled.
  fn wait_for_receipt(
    &mut self,
    receipt: LinkReceipt<<Api as ServerApi>::Rendezvous>,
    reply: oneshot::Sender<Result<(), String>>,
  ) {
    let (waiting, cancel) = receipt.detach();
    self.maybe_receipt = Some(cancel);
    wasm_bindgen_futures::spawn_local(async move {
      let outcome = waiting.await.map_err(describe_failure);
      if let Err(message) = &outcome {
        tracing::info!(%message, "the device link was not confirmed");
      }
      // The dialog may have closed meanwhile; nobody is left to tell then.
      drop(reply.send(outcome));
    });
  }

  fn snapshot(&mut self) -> Result<ChatSnapshot, String> {
    let security = self.security_snapshot()?;
    self.maybe_last_security = Some(security.clone());
    let (Some(backend), Some(store)) = (self.backend.as_ref(), self.store.as_ref()) else {
      return Err(not_set_up());
    };
    crate::persisted_media::drop_renditions(store.take_removed_renditions());
    Ok(
      self
        .snapshots
        .borrow_mut()
        .snapshot(backend, store.take_changes(), || Some(security)),
    )
  }

  fn snapshot_changes(&mut self, maybe_since: Option<SnapshotCursor>) -> Result<SnapshotChanges, String> {
    let (Some(backend), Some(store)) = (self.backend.as_ref(), self.store.as_ref()) else {
      return Err(not_set_up());
    };
    let changes = store.take_changes();
    crate::persisted_media::drop_renditions(store.take_removed_renditions());
    let mut maybe_security = None;
    let answer = self.snapshots.borrow_mut().changes_since(
      backend,
      changes,
      || {
        maybe_security = self.security_snapshot().ok();
        maybe_security.clone()
      },
      maybe_since,
    );
    if maybe_security.is_some() {
      self.maybe_last_security = maybe_security;
    }
    Ok(answer)
  }

  /// Fingerprints of the keys this device pinned, and of keys the server showed since.
  fn security_snapshot(&self) -> Result<SecuritySnapshot, String> {
    let client = self.client.as_ref().ok_or_else(not_set_up)?;
    let fingerprint = |key: [u8; 32]| IdentityPublicKey::from_bytes(key).fingerprint().to_string();
    let contacts = records::pinned_identities(client.store())
      .map_err(describe)?
      .into_iter()
      .map(|(user, pinned)| ContactKeys {
        user,
        fingerprint: fingerprint(pinned.public_key),
        maybe_new_fingerprint: pinned.maybe_changed_to.map(fingerprint),
        is_attested: pinned.is_attested,
      })
      .collect();
    Ok(SecuritySnapshot {
      my_fingerprint: client.identity_fingerprint(),
      contacts,
      maybe_push_content: Some(teamcraft_core::push_preview::push_content_mode(client.store())),
      maybe_owners: client.owner_roster_status(),
      maybe_presence_policy: teamcraft_core::workspace::presence_policy(client.store()),
      maybe_screen_share_policy: teamcraft_core::workspace::screen_share_policy(client.store()),
      maybe_notetaker: teamcraft_core::notetaker::notetaker_policy(client.store()),
      maybe_backup: teamcraft_core::ui_snapshot::key_backup_state(client.store()).unwrap_or_else(|error| {
        // The settings then show the backup as unknown; the rest of the snapshot still counts.
        tracing::warn!(%error, "the key backup state is unreadable");
        None
      }),
    })
  }

  fn window(&self, conversation: ConversationId, start: usize, len: usize) -> Result<MessageWindow, String> {
    let backend = self.backend.as_ref().ok_or_else(not_set_up)?;
    Ok(MessageWindow::read(backend, conversation, start, len))
  }

  async fn action(&mut self, action: ChatAction) -> Result<(), String> {
    let client = self.client.as_mut().ok_or_else(not_set_up)?;
    // Structure changes are engine operations (they found or change encryption groups).
    match &action {
      ChatAction::CreateChannel { space, name } => {
        return client
          .create_channel(*space, name)
          .await
          .map(|_| ())
          .map_err(describe_failure);
      },
      ChatAction::OpenDm { user } => {
        return client.open_dm(*user).await.map(|_| ()).map_err(describe_failure);
      },
      ChatAction::OpenGroupDm { users } => {
        return client
          .open_group_dm(users)
          .await
          .map(|_| ())
          .map_err(describe_failure);
      },
      ChatAction::AddMember { conversation, user } => {
        return client
          .add_member(*conversation, *user)
          .await
          .map_err(describe_failure);
      },
      ChatAction::ChangePassphrase { passphrase } => {
        let passphrase = SecretBytes::new(passphrase.clone().into_bytes());
        return client
          .upload_key_bag_with(&passphrase, Argon2Settings::default())
          .await
          .map_err(describe_failure);
      },
      ChatAction::SetRecoveryKey { recovery_key } => {
        let recovery_key = parse_recovery_key(recovery_key).map_err(describe_failure)?;
        return client
          .set_recovery_key(&recovery_key)
          .await
          .map_err(describe_failure);
      },
      ChatAction::SetActivity { is_idle } => {
        self.send_activity(*is_idle).await;
        return Ok(());
      },
      ChatAction::SetAttention { is_attentive } => {
        let is_back = *is_attentive && self.maybe_attention != Some(true);
        self.send_attention(*is_attentive).await;
        if is_back {
          self.reconcile_calls().await;
        }
        return Ok(());
      },
      ChatAction::AcceptKeyChange { user } => {
        let mut pinned = records::pinned_identities(client.store()).map_err(describe_failure)?;
        if let Some(entry) = pinned.get_mut(user)
          && let Some(new_key) = entry.maybe_changed_to.take()
        {
          entry.public_key = new_key;
          records::pin_identity(client.store(), *user, entry).map_err(describe_failure)?;
        }
        return Ok(());
      },
      ChatAction::PublishPolicy {
        base,
        policy,
        owners,
        admins,
      } => {
        return client
          .publish_policy_with_genesis(base, policy.clone(), owners, admins)
          .await
          .map_err(describe_failure);
      },
      ChatAction::PublishRoster {
        base_version,
        members,
      } => {
        let roles = teamcraft_core::workspace::roster_roles(members);
        return client
          .publish_roster_based_on(*base_version, &roles)
          .await
          .map_err(describe_failure);
      },
      ChatAction::AdminJoinChannel { conversation } => {
        let me = client.user_id();
        return client
          .admin_add_member(*conversation, me)
          .await
          .map_err(describe_failure);
      },
      ChatAction::AdminRemoveFromAllChats { user } => {
        return client
          .admin_remove_from_all_chats(*user)
          .await
          .map_err(describe_failure);
      },
      ChatAction::AdminDeleteUser { user } => {
        return client.admin_delete_user(*user).await.map_err(describe_failure);
      },
      ChatAction::AdminRemoveGroupLeftover { conversation, user } => {
        return client
          .admin_remove_group_leftover(*conversation, *user)
          .await
          .map_err(describe_failure);
      },
      _ => {},
    }
    let backend = self.backend.as_ref().ok_or_else(not_set_up)?;
    match action {
      ChatAction::PostMessage {
        conversation,
        maybe_thread,
        body,
      } => backend.post_message(conversation, maybe_thread, &body),
      ChatAction::ToggleReaction {
        conversation,
        message,
        emoji,
      } => backend.toggle_reaction(conversation, message, &emoji),
      ChatAction::MarkRead { conversation } => backend.mark_read(conversation),
      ChatAction::SetFavorite {
        conversation,
        is_favorite,
      } => backend.set_favorite(conversation, is_favorite),
      ChatAction::SetPresence {
        presence,
        maybe_until,
      } => backend.set_presence_until(presence, maybe_until),
      ChatAction::SetCustomStatus { maybe_status } => backend.set_custom_status(maybe_status),
      ChatAction::CreateChannel { .. }
      | ChatAction::OpenDm { .. }
      | ChatAction::OpenGroupDm { .. }
      | ChatAction::AddMember { .. }
      | ChatAction::ChangePassphrase { .. }
      | ChatAction::SetRecoveryKey { .. }
      | ChatAction::SetActivity { .. }
      | ChatAction::SetAttention { .. }
      | ChatAction::AcceptKeyChange { .. }
      | ChatAction::PublishPolicy { .. }
      | ChatAction::PublishRoster { .. }
      | ChatAction::AdminJoinChannel { .. }
      | ChatAction::AdminRemoveFromAllChats { .. }
      | ChatAction::AdminDeleteUser { .. }
      | ChatAction::AdminRemoveGroupLeftover { .. } => {},
      // Message actions, files and workspace management go to the backend, which queues what
      // needs the engine as commands.
      other => {
        if let Some(unhandled) = other.apply_to(backend) {
          tracing::warn!(?unhandled, "chat action not handled");
        }
      },
    }
    let ran = self.run_queued_commands().await;
    // New messages go out right away; failures stay in the outbox for the periodic retry.
    self.flush().await;
    ran.map(|_| ()).map_err(describe_failure)
  }

  /// Runs what the UI adapter queued; returns how many commands there were, or the first failure.
  /// Every command runs even when one fails: they are independent user actions. The failure goes
  /// back to the UI, which shows it (issue #322: a failed action never passes silently).
  async fn run_queued_commands(&mut self) -> Result<usize, CoreError> {
    let commands = self.drain_commands();
    let Some(client) = self.client.as_mut() else {
      return Ok(0);
    };
    let count = commands.len();
    let mut maybe_failure = None;
    for command in commands {
      // A command can take long (a big history page); heartbeats go on meanwhile.
      let result = self
        .connection
        .keep_alive(now_ms(), client.run_ui_command(command))
        .await;
      if let Err(error) = result {
        tracing::warn!(%error, "UI command failed");
        maybe_failure.get_or_insert(error);
      }
    }
    maybe_failure.map_or(Ok(count), Err)
  }

  /// What the UI adapter queued, without the uploads: those run beside the loop
  /// ([`Self::start_upload`]).
  fn drain_commands(&mut self) -> Vec<CoreCommand> {
    let commands = self
      .backend
      .as_ref()
      .map(CoreChatBackend::drain_commands)
      .unwrap_or_default();
    let (uploads, commands): (Vec<_>, Vec<_>) = commands
      .into_iter()
      .partition(|command| matches!(command, CoreCommand::UploadFiles));
    self.is_upload_due |= !uploads.is_empty();
    commands
      .into_iter()
      .filter(|command| match command {
        // Saved beside the loop, like uploads.
        CoreCommand::DownloadFile {
          conversation,
          attachment,
        } => {
          self.downloads.save(*conversation, *attachment);
          false
        },
        CoreCommand::ControlDownload { attachment, control } => {
          self.downloads.control(*attachment, *control);
          false
        },
        _ => true,
      })
      .collect()
  }

  async fn flush(&mut self) {
    let Some(client) = self.client.as_mut() else {
      return;
    };
    if self
      .connection
      .keep_alive(now_ms(), upkeep::flush_sends(client))
      .await
    {
      notify_changed();
    }
  }

  /// Sends what [`Request::Action`] put into the outbox (link previews first), while reads are
  /// answered. A failure was not the action's (that was answered already): it is shown by itself.
  async fn send_queued(&mut self, requests: &mut mpsc::UnboundedReceiver<Request>) {
    let commands = self.drain_commands();
    let outcome = self
      .serve_during(requests, async |client: &mut Client<Api, Store>| {
        let mut maybe_failure = None;
        for command in commands {
          if let Err(error) = client.run_ui_command(command).await {
            maybe_failure.get_or_insert(error);
          }
        }
        upkeep::flush_sends(client).await;
        maybe_failure
      })
      .await;
    if let Some(Some(error)) = outcome {
      tracing::warn!(%error, "sending a queued message failed");
      crate::feedback::report_background_failure(error.to_failure());
    }
    notify_changed();
  }

  /// Starts the next due upload, if none runs: the file is encrypted and announced, then goes up
  /// beside the loop.
  async fn start_upload(&mut self, requests: &mut mpsc::UnboundedReceiver<Request>) {
    if !self.is_upload_due {
      return;
    }
    // A cancelled file (or a message deleted meanwhile) stops its upload: dropping it aborts it.
    if let (Some(upload), Some(client)) = (&self.maybe_upload, &self.client)
      && !client.is_upload_current(upload)
    {
      self.maybe_upload = None;
      notify_changed();
    }
    if self.maybe_upload.is_some() || self.client.is_none() {
      return;
    }
    self.is_upload_due = false;
    let started = self
      .serve_during(requests, async |client: &mut Client<Api, Store>| {
        client.start_upload().await
      })
      .await;
    match started {
      Some(Ok(Some(upload))) => {
        self.maybe_upload = Some(upload);
        // The pending message shows the file's thumbnail from now on.
        notify_changed();
      },
      Some(Ok(None)) | None => {},
      Some(Err(error)) => tracing::debug!(%error, "upload not started"),
    }
  }

  /// The running upload's blob is stored (or failed): the file goes into its message, which is sent
  /// once all its files are up.
  async fn on_uploaded(
    &mut self,
    sent: Result<(), ApiError>,
    requests: &mut mpsc::UnboundedReceiver<Request>,
  ) {
    let Some(upload) = self.maybe_upload.take() else {
      return;
    };
    let finished = self
      .serve_during(requests, async |client: &mut Client<Api, Store>| {
        let step = client.finish_upload(upload, sent).await;
        upkeep::flush_sends(client).await;
        step
      })
      .await;
    if let Some(Err(error)) = finished {
      tracing::debug!(%error, "upload not finished");
    }
    self.is_upload_due = true;
    notify_changed();
  }

  /// Queues one file, decrypted, for the image viewer or an inline picture; it downloads beside
  /// the loop and is answered when it arrived.
  fn queue_attachment(
    &mut self,
    conversation: ConversationId,
    message: MessageId,
    file_id: &str,
    priority: AttachmentPriority,
    reply: oneshot::Sender<Result<SecretBytes, String>>,
  ) {
    let maybe_reference = self
      .backend
      .as_ref()
      .and_then(|backend| backend.attachment_ref(conversation, message, file_id));
    let Some(reference) = maybe_reference else {
      drop(reply.send(Err("no such file in this conversation".to_owned())));
      return;
    };
    // A closed reply channel (the caller gave up) needs nothing.
    // The caller dropping its end (row scrolled away, viewer closed) cancels a file still waiting.
    let reply = Rc::new(RefCell::new(Some(reply)));
    let asker = reply.clone();
    let deliver = move |opened: Result<SecretBytes, String>| {
      if let Some(reply) = reply.borrow_mut().take() {
        drop(reply.send(opened));
      }
    };
    let is_wanted = move || asker.borrow().as_ref().is_some_and(|reply| !reply.is_canceled());
    self
      .downloads
      .view_while(reference, priority.into(), Box::new(deliver), Box::new(is_wanted));
  }

  /// Starts the downloads that may run now.
  async fn start_downloads(&mut self) {
    let Some(client) = self.client.as_mut() else {
      return;
    };
    if self
      .connection
      .keep_alive(now_ms(), self.downloads.start_due(client))
      .await
    {
      notify_changed();
    }
  }

  /// Tells the UI to read again when the transfers in flight moved on, at most every
  /// [`TRANSFER_PROGRESS`] (the loop wakes up for it while there are any).
  fn report_progress(&mut self) {
    let Some(client) = self.client.as_ref() else {
      return;
    };
    let moved = client.transfers().moved_bytes();
    if moved != self.maybe_reported_bytes {
      self.maybe_reported_bytes = moved;
      notify_local();
    }
  }

  /// Runs `work` with the client while the UI's reads (and new messages) are answered from the
  /// store, as during a connect. `None` without a client.
  async fn serve_during<T>(
    &mut self,
    requests: &mut mpsc::UnboundedReceiver<Request>,
    work: impl AsyncFnOnce(&mut Client<Api, Store>) -> T,
  ) -> Option<T> {
    let Self {
      client: Some(client),
      store: Some(store),
      backend: Some(backend),
      connection,
      status,
      store_health,
      maybe_last_security,
      snapshots,
      catch_up_focus,
      deferred,
      is_closed,
      has_queued_sends,
      ..
    } = self
    else {
      return None;
    };
    let reads = SyncReads {
      backend,
      store,
      stored: Cell::new(StoredWorkspace::of(backend)),
      status,
      store_health,
      maybe_security: maybe_last_security.as_ref(),
      snapshots,
      focus: catch_up_focus,
      has_queued_sends,
      transfers: client.transfers(),
    };
    let working = pin!(connection.keep_alive(now_ms(), work(client)));
    Some(reads.serve(working, requests, deferred, is_closed).await)
  }

  /// Connects when due. A fresh session's full sync can take minutes on a workspace that grew
  /// meanwhile (issue #247): until it is done, the UI's reads are answered from the local store,
  /// which the sync fills as it goes (`sync_reads`), and requests that need the client wait for
  /// the end of the connect.
  async fn connect(&mut self, requests: &mut mpsc::UnboundedReceiver<Request>) {
    if self.maybe_last_security.is_none() && self.client.is_some() {
      self.maybe_last_security = self.security_snapshot().ok();
    }
    let Self {
      client: Some(client),
      store: Some(store),
      backend: Some(backend),
      connection,
      status,
      store_health,
      maybe_last_security,
      snapshots,
      catch_up_focus,
      deferred,
      is_closed,
      has_queued_sends,
      ..
    } = self
    else {
      return;
    };
    let outcome = {
      let transfers = client.transfers();
      let mut connecting = pin!(connection.connect_if_due(client));
      // No attempt due (the usual case): done without a timer or a look at the store.
      match poll_immediate(connecting.as_mut()).await {
        Some(outcome) => outcome,
        None => {
          let reads = SyncReads {
            backend,
            store,
            stored: Cell::new(StoredWorkspace::of(backend)),
            status,
            store_health,
            maybe_security: maybe_last_security.as_ref(),
            snapshots,
            focus: catch_up_focus,
            has_queued_sends,
            transfers,
          };
          reads
            .serve(connecting.as_mut(), requests, deferred, is_closed)
            .await
        },
      }
    };
    if outcome.is_some() {
      self.repeat_reports().await;
      // Back online: what waited goes out now, not on the next tick.
      // async-ok: the core's own flag (a cell), not a signal.
      self.has_queued_sends.set(true);
      self.is_upload_due = true;
      notify_changed();
    }
  }

  async fn on_frame(&mut self, frame: Option<Result<GatewayServerMessage, ApiError>>) {
    let Some(client) = self.client.as_mut() else {
      return;
    };
    if self.connection.on_frame(client, frame).await {
      notify_changed();
    }
  }

  async fn on_tick(&mut self) {
    let now = now_ms();
    if self.client.is_some() {
      self.connection.on_tick(now).await;
    }
    // The UI reads who types again once an indicator ran out.
    if self.client.as_ref().is_some_and(Client::drop_expired_typing) {
      notify_changed();
    }
    if now < self.next_tick_ms {
      // Woken for the connection only.
      return;
    }
    self.next_tick_ms = now.saturating_add(millis(TICK));
    // Retries of failed uploads come due with time.
    self.is_upload_due = true;
    self.save_push_session();
    let Some(client) = self.client.as_mut() else {
      return;
    };
    // The upkeep may resync many conversations; heartbeats go on meanwhile.
    if self.connection.keep_alive(now, self.upkeep.tick(client)).await {
      notify_changed();
    }
  }

  /// Tells the other members that the user types (throttled; dropped while offline).
  async fn announce_typing(&mut self, conversation: ConversationId, maybe_thread: Option<ThreadId>) {
    if self.client.is_some() {
      self
        .typing
        .announce(&mut self.connection, conversation, maybe_thread, now_ms())
        .await;
    }
  }

  /// Tells the server whether this device is idle (docs/gateway.md); a failure ends the connection
  /// like a failed heartbeat. Remembered so a new connection repeats it.
  async fn send_activity(&mut self, is_idle: bool) {
    self.is_idle = is_idle;
    let activity = if is_idle {
      DeviceActivity::Idle
    } else {
      DeviceActivity::Active
    };
    if let Some(client) = self.client.as_mut() {
      client.set_device_activity(activity.clone());
    }
    let message = GatewayClientMessage::SetActivity { activity };
    if self.connection.is_connected() && !self.connection.send(now_ms(), &message).await {
      tracing::warn!("could not report the device activity");
    }
  }

  /// Tells the server whether the user is looking at this session (notifications.md, "Attention").
  /// Remembered so a new connection repeats it.
  async fn send_attention(&mut self, is_attentive: bool) {
    self.maybe_attention = Some(is_attentive);
    let message = GatewayClientMessage::Attention { is_attentive };
    if self.connection.is_connected() && !self.connection.send(now_ms(), &message).await {
      tracing::warn!("could not report the attention");
    }
  }

  /// The user came back to the app: its call bars are checked against the server's running calls
  /// (issue #976), in case a call event got lost while they were away. Only while connected: a new
  /// connection checks anyway.
  async fn reconcile_calls(&mut self) {
    if !self.connection.is_connected() {
      return;
    }
    if let Some(client) = self.client.as_mut()
      && let Err(error) = client.reconcile_calls().await
    {
      tracing::warn!(%error, "could not read the running calls");
    }
  }

  /// What a new connection has to hear again: the device's activity and attention.
  async fn repeat_reports(&mut self) {
    self.send_activity(self.is_idle).await;
    if let Some(is_attentive) = self.maybe_attention {
      self.send_attention(is_attentive).await;
    }
  }
}

fn now_ms() -> i64 {
  SystemClock.now_ms()
}

fn millis(duration: Duration) -> i64 {
  i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

/// What the core can answer while a connect holds the client: reads of the local store.
struct SyncReads<'a> {
  backend: &'a CoreChatBackend<Store>,
  store: &'a Store,
  /// Becomes [`StoredWorkspace::Known`] as soon as the sync has stored the conversation list.
  stored: Cell<StoredWorkspace>,
  status: &'a CoreStatus,
  store_health: &'a StoreHealth,
  maybe_security: Option<&'a SecuritySnapshot>,
  snapshots: &'a RefCell<SnapshotCache>,
  focus: &'a CatchUpFocus,
  /// Set when a new message went into the outbox; the loop sends it once the client is free.
  has_queued_sends: &'a Cell<bool>,
  transfers: Transfers,
}

impl SyncReads<'_> {
  /// Answers reads until the connect is done; the rest is queued in `deferred`. Every
  /// [`SYNC_PROGRESS`], this tab's UI is told to read again if the sync wrote anything and the page
  /// is visible (follower tabs hear about it when the connect is done).
  async fn serve<F: Future + Unpin>(
    &self,
    mut connecting: F,
    requests: &mut mpsc::UnboundedReceiver<Request>,
    deferred: &mut VecDeque<Request>,
    is_closed: &mut bool,
  ) -> F::Output {
    let mut seen_revision = self.store.inner().write_revision();
    let mut seen_caught_up = self.focus.caught_up_count();
    let mut seen_moved = self.transfers.moved_bytes();
    // Deadlines, not a fresh sleep per wake-up: a burst of reads must not postpone the progress.
    let mut progress = Box::pin(platform::sleep(SYNC_PROGRESS));
    loop {
      let request = if *is_closed {
        Either::Left(pending())
      } else {
        Either::Right(requests.next())
      };
      // The conversation the user opened is shown as soon as the sync woke this for it.
      let focus_caught_up = pin!(self.focus.caught_up_since(seen_caught_up));
      let wakes = select(progress.as_mut(), focus_caught_up);
      match select(&mut connecting, select(request, wakes)).await {
        Either::Left((outcome, _)) => return outcome,
        Either::Right((Either::Left((Some(request), _)), _)) => {
          if let Some(request) = self.answer(request) {
            deferred.push_back(request);
          }
        },
        Either::Right((Either::Left((None, _)), _)) => *is_closed = true,
        Either::Right((Either::Right((Either::Left(((), _)), _)), _)) => {
          progress = Box::pin(platform::sleep(SYNC_PROGRESS));
          let revision = self.store.inner().write_revision();
          if revision != seen_revision && self.has_become_known(deferred) {
            notify_local();
          }
          // A download or upload in flight: its progress.
          let moved = self.transfers.moved_bytes();
          if moved.is_some() && moved != seen_moved && is_page_visible() {
            seen_moved = moved;
            notify_local();
          }
          if revision != seen_revision && is_page_visible() {
            seen_revision = revision;
            notify_local();
          }
        },
        Either::Right((Either::Right((Either::Right((caught_up, _)), _)), _)) => {
          seen_caught_up = caught_up;
          if is_page_visible() {
            seen_revision = self.store.inner().write_revision();
            notify_local();
          }
        },
      }
    }
  }

  /// A first sync stores the conversation list before it catches each one up (issue #247): from
  /// then on the workspace is shown, each conversation marked while it is caught up, and the
  /// requests that waited for it are answered. Returns whether that happened now.
  fn has_become_known(&self, deferred: &mut VecDeque<Request>) -> bool {
    if self.stored.get() == StoredWorkspace::Known
      || StoredWorkspace::of(self.backend) == StoredWorkspace::Empty
    {
      return false;
    }
    self.stored.set(StoredWorkspace::Known);
    for request in std::mem::take(deferred) {
      if let Some(request) = self.answer(request) {
        deferred.push_back(request);
      }
    }
    true
  }

  /// Answers a read; hands back a request that has to wait.
  fn answer(&self, request: Request) -> Option<Request> {
    let request = match request {
      Request::Snapshot(_) | Request::SnapshotChanges(..) => return self.answer_snapshot(request),
      // Never held back by a sync or an upload: the message shows at once and goes out after.
      Request::Action(action, reply) if action.is_new_message() => {
        queue_send(self.backend, *action);
        self.has_queued_sends.set(true);
        drop(reply.send(Ok(())));
        notify_changed();
        return None;
      },
      // Typing arriving during a sync would go out late: dropped.
      Request::Action(action, reply) if action.typing_place().is_some() => {
        drop(reply.send(Ok(())));
        return None;
      },
      Request::Focus(conversation) => {
        // The sync holding the client takes it between two steps.
        self.focus.set(conversation);
        return None;
      },
      other => other,
    };
    let read = match &request {
      Request::Status(_) if self.stored.get() == StoredWorkspace::Empty => {
        // Ready with an empty store would show an empty workspace: the first sync is awaited.
        return Some(request);
      },
      Request::Status(_) | Request::StoreHealth(_) => None,
      Request::Messages {
        conversation,
        start,
        len,
        ..
      } => {
        // The conversation the user opened is caught up next.
        self.focus.set(*conversation);
        Some(SyncRead::Messages {
          conversation: *conversation,
          start: *start,
          len: *len,
        })
      },
      Request::Thread {
        conversation, thread, ..
      } => Some(SyncRead::Thread {
        conversation: *conversation,
        thread: *thread,
      }),
      // Commands a query queues run with the next query after the connect.
      Request::Query(query, _) => Some(SyncRead::Query(query.clone())),
      _ => return Some(request),
    };
    let maybe_answer = match read {
      Some(read) => match answer_during_sync(read, self.backend, self.stored.get(), self.maybe_security) {
        Some(answer) => Some(answer),
        None => return Some(request),
      },
      None => None,
    };
    // A closed reply channel (the caller gave up) needs nothing.
    match (request, maybe_answer) {
      (Request::Status(reply), None) => drop(reply.send(self.status.clone())),
      (Request::StoreHealth(reply), None) => drop(reply.send(self.store_health.clone())),
      (Request::Messages { reply, .. }, Some(SyncReadAnswer::Messages(window))) => {
        drop(reply.send(Ok(window)))
      },
      (Request::Thread { reply, .. }, Some(SyncReadAnswer::Thread(messages))) => {
        drop(reply.send(Ok(messages)))
      },
      (Request::Query(_, reply), Some(SyncReadAnswer::Query(answer))) => drop(reply.send(Ok(answer))),
      // Invariant: `answer_during_sync` answers with the variant of the read. Should it not, the
      // request waits for the sync like any other.
      (request, _) => return Some(request),
    }
    None
  }
}

impl SyncReads<'_> {
  /// The snapshot from the cache, as far as the sync has filled the store. Waits while the
  /// workspace was empty or no security part was taken yet (it needs the client).
  fn answer_snapshot(&self, request: Request) -> Option<Request> {
    let (StoredWorkspace::Known, Some(security)) = (self.stored.get(), self.maybe_security) else {
      return Some(request);
    };
    let changes = self.store.take_changes();
    let mut snapshots = self.snapshots.borrow_mut();
    let security = || Some(security.clone());
    match request {
      Request::Snapshot(reply) => drop(reply.send(Ok(snapshots.snapshot(self.backend, changes, security)))),
      Request::SnapshotChanges(maybe_since, reply) => {
        let answer = snapshots.changes_since(self.backend, changes, security, maybe_since);
        drop(reply.send(Ok(answer)));
      },
      other => return Some(other),
    }
    None
  }
}

/// Writes a new message ([`ChatAction::is_new_message`]) into the outbox; the loop sends it.
fn queue_send(backend: &CoreChatBackend<Store>, action: ChatAction) {
  if let Some(unhandled) = action.apply_to(backend) {
    tracing::warn!(?unhandled, "chat action not handled");
  }
}

fn is_page_visible() -> bool {
  web_sys::window()
    .and_then(|window| window.document())
    .is_none_or(|document| document.visibility_state() == web_sys::VisibilityState::Visible)
}

fn not_set_up() -> String {
  "this device is not set up yet".to_owned()
}

/// The status for an error of opening the store or the client: unreadable data asks the user what
/// to do (issue #160); anything else is a failure.
fn unreadable_status(error: CoreError) -> Result<CoreStatus, String> {
  match error.unreadable_store() {
    Some(problem) => {
      tracing::error!(%error, ?problem, "the local database is unreadable");
      Ok(CoreStatus::StoreUnreadable {
        problem: match problem {
          UnreadableStore::Damaged => StoreProblem::Damaged,
          UnreadableStore::NewerVersion => StoreProblem::NewerVersion,
        },
      })
    },
    None => Err(error.to_string()),
  }
}

/// Error text for the UI: the cause, without internal type names.
fn describe(error: CoreError) -> String {
  error.to_string()
}

/// Error text of a failed user action: its kind first, so the UI words it (`ActionFailure`).
fn describe_failure(error: CoreError) -> String {
  error.to_failure().to_wire()
}

/// Waits for the existing device to answer, shows the short code, then waits for the keys.
async fn link_task(
  api: Api,
  store: Store,
  device_name: String,
  offer: LinkOffer,
  view: Rc<RefCell<LinkStatus>>,
) -> Result<LinkedSecrets, String> {
  let wait = LinkWait::join(&api, &offer).await.map_err(describe_failure)?;
  *view.borrow_mut() = LinkStatus::WaitingForScan {
    code: offer.to_code(),
    maybe_expires_at_ms: expiry_at_ms(wait.maybe_expires_in()),
  };
  notify_changed();
  let link = wait.new_device(&api).await.map_err(describe_failure)?;
  *view.borrow_mut() = LinkStatus::Confirm {
    code: offer.to_code(),
    short_code: link.short_code(),
  };
  notify_changed();
  link
    .receive_secrets(&store, &device_name)
    .await
    .map_err(describe_failure)
}

/// When a code that stays valid for `maybe_left` stops working, on this machine's clock.
fn expiry_at_ms(maybe_left: Option<std::time::Duration>) -> Option<i64> {
  maybe_left.map(|left| now_ms().saturating_add(i64::try_from(left.as_millis()).unwrap_or(i64::MAX)))
}

/// Issues the store's queued writes at once when the page is hidden, frozen or unloaded: a tab in
/// the background may be frozen or discarded without another chance to run (robustness.md,
/// Lifecycle). One listener per page, for the store opened last.
fn watch_page_lifecycle(store: &Rc<IndexedDbStore>) {
  LIFECYCLE_STORE.with(|current| *current.borrow_mut() = Rc::downgrade(store));
  if IS_WATCHING_LIFECYCLE.with(|is_watching| is_watching.replace(true)) {
    return;
  }
  let Some(document) = web_sys::window().and_then(|window| window.document()) else {
    return;
  };
  let listener = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::Event)>::new(|event: web_sys::Event| {
    if event.type_() == "visibilitychange" && is_page_visible() {
      return;
    }
    // async-ok: a thread-local weak reference to the store, not a signal.
    if let Some(store) = LIFECYCLE_STORE.with(|current| current.borrow().upgrade()) {
      store.write_now();
    }
  });
  let mut is_listening = false;
  for kind in ["visibilitychange", "freeze", "pagehide"] {
    // `pagehide` fires on the window; the others on the document.
    let added = if kind == "pagehide" {
      web_sys::window()
        .map(|window| window.add_event_listener_with_callback(kind, listener.as_ref().unchecked_ref()))
    } else {
      Some(document.add_event_listener_with_callback(kind, listener.as_ref().unchecked_ref()))
    };
    is_listening |= matches!(added, Some(Ok(())));
  }
  if is_listening {
    // Lives as long as the page.
    listener.forget();
  }
}

/// Passes the service worker's "subscription changed" on to the core (once per page).
fn watch_worker() {
  if IS_WATCHING_WORKER.with(|is_watching| is_watching.replace(true)) {
    return;
  }
  let Some(window) = web_sys::window() else { return };
  let listener = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::MessageEvent)>::new(
    |event: web_sys::MessageEvent| {
      let kind = js_sys::Reflect::get(&event.data(), &"type".into())
        .ok()
        .and_then(|kind| kind.as_string());
      // Follower tabs get the message too; only the owner has a core to tell.
      if kind.as_deref() == Some(SUBSCRIPTION_CHANGED) && current_follower().is_none() {
        drop(send(Request::CheckPush));
      }
    },
  );
  if window
    .navigator()
    .service_worker()
    .add_event_listener_with_callback("message", listener.as_ref().unchecked_ref())
    .is_ok()
  {
    // Lives as long as the page.
    listener.forget();
  }
}

/// Asks the browser not to delete the store under storage pressure (e2ee.md §9.4). Browsers decide
/// on their own (Chrome grants it to installed or often used sites, Firefox asks the user); without
/// it the store can be evicted, and the enrollment screen says so.
fn request_persistent_storage() {
  let Some(window) = web_sys::window() else { return };
  let Ok(promise) = window.navigator().storage().persist() else {
    return;
  };
  wasm_bindgen_futures::spawn_local(async move {
    match wasm_bindgen_futures::JsFuture::from(promise).await {
      Ok(granted) => tracing::info!(is_persistent = granted.is_truthy(), "persistent storage"),
      Err(error) => tracing::debug!(?error, "persistent storage not available"),
    }
  });
}

/// The notification texts the service worker shows in `locale` (issue #644): it has no translations
/// of its own, and no page may be open when a push arrives.
fn notification_labels(locale: crate::i18n::Locale) -> NotificationLabels {
  let translate = |key: &str| Some(crate::i18n::translate(locale, key, None));
  let mut name = FluentArgs::new();
  name.set("name", NAME_PLACEHOLDER);
  let with_name = |key: &str| Some(crate::i18n::translate(locale, key, Some(&name)));
  NotificationLabels {
    maybe_new_message: translate("notify-new-message"),
    maybe_new_message_from: with_name("notify-new-message-from"),
    maybe_photo: translate("notify-photo"),
    maybe_video: translate("notify-video"),
    maybe_audio: translate("notify-audio"),
    maybe_call_title: translate("incoming-title"),
    maybe_call_from: with_name("notify-call-from"),
    maybe_call_unknown: translate("notify-call-unknown"),
    maybe_test_body: translate("notify-test-body"),
    maybe_mark_read: translate("notify-action-mark-read"),
  }
}

/// Tells the service worker which database holds this account's keys, so it can open pushes, and
/// the notification texts in the user's language. It cannot ask the page later (no page may be
/// open), so it keeps both in its own cache.
fn announce_database(name: &str) {
  let name = name.to_owned();
  wasm_bindgen_futures::spawn_local(async move {
    let Some(window) = web_sys::window() else { return };
    let Ok(ready) = window.navigator().service_worker().ready() else {
      return;
    };
    let Ok(registration) = wasm_bindgen_futures::JsFuture::from(ready).await else {
      return;
    };
    let registration: web_sys::ServiceWorkerRegistration = registration.unchecked_into();
    let Some(worker) = registration.active() else {
      return;
    };
    let message = js_sys::Object::new();
    let set = |key: &str, value: &str| js_sys::Reflect::set(&message, &key.into(), &value.into());
    let labels = match serde_json::to_string(&notification_labels(crate::settings::stored_locale())) {
      Ok(labels) => labels,
      Err(error) => {
        // The worker then shows English texts; nothing the user must act on.
        tracing::warn!(%error, "notification texts not serialized");
        String::new()
      },
    };
    if set("type", "push_database")
      .and_then(|_| set("name", &name))
      .and_then(|_| set("labels", &labels))
      .is_ok()
    {
      drop(worker.post_message(&message));
    }
  });
}
