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

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

use futures_channel::{mpsc, oneshot};
use futures_util::StreamExt;
use futures_util::future::{Either, select};
use serde::de::DeserializeOwned;
use serde_json::Value;
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::model::{Message, ThreadId};
use teamcraft_client_api::remote::{
  ChatAction, ChatAnswer, ChatQuery, ChatSnapshot, ContactKeys, CoreStatus, EnrollRequest, LinkStatus,
  MessageWindow, SecuritySnapshot,
};
use teamcraft_core::api::{AccessTokenSource, ApiError, GatewayConnection, HttpServerApi, ServerApi};
use teamcraft_core::client::{LinkApproval, LinkedSecrets, NewDeviceLink};
use teamcraft_core::clock::SystemClock;
use teamcraft_core::push_preview::{PushSession, save_push_session};
use teamcraft_core::records;
use teamcraft_core::store::IndexedDbStore;
use teamcraft_core::store::browser_database_key;
use teamcraft_core::ui_backend::{CoreChatBackend, CoreCommand};
use teamcraft_core::workspace::RosterRole;
use teamcraft_core::{
  Client, ClientSettings, CoreError, EnrollmentOptions, EnrollmentSecret, LinkOffer, PendingEnrollment,
  platform,
};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_crypto::identity::IdentityPublicKey;
use teamcraft_crypto::secret::SecretBytes;
use teamcraft_proto::{
  ConversationId, DeviceActivity, GatewayClientMessage, GatewayServerMessage, UserId, WorkspacePolicy,
};
use wasm_bindgen::JsCast;

use crate::auth::{AuthError, Authenticator};

/// Wrong short codes allowed before a link approval is dropped.
const MAX_APPROVAL_MISSES: u8 = 3;
/// How often the loop wakes up on its own: outbox retries, heartbeats, staggered commits.
const TICK: Duration = Duration::from_secs(3);
/// Gateway heartbeat interval (the server allows some slack).
const HEARTBEAT: Duration = Duration::from_secs(25);
/// How often account data (saved items, drafts) is pulled from the user's other devices.
const ACCOUNT_SYNC: Duration = Duration::from_secs(60);
/// How often the core's copy of the access token is renewed from the page's session.
const TOKEN_REFRESH: Duration = Duration::from_secs(20);
/// How long the access token stored for the service worker is used. Tokens are renewed every
/// [`TOKEN_REFRESH`]; the stored one only has to outlive a push that arrives while no page is open.
const PUSH_SESSION_LIFETIME_MS: i64 = 4 * 60 * 1000;
const RECONNECT_MIN: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(30);

type Api = HttpServerApi<WebTokens>;
type Store = Rc<IndexedDbStore>;
type Listener = Rc<RefCell<dyn FnMut()>>;

thread_local! {
  static SETUP: RefCell<Option<Setup>> = const { RefCell::new(None) };
  static RUNNING: RefCell<Option<Running>> = const { RefCell::new(None) };
  static LISTENERS: RefCell<Vec<Listener>> = const { RefCell::new(Vec::new()) };
}

/// What the page tells the core once: who signs the requests and where the server is.
struct Setup {
  auth: Rc<dyn Authenticator>,
  server_url: String,
}

struct Running {
  user: UserId,
  requests: mpsc::UnboundedSender<Request>,
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

/// Calls `callback` whenever the core's data changed.
pub fn listen(callback: impl FnMut() + 'static) {
  LISTENERS.with(|listeners| listeners.borrow_mut().push(Rc::new(RefCell::new(callback))));
}

/// Answers one `core_*` command the way the desktop shell does.
pub async fn call(command: &str, args: Value) -> Result<Value, String> {
  match command {
    "core_start" => {
      let user: UserId = field(&args, "user")?;
      to_value(start(user).await?)
    },
    "core_stop" => {
      stop();
      Ok(Value::Null)
    },
    "core_status" => to_value(ask(Request::Status).await?),
    "core_enroll" => {
      let request: EnrollRequest = field(&args, "request")?;
      ask(|reply| Request::Enroll(request, reply))
        .await?
        .map(|()| Value::Null)
    },
    "core_snapshot" => to_value(ask(Request::Snapshot).await??),
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
      ask(|reply| Request::Action(action, reply)).await??;
      Ok(Value::Null)
    },
    "core_query" => {
      let query: ChatQuery = field(&args, "query")?;
      to_value(ask(|reply| Request::Query(query, reply)).await??)
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

fn field<T: DeserializeOwned>(args: &Value, name: &str) -> Result<T, String> {
  let value = args.get(name).cloned().unwrap_or(Value::Null);
  serde_json::from_value(value).map_err(|error| format!("bad argument {name}: {error}"))
}

fn to_value<T: serde::Serialize>(value: T) -> Result<Value, String> {
  serde_json::to_value(value).map_err(|error| error.to_string())
}

/// Starts (or reuses) the core for `user` and reports where it stands.
async fn start(user: UserId) -> Result<CoreStatus, String> {
  let reuse = RUNNING.with(|running| {
    running
      .borrow()
      .as_ref()
      .is_some_and(|running| running.user == user)
  });
  if !reuse {
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
    let core = Core::new(server_url, user, tokens.clone());
    let generation = std::rc::Rc::new(());
    keep_token_fresh(auth, tokens, Rc::downgrade(&generation));
    wasm_bindgen_futures::spawn_local(async move {
      core.run(receiver).await;
      drop(generation);
    });
    // Replacing a running core (another account signed in) drops its sender, which stops it.
    RUNNING.with(|running| {
      *running.borrow_mut() = Some(Running {
        user,
        requests: sender,
      })
    });
  }
  ask(Request::Status).await
}

fn stop() {
  RUNNING.with(|running| *running.borrow_mut() = None);
}

fn send(request: Request) -> Result<(), String> {
  RUNNING.with(|running| {
    running
      .borrow()
      .as_ref()
      .ok_or_else(|| "the core has not been started".to_owned())?
      .requests
      .unbounded_send(request)
      .map_err(|_| stopped())
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

fn notify_changed() {
  let listeners: Vec<_> = LISTENERS.with(|listeners| listeners.borrow().clone());
  for listener in listeners {
    if let Ok(mut callback) = listener.try_borrow_mut() {
      (*callback)();
    }
  }
}

enum Request {
  Status(oneshot::Sender<CoreStatus>),
  Enroll(EnrollRequest, oneshot::Sender<Result<(), String>>),
  Snapshot(oneshot::Sender<Result<ChatSnapshot, String>>),
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
  Action(ChatAction, oneshot::Sender<Result<(), String>>),
  Query(ChatQuery, oneshot::Sender<Result<ChatAnswer, String>>),
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
  ApprovalStart {
    code: String,
    reply: oneshot::Sender<Result<(), String>>,
  },
  ApprovalConfirm {
    short_code: String,
    reply: oneshot::Sender<Result<(), String>>,
  },
  ApprovalCancel,
}

/// What woke the loop.
enum Wake {
  Request(Option<Request>),
  Frame(Option<Result<GatewayServerMessage, ApiError>>),
  Linked(Result<LinkedSecrets, String>),
  Tick,
}

struct Core {
  server_url: String,
  user: UserId,
  tokens: WebTokens,
  store: Option<Store>,
  client: Option<Client<Api, Store>>,
  backend: Option<CoreChatBackend<Store>>,
  gateway: Option<<Api as ServerApi>::Gateway>,
  status: CoreStatus,
  backoff: Duration,
  reconnect_at_ms: i64,
  last_heartbeat_ms: i64,
  last_account_sync_ms: i64,
  link: Option<LinkRun>,
  /// The access token last written to the push session, so it is only stored when it changes.
  last_session_token: Option<String>,
  /// Why the last link failed, until the user tries again.
  link_failure: LinkStatus,
  approval: Option<LinkApproval<<Api as ServerApi>::Rendezvous>>,
  approval_misses: u8,
}

/// A link in progress on this (new) device.
struct LinkRun {
  device_name: String,
  view: Rc<RefCell<LinkStatus>>,
  future: Pin<Box<dyn Future<Output = Result<LinkedSecrets, String>>>>,
}

impl Core {
  fn new(server_url: String, user: UserId, tokens: WebTokens) -> Self {
    Self {
      server_url,
      user,
      tokens,
      store: None,
      client: None,
      backend: None,
      gateway: None,
      status: CoreStatus::NotStarted,
      backoff: RECONNECT_MIN,
      reconnect_at_ms: i64::MIN,
      last_heartbeat_ms: i64::MIN,
      last_account_sync_ms: i64::MIN,
      link: None,
      last_session_token: None,
      link_failure: LinkStatus::Idle,
      approval: None,
      approval_misses: 0,
    }
  }

  async fn run(mut self, mut requests: mpsc::UnboundedReceiver<Request>) {
    self.open().await;
    loop {
      if self.client.is_some() && self.gateway.is_none() && now_ms() >= self.reconnect_at_ms {
        self.connect().await;
      }
      match self.next_wake(&mut requests).await {
        Wake::Request(None) => return,
        Wake::Request(Some(request)) => self.handle(request).await,
        Wake::Frame(frame) => self.on_frame(frame).await,
        Wake::Linked(result) => self.on_linked(result).await,
        Wake::Tick => self.on_tick().await,
      }
    }
  }

  async fn next_wake(&mut self, requests: &mut mpsc::UnboundedReceiver<Request>) -> Wake {
    let Self { gateway, link, .. } = self;
    let frame = Box::pin(async {
      match gateway {
        Some(connection) => connection.receive().await,
        None => std::future::pending().await,
      }
    });
    let linked = Box::pin(async {
      match link {
        Some(run) => run.future.as_mut().await,
        None => std::future::pending().await,
      }
    });
    let tick = Box::pin(platform::sleep(TICK));
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
    let key = browser_database_key(&name)
      .await
      .map_err(|error| error.to_string())?;
    let store = Rc::new(
      IndexedDbStore::open(&name, &key)
        .await
        .map_err(|error| error.to_string())?,
    );
    self.store = Some(store.clone());
    match Client::open(
      self.api(),
      store,
      Box::new(SystemClock),
      ClientSettings::default(),
    )
    .map_err(|error| error.to_string())?
    {
      Some(client) => Ok(self.adopt(client)),
      None => Ok(CoreStatus::NeedsEnrollment {
        has_existing_identity: self.has_existing_identity().await?,
      }),
    }
  }

  async fn has_existing_identity(&self) -> Result<bool, String> {
    match self.api().identity_key(self.user).await {
      Ok(_) => Ok(true),
      Err(error) if error.is_not_found() => Ok(false),
      Err(error) => Err(error.to_string()),
    }
  }

  /// Another device may have enrolled the account since the database was opened; the enrollment
  /// screen must then offer recovery and linking instead of a new identity.
  async fn refresh_enrollment_hint(&mut self) {
    if self.client.is_none()
      && self.link.is_none()
      && let CoreStatus::NeedsEnrollment { .. } = self.status
      && let Ok(has_existing_identity) = self.has_existing_identity().await
    {
      self.status = CoreStatus::NeedsEnrollment {
        has_existing_identity,
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
    self.client = Some(client);
    self.reconnect_at_ms = i64::MIN;
    status
  }

  fn api(&self) -> Api {
    HttpServerApi::new(self.server_url.clone(), self.tokens.clone())
  }

  async fn handle(&mut self, request: Request) {
    match request {
      Request::Status(reply) => {
        self.refresh_enrollment_hint().await;
        drop(reply.send(self.status.clone()));
      },
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
      },
      Request::ApprovalStart { code, reply } => drop(reply.send(self.approval_start(&code).await)),
      Request::ApprovalConfirm { short_code, reply } => {
        drop(reply.send(self.approval_confirm(&short_code).await));
      },
      Request::ApprovalCancel => self.approval = None,
      Request::Snapshot(reply) => drop(reply.send(self.snapshot())),
      Request::Messages {
        conversation,
        start,
        len,
        reply,
      } => drop(reply.send(self.window(conversation, start, len))),
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
        let result = self.action(action).await;
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
        // A search can ask for messages found only in shared index segments.
        self.run_queued_commands().await;
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
    let future = Box::pin(link_task(self.api(), offer, view.clone()));
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

  fn snapshot(&self) -> Result<ChatSnapshot, String> {
    let backend = self.backend.as_ref().ok_or_else(not_set_up)?;
    let mut conversations = Vec::new();
    for space in backend.spaces() {
      conversations.extend(backend.space_conversations(space.id));
    }
    conversations.extend(backend.direct_conversations());
    let message_counts = conversations
      .iter()
      .map(|conversation| (conversation.id, backend.message_count(conversation.id)))
      .collect();
    Ok(ChatSnapshot {
      me: backend.me(),
      users: backend.users(),
      spaces: backend.spaces(),
      conversations,
      message_counts,
      security: self.security_snapshot()?,
    })
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
      })
      .collect();
    Ok(SecuritySnapshot {
      my_fingerprint: client.identity_fingerprint(),
      contacts,
    })
  }

  fn window(&self, conversation: ConversationId, start: usize, len: usize) -> Result<MessageWindow, String> {
    let backend = self.backend.as_ref().ok_or_else(not_set_up)?;
    Ok(MessageWindow {
      conversation,
      start,
      total: backend.message_count(conversation),
      messages: backend.messages(conversation, start, len),
    })
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
          .map_err(describe);
      },
      ChatAction::OpenDm { user } => return client.open_dm(*user).await.map(|_| ()).map_err(describe),
      ChatAction::AddMember { conversation, user } => {
        return client.add_member(*conversation, *user).await.map_err(describe);
      },
      ChatAction::ChangePassphrase { passphrase } => {
        let passphrase = SecretBytes::new(passphrase.clone().into_bytes());
        return client
          .upload_key_bag_with(&passphrase, Argon2Settings::default())
          .await
          .map_err(describe);
      },
      ChatAction::SetActivity { is_idle } => {
        self.send_activity(*is_idle).await;
        return Ok(());
      },
      ChatAction::AcceptKeyChange { user } => {
        let mut pinned = records::pinned_identities(client.store()).map_err(describe)?;
        if let Some(entry) = pinned.get_mut(user)
          && let Some(new_key) = entry.maybe_changed_to.take()
        {
          entry.public_key = new_key;
          records::pin_identity(client.store(), *user, entry).map_err(describe)?;
        }
        return Ok(());
      },
      ChatAction::PublishPolicy {
        policy,
        owners,
        admins,
      } => return publish_policy(client, policy.clone(), owners, admins).await,
      ChatAction::AdminJoinChannel { conversation } => {
        let me = client.user_id();
        return client.admin_add_member(*conversation, me).await.map_err(describe);
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
      ChatAction::SetPresence { presence } => backend.set_presence(presence),
      ChatAction::SetCustomStatus { maybe_status } => backend.set_custom_status(maybe_status),
      ChatAction::CreateChannel { .. }
      | ChatAction::OpenDm { .. }
      | ChatAction::AddMember { .. }
      | ChatAction::ChangePassphrase { .. }
      | ChatAction::SetActivity { .. }
      | ChatAction::AcceptKeyChange { .. }
      | ChatAction::PublishPolicy { .. }
      | ChatAction::AdminJoinChannel { .. } => {},
      // Message actions, files and workspace management go to the backend, which queues what
      // needs the engine as commands.
      other => {
        if let Some(unhandled) = other.apply_to(backend) {
          tracing::warn!(?unhandled, "chat action not handled");
        }
      },
    }
    self.run_queued_commands().await;
    // New messages go out right away; failures stay in the outbox for the periodic retry.
    self.flush().await;
    Ok(())
  }

  async fn run_queued_commands(&mut self) {
    let commands: Vec<CoreCommand> = self
      .backend
      .as_ref()
      .map(CoreChatBackend::drain_commands)
      .unwrap_or_default();
    let Some(client) = self.client.as_mut() else {
      return;
    };
    for command in commands {
      if let Err(error) = client.run_ui_command(command).await {
        tracing::warn!(%error, "UI command failed");
      }
    }
  }

  async fn flush(&mut self) {
    let Some(client) = self.client.as_mut() else {
      return;
    };
    if let Err(error) = client.generate_pending_previews().await {
      tracing::debug!(%error, "link previews not made");
    }
    let mut is_changed = false;
    match client.upload_pending().await {
      Ok(uploaded) => is_changed |= uploaded > 0,
      Err(error) => tracing::debug!(%error, "uploads did not finish"),
    }
    match client.flush_outbox().await {
      Ok(report) => {
        if report.pending > 0 || report.failed > 0 {
          tracing::debug!(
            sent = report.sent,
            pending = report.pending,
            failed = report.failed,
            "outbox"
          );
        }
        is_changed |= report.sent > 0;
      },
      Err(error) => tracing::debug!(%error, "outbox flush did not finish"),
    }
    if is_changed {
      notify_changed();
    }
  }

  async fn connect(&mut self) {
    let Some(client) = self.client.as_mut() else {
      return;
    };
    match client.connect_gateway().await {
      Ok((connection, outcome)) => {
        tracing::info!(?outcome, "gateway connected");
        self.gateway = Some(connection);
        self.backoff = RECONNECT_MIN;
        self.last_heartbeat_ms = now_ms();
        notify_changed();
      },
      Err(error) => {
        tracing::warn!(%error, "gateway connection failed, retrying");
        self.schedule_reconnect();
      },
    }
  }

  fn schedule_reconnect(&mut self) {
    self.gateway = None;
    self.reconnect_at_ms = now_ms().saturating_add(millis(self.backoff));
    self.backoff = (self.backoff * 2).min(RECONNECT_MAX);
  }

  async fn on_frame(&mut self, frame: Option<Result<GatewayServerMessage, ApiError>>) {
    let outcome = match (frame, self.client.as_mut()) {
      (Some(Ok(frame)), Some(client)) => client.handle_frame(frame).await,
      (Some(Err(error)), _) => Err(CoreError::Api(error)),
      (None, _) => Err(CoreError::Gateway("connection closed".to_owned())),
      (_, None) => return,
    };
    match outcome {
      Ok(()) => notify_changed(),
      Err(error) => {
        tracing::warn!(%error, "gateway ended");
        self.schedule_reconnect();
      },
    }
  }

  async fn on_tick(&mut self) {
    let now = now_ms();
    if now.saturating_sub(self.last_heartbeat_ms) >= millis(HEARTBEAT)
      && let Some(connection) = self.gateway.as_mut()
    {
      self.last_heartbeat_ms = now;
      if let Err(error) = connection.send(&GatewayClientMessage::Heartbeat).await {
        tracing::warn!(%error, "heartbeat failed");
        self.schedule_reconnect();
      }
    }
    self.save_push_session();
    let Some(client) = self.client.as_mut() else {
      return;
    };
    if client.is_key_bag_dirty() || now.saturating_sub(self.last_account_sync_ms) >= millis(ACCOUNT_SYNC) {
      self.last_account_sync_ms = now;
      match client.sync_account_data().await {
        Ok(()) => notify_changed(),
        Err(error) => tracing::debug!(%error, "account data not synced yet"),
      }
    }
    // Staggered commits (a colleague's new device) come due without a gateway event.
    if let Err(error) = client.apply_pending_changes().await {
      tracing::debug!(%error, "pending membership changes not applied yet");
    }
    self.flush().await;
  }

  /// Tells the server whether this device is idle (docs/gateway.md); a failure ends the connection
  /// like a failed heartbeat.
  async fn send_activity(&mut self, is_idle: bool) {
    let Some(connection) = self.gateway.as_mut() else {
      return;
    };
    let activity = if is_idle {
      DeviceActivity::Idle
    } else {
      DeviceActivity::Active
    };
    if let Err(error) = connection
      .send(&GatewayClientMessage::SetActivity { activity })
      .await
    {
      tracing::warn!(%error, "could not report the device activity");
      self.schedule_reconnect();
    }
  }
}

fn now_ms() -> i64 {
  js_sys::Date::now() as i64
}

fn millis(duration: Duration) -> i64 {
  i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

fn not_set_up() -> String {
  "this device is not set up yet".to_owned()
}

/// Error text for the UI: the cause, without internal type names.
fn describe(error: CoreError) -> String {
  error.to_string()
}

/// Signs and publishes a policy version. A workspace without a signed roster gets one first, with
/// the signer as owner (the root of trust, e2ee.md §11); later policy changes only append.
async fn publish_policy(
  client: &mut Client<Api, Store>,
  policy: WorkspacePolicy,
  owners: &[UserId],
  admins: &[UserId],
) -> Result<(), String> {
  client.sync_workspace().await.map_err(describe)?;
  if client.workspace().map_err(describe)?.roster.version == 0 {
    let me = client.user_id();
    let mut roles: Vec<(UserId, RosterRole)> = vec![(me, RosterRole::Owner)];
    roles.extend(
      owners
        .iter()
        .filter(|user| **user != me)
        .map(|user| (*user, RosterRole::Owner)),
    );
    roles.extend(
      admins
        .iter()
        .filter(|user| **user != me && !owners.contains(user))
        .map(|user| (*user, RosterRole::Admin)),
    );
    client.publish_roster(&roles).await.map_err(describe)?;
  }
  client.publish_policy(policy).await.map_err(describe)
}

/// Waits for the existing device to answer, shows the short code, then waits for the keys.
async fn link_task(
  api: Api,
  offer: LinkOffer,
  view: Rc<RefCell<LinkStatus>>,
) -> Result<LinkedSecrets, String> {
  let link = NewDeviceLink::connect(&api, &offer).await.map_err(describe)?;
  *view.borrow_mut() = LinkStatus::Confirm {
    code: offer.to_code(),
    short_code: link.short_code(),
  };
  notify_changed();
  link.receive_secrets().await.map_err(describe)
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
