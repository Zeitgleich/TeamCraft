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
//! Only one tab per account runs the core (`web_tabs`, e2ee.md §9.4); the others pass their calls
//! to it.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::future::Future;
use std::pin::{Pin, pin};
use std::rc::Rc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use futures_channel::{mpsc, oneshot};
use futures_util::future::{Either, pending, poll_immediate, select};
use futures_util::{FutureExt, StreamExt};
use serde::de::DeserializeOwned;
use serde_json::Value;
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::model::{Message, ThreadId};
use teamcraft_client_api::remote::{
  ChatAction, ChatAnswer, ChatQuery, ChatSnapshot, ContactKeys, CoreConnection, CoreStatus, EnrollRequest,
  LinkStatus, MessageWindow, SecuritySnapshot, StoreHealth, StoreProblem,
};
use teamcraft_client_api::snapshot_changes::{SnapshotChanges, SnapshotCursor};
use teamcraft_core::api::{AccessTokenSource, ApiError, HttpServerApi, ServerApi};
use teamcraft_core::client::{CatchUpFocus, LinkApproval, LinkedSecrets, NewDeviceLink};
use teamcraft_core::clock::{Clock, SystemClock};
use teamcraft_core::connection::{ConnectionSettings, ConnectionStatus, ConnectionSupervisor};
use teamcraft_core::push_preview::{PushSession, save_push_session};
use teamcraft_core::records;
use teamcraft_core::store::{IndexedDbStore, StoreError, TrackedStore, browser_database_key};
use teamcraft_core::ui_backend::{CoreChatBackend, CoreCommand};
use teamcraft_core::ui_snapshot::SnapshotCache;
use teamcraft_core::upkeep::{self, Upkeep};
use teamcraft_core::{
  CallPreparation, Client, ClientSettings, CoreError, EnrollmentOptions, EnrollmentSecret, LinkOffer,
  PendingEnrollment, UnreadableStore, platform,
};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_crypto::identity::IdentityPublicKey;
use teamcraft_crypto::secret::SecretBytes;
use teamcraft_proto::{
  ConversationId, DeviceActivity, GatewayClientMessage, GatewayServerMessage, MessageId, UserId,
};
use wasm_bindgen::JsCast;

use crate::auth::{AuthError, Authenticator};
use crate::settings::{load_string, store_string};
use crate::sync_reads::{StoredWorkspace, SyncRead, SyncReadAnswer, answer_during_sync};
use crate::web_tabs::{self, Follower, OwnerChannel, OwnerLock, Ownership};

/// Wrong short codes allowed before a link approval is dropped.
const MAX_APPROVAL_MISSES: u8 = 3;
/// How often the loop wakes up on its own: outbox retries, heartbeats, staggered commits.
const TICK: Duration = Duration::from_secs(3);
/// How often the UI is told to read again while a long sync fills the store (issue #247).
const SYNC_PROGRESS: Duration = Duration::from_secs(1);
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
const STORE_READY_PREFIX: &str = "teamcraft.store_ready.";
/// How long to wait before checking the push subscription again after the check failed (offline).
const PUSH_CHECK_RETRY: Duration = Duration::from_secs(60);
/// What the service worker posts when the browser changed the push subscription.
const SUBSCRIPTION_CHANGED: &str = "push_subscription_changed";

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

/// Whether the workspace requires an organization background behind the camera image for this
/// user (admin.md §3.5). Unknown restricts nothing, as on desktop.
pub async fn requires_org_background() -> bool {
  match ask(|reply| Request::Query(ChatQuery::OrgBackgrounds, reply)).await {
    Ok(Ok(ChatAnswer::OrgBackgrounds { backgrounds })) => backgrounds.is_required(),
    _ => false,
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
      let plaintext = ask_attachment(conversation, message, file_id).await?;
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
    other => Err(format!("{other} is not available in the browser")),
  }
}

/// One file of a message, downloaded and decrypted in memory, for the image viewer. A follower tab
/// gets it from the tab that runs the core.
pub async fn attachment(
  conversation: ConversationId,
  message: MessageId,
  file_id: String,
) -> Result<SecretBytes, String> {
  let Some(follower) = current_follower() else {
    return ask_attachment(conversation, message, file_id).await;
  };
  let args = serde_json::json!({ "conversation": conversation, "message": message, "file_id": file_id });
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
) -> Result<SecretBytes, String> {
  ask(|reply| Request::Attachment {
    conversation,
    message,
    file_id,
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
/// when it gets the store's lock; otherwise the tab follows the one that has it.
async fn start(user: UserId) -> Result<CoreStatus, String> {
  let reuse = RUNNING.with(|running| {
    running
      .borrow()
      .as_ref()
      .is_some_and(|running| running.user() == user)
  });
  if !reuse {
    match web_tabs::try_own(user).await {
      Ownership::Owned(lock) => start_owner(user, Some(lock)).await?,
      Ownership::Unsupported => start_owner(user, None).await?,
      Ownership::Taken => start_follower(user)?,
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
      true => Some(OwnerChannel::open(user, serve_follower)?),
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

/// Follows the tab that owns the store, and takes over once that tab is gone.
fn start_follower(user: UserId) -> Result<(), String> {
  tracing::info!("TeamCraft is open in another tab; following it");
  let follower = Follower::join(user, notify_local)?;
  RUNNING.with(|running| {
    *running.borrow_mut() = Some(Running::Follower {
      user,
      follower: Rc::new(follower),
    })
  });
  wasm_bindgen_futures::spawn_local(async move {
    let Some(lock) = web_tabs::wait_to_own(user).await else {
      return;
    };
    // async-ok: a thread-local, not a signal.
    let is_still_following = RUNNING.with(|running| {
      matches!(&*running.borrow(), Some(Running::Follower { user: following, .. }) if *following == user)
    });
    if !is_still_following {
      // Dropping the lock hands it on.
      return;
    }
    tracing::info!("the other tab closed; TeamCraft runs in this tab now");
    match start_owner(user, Some(lock)).await {
      Ok(()) => notify_local(),
      Err(error) => tracing::error!(%error, "could not take over the local data"),
    }
  });
  Ok(())
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
  /// The service worker saw the browser change the push subscription.
  CheckPush,
  Enroll(EnrollRequest, oneshot::Sender<Result<(), String>>),
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
  /// The browser went online, the tab became visible or was resumed: check the connection now.
  NetworkChanged,
  /// What joining a conversation's call needs (`web_calls`).
  PrepareCall(ConversationId, oneshot::Sender<Result<CallPreparation, String>>),
  /// The user opened a conversation; a running catch-up takes it next (issue #247).
  Focus(ConversationId),
}

/// What woke the loop.
enum Wake {
  Request(Option<Request>),
  Frame(Option<Result<GatewayServerMessage, ApiError>>),
  Linked(Result<LinkedSecrets, String>),
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
  /// The last attention report, repeated after a reconnect (the server forgets it with the
  /// connection).
  maybe_attention: Option<bool>,
  /// Whether this device last said it is idle, repeated after a reconnect: a resumed session keeps
  /// what the server last knew, which may be an away timeout that ended meanwhile.
  is_idle: bool,
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
      upkeep: Upkeep::new(UPKEEP_INTERVAL),
      link: None,
      last_session_token: None,
      link_failure: LinkStatus::Idle,
      approval: None,
      approval_misses: 0,
      maybe_attention: None,
      is_idle: false,
      store_health: StoreHealth::Saved,
      maybe_flush: None,
      maybe_push_check_at_ms: None,
      maybe_last_security: None,
      snapshots: RefCell::new(SnapshotCache::new(now_ms().unsigned_abs())),
      catch_up_focus: CatchUpFocus::default(),
      deferred: VecDeque::new(),
      is_closed: false,
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
      match self.next_wake(&mut requests).await {
        Wake::Request(None) => return,
        Wake::Request(Some(request)) => self.handle(request).await,
        Wake::Frame(frame) => self.on_frame(frame).await,
        Wake::Linked(result) => self.on_linked(result).await,
        Wake::Tick => self.on_tick().await,
      }
      self.check_store();
      self.check_push().await;
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
    let until_tick_ms = deadline.saturating_sub(now_ms()).max(0);
    let Self { connection, link, .. } = self;
    let frame = Box::pin(connection.receive());
    let linked = Box::pin(async {
      match link {
        Some(run) => run.future.as_mut().await,
        None => std::future::pending().await,
      }
    });
    let tick = Box::pin(platform::sleep(Duration::from_millis(
      until_tick_ms.unsigned_abs(),
    )));
    let other = select(select(frame, linked), tick);
    match select(requests.next(), other).await {
      Either::Left((request, _)) => Wake::Request(request),
      Either::Right((Either::Left((Either::Left((frame, _)), _)), _)) => Wake::Frame(frame),
      Either::Right((Either::Left((Either::Right((linked, _)), _)), _)) => Wake::Linked(linked),
      Either::Right((Either::Right(((), _)), _)) => Wake::Tick,
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
    let store = TrackedStore::new(Rc::new(opened));
    self.store = Some(store.clone());
    watch_page_lifecycle(store.inner());
    let maybe_client = match Client::open(
      self.api(),
      store.clone(),
      Box::new(SystemClock),
      ClientSettings::default(),
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
        Ok(CoreStatus::NeedsEnrollment {
          has_existing_identity: self.has_existing_identity().await?,
          was_data_lost: load_string(&self.store_ready_key()).is_some(),
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
      ClientSettings::default(),
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
  /// screen must then offer recovery and linking instead of a new identity.
  async fn refresh_enrollment_hint(&mut self) {
    if self.client.is_none()
      && self.link.is_none()
      && let CoreStatus::NeedsEnrollment { was_data_lost, .. } = self.status
      && let Ok(has_existing_identity) = self.has_existing_identity().await
    {
      self.status = CoreStatus::NeedsEnrollment {
        has_existing_identity,
        was_data_lost,
      };
    }
  }

  fn adopt(&mut self, client: Client<Api, Store>) -> CoreStatus {
    let status = CoreStatus::Ready {
      identity_fingerprint: client.identity_fingerprint(),
    };
    self.backend = Some(CoreChatBackend::new(
      client.store().clone(),
      client.user_id(),
      Box::new(SystemClock),
    ));
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
      Request::CheckPush => self.maybe_push_check_at_ms = Some(i64::MIN),
      Request::Enroll(request, reply) => {
        let result = self.enroll(request).await;
        if result.is_ok() {
          notify_changed();
        }
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
          // Flushed: the core stops right after, and queued writes would be lost with it.
          let forgotten = match records::forget_unfinished_enrollment(store) {
            Ok(()) => store.inner().flush().await.map_err(CoreError::from),
            Err(error) => Err(error),
          };
          if let Err(error) = forgotten {
            tracing::warn!(%error, "could not drop the keys of an unfinished setup");
          }
        }
        // Deliberately ignored: the caller may have stopped waiting; nothing is left to tell.
        let _ = reply.send(());
      },
      Request::ApprovalStart { code, reply } => drop(reply.send(self.approval_start(&code).await)),
      Request::ApprovalConfirm { short_code, reply } => {
        drop(reply.send(self.approval_confirm(&short_code).await));
      },
      Request::ApprovalCancel => self.approval = None,
      Request::NetworkChanged => self.connection.network_changed(now_ms()).await,
      Request::PrepareCall(conversation, reply) => {
        let result = match self.client.as_mut() {
          Some(client) => client.prepare_call(conversation).await.map_err(describe_failure),
          None => Err(not_set_up()),
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
        reply,
      } => drop(reply.send(self.attachment(conversation, message, &file_id).await)),
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
      Request::Action(action, reply) => {
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
    let options = EnrollmentOptions {
      device_name: request.device_name,
      secret: EnrollmentSecret::Passphrase(SecretBytes::new(request.passphrase.into_bytes())),
      argon2: Argon2Settings::default(),
      maybe_attestation: None,
    };
    self.complete_enrollment(options).await
  }

  async fn complete_enrollment(&mut self, options: EnrollmentOptions) -> Result<(), String> {
    let store = self.store.clone().ok_or("the local database is not open")?;
    tracing::info!("enrolling this device");
    let client = PendingEnrollment::new()
      .complete(
        self.api(),
        store,
        Box::new(SystemClock),
        ClientSettings::default(),
        options,
      )
      .await
      .map_err(describe)?;
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
    let approval = client.start_link_approval(code).await.map_err(describe)?;
    self.approval = Some(approval);
    self.approval_misses = 0;
    Ok(())
  }

  /// Compares what the user typed with the short code of the handshake; only a match hands over the
  /// keys. A few wrong guesses end the attempt, so the code cannot be guessed by trial.
  async fn approval_confirm(&mut self, typed: &str) -> Result<(), String> {
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
    client.complete_link_approval(approval).await.map_err(describe)
  }

  fn snapshot(&mut self) -> Result<ChatSnapshot, String> {
    let security = self.security_snapshot()?;
    self.maybe_last_security = Some(security.clone());
    let (Some(backend), Some(store)) = (self.backend.as_ref(), self.store.as_ref()) else {
      return Err(not_set_up());
    };
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
    })
  }

  fn window(&self, conversation: ConversationId, start: usize, len: usize) -> Result<MessageWindow, String> {
    let backend = self.backend.as_ref().ok_or_else(not_set_up)?;
    Ok(MessageWindow::read(backend, conversation, start, len))
  }

  async fn attachment(
    &self,
    conversation: ConversationId,
    message: MessageId,
    file_id: &str,
  ) -> Result<SecretBytes, String> {
    let (Some(client), Some(backend)) = (self.client.as_ref(), self.backend.as_ref()) else {
      return Err(not_set_up());
    };
    let reference = backend
      .attachment_ref(conversation, message, file_id)
      .ok_or("no such file in this conversation")?;
    client.open_attachment(&reference).await.map_err(describe)
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
      ChatAction::SetActivity { is_idle } => {
        self.send_activity(*is_idle).await;
        return Ok(());
      },
      ChatAction::SetAttention { is_attentive } => {
        self.send_attention(*is_attentive).await;
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
      | ChatAction::SetActivity { .. }
      | ChatAction::SetAttention { .. }
      | ChatAction::AcceptKeyChange { .. }
      | ChatAction::PublishPolicy { .. }
      | ChatAction::PublishRoster { .. }
      | ChatAction::AdminJoinChannel { .. } => {},
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
    let commands: Vec<CoreCommand> = self
      .backend
      .as_ref()
      .map(CoreChatBackend::drain_commands)
      .unwrap_or_default();
    let Some(client) = self.client.as_mut() else {
      return Ok(0);
    };
    let count = commands.len();
    let mut maybe_failure = None;
    for command in commands {
      // A command can take long (an upload, a big history page); heartbeats go on meanwhile.
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

  async fn flush(&mut self) {
    let Some(client) = self.client.as_mut() else {
      return;
    };
    if self.connection.keep_alive(now_ms(), upkeep::flush(client)).await {
      notify_changed();
    }
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
      ..
    } = self
    else {
      return;
    };
    let outcome = {
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
          };
          reads
            .serve(connecting.as_mut(), requests, deferred, is_closed)
            .await
        },
      }
    };
    if outcome.is_some() {
      self.repeat_reports().await;
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
    if now < self.next_tick_ms {
      // Woken for the connection only.
      return;
    }
    self.next_tick_ms = now.saturating_add(millis(TICK));
    self.save_push_session();
    let Some(client) = self.client.as_mut() else {
      return;
    };
    // The upkeep may resync many conversations; heartbeats go on meanwhile.
    if self.connection.keep_alive(now, self.upkeep.tick(client)).await {
      notify_changed();
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
  let link = NewDeviceLink::connect(&api, &offer).await.map_err(describe)?;
  *view.borrow_mut() = LinkStatus::Confirm {
    code: offer.to_code(),
    short_code: link.short_code(),
  };
  notify_changed();
  link.receive_secrets(&store, &device_name).await.map_err(describe)
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

/// Tells the service worker which database holds this account's keys, so it can open pushes. It
/// cannot ask the page later (no page may be open), so it keeps the name in its own cache.
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
    if set("type", "push_database")
      .and_then(|_| set("name", &name))
      .is_ok()
    {
      drop(worker.post_message(&message));
    }
  });
}
