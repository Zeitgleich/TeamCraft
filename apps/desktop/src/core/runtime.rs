use std::cell::RefCell;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use secrecy::ExposeSecret;
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::model::{Message, ThreadId};
use teamcraft_client_api::remote::{
  ChatAction, ChatAnswer, ChatQuery, ChatSnapshot, ContactKeys, CoreStatus, EnrollRequest, LinkStatus,
  MessageWindow, SecuritySnapshot,
};
use teamcraft_core::api::{AccessTokenSource, ApiError, HttpServerApi, ServerApi};
use teamcraft_core::client::{LinkApproval, LinkedSecrets, NewDeviceLink};
use teamcraft_core::clock::{Clock, SystemClock};
use teamcraft_core::connection::{
  ConnectionSettings, ConnectionState, ConnectionStatus, ConnectionSupervisor,
};
use teamcraft_core::records;
use teamcraft_core::store::{
  DatabaseKeySource, KeySourceError, KeyringKeySource, SqlCipherStore, StoreError, StoreLock,
};
use teamcraft_core::ui_backend::{CoreChatBackend, CoreCommand};
use teamcraft_core::upkeep::{self, Upkeep};
use teamcraft_core::workspace::RosterRole;
use teamcraft_core::{
  CallKeySigner, Client, ClientSettings, CoreError, EnrollmentOptions, EnrollmentSecret, LinkOffer,
  PendingEnrollment,
};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_crypto::identity::IdentityPublicKey;
use teamcraft_crypto::identity::VerifiedDevice;
use teamcraft_crypto::secret::{SecretBytes, SymmetricKey};
use teamcraft_proto::{
  BackgroundId, CallJoinTokenResponse, ClientUpdatePolicy, ConversationId, DeviceActivity, DeviceId,
  GatewayClientMessage, GatewayEvent, OrgBackgroundId, UserId, WorkspacePolicy,
};
use tokio::sync::{mpsc, oneshot};

use crate::auth::{AuthService, TokenPeek, TokenStore};

/// Wrong short codes allowed before a link approval is dropped.
const MAX_APPROVAL_MISSES: u8 = 3;
/// Retries (outbox, uploads, previews), staggered commits and the other periodic upkeep; also the
/// connection's regular check.
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);
/// How often account data (saved items, drafts) is pulled from the user's other devices.
const ACCOUNT_SYNC: Duration = Duration::from_secs(60);
/// A core of this process that is just stopping may hold the store lock for a moment longer.
const STORE_LOCK_ATTEMPTS: u32 = 15;
const STORE_LOCK_RETRY: Duration = Duration::from_millis(200);
/// Opening without an access token yet (sign-in still settling) is retried after this, doubling up
/// to [`OPEN_RETRY_MAX`].
const OPEN_RETRY_FIRST: Duration = Duration::from_millis(250);
const OPEN_RETRY_MAX: Duration = Duration::from_secs(10);

/// Where the core keeps its data and talks to.
#[derive(Clone, Debug)]
pub struct CoreConfig {
  pub server_url: String,
  /// Directory of the encrypted databases; one file per user.
  pub data_dir: PathBuf,
  /// Keystore namespace, so several servers do not share database keys.
  pub keystore_scope: String,
  pub argon2: Argon2Settings,
}

/// The access token the core sends, with the server (origin) it belongs to. With a live source
/// (the login session) every request reads the current token, so a core started right after
/// sign-in never sees an empty cache; while a refresh holds the session, the token seen last is
/// used only as long as it is still valid. Without a source (tests) it holds what [`Self::set`]
/// put there.
#[derive(Clone, Default)]
pub struct TokenCache {
  last: Arc<Mutex<Option<CachedToken>>>,
  maybe_source: Option<Arc<dyn Fn() -> TokenPeek + Send + Sync>>,
}

struct CachedToken {
  server_url: String,
  token: String,
  /// `None` for a token set by hand, which has no known expiry.
  maybe_valid_until: Option<Instant>,
}

impl TokenCache {
  /// Reads the current token of `service` on every request.
  pub fn live<S: TokenStore>(service: Arc<AuthService<S>>) -> Self {
    Self {
      last: Arc::default(),
      maybe_source: Some(Arc::new(move || service.peek_server_bearer_token())),
    }
  }

  /// `(server origin, access token)`, or nothing while signed out.
  #[cfg(test)]
  pub fn set(&self, maybe_token: Option<(String, String)>) {
    self.remember(maybe_token.map(|(server_url, token)| CachedToken {
      server_url,
      token,
      maybe_valid_until: None,
    }));
  }

  /// The tokens of one server: a core never sends another server's token, even right after the
  /// user switched servers.
  pub fn for_server(&self, server_url: &str) -> ServerTokens {
    ServerTokens {
      cache: self.clone(),
      server_url: server_url.to_owned(),
    }
  }

  fn current(&self) -> Option<(String, String)> {
    match self.maybe_source.as_ref().map(|source| source()) {
      Some(TokenPeek::Ready {
        server_url,
        token,
        valid_for,
      }) => {
        let token = token.expose_secret().to_owned();
        self.remember(Some(CachedToken {
          server_url: server_url.clone(),
          token: token.clone(),
          maybe_valid_until: Some(Instant::now() + valid_for),
        }));
        Some((server_url, token))
      },
      Some(TokenPeek::Unavailable) => {
        self.remember(None);
        None
      },
      Some(TokenPeek::Busy) | None => self.last.lock().ok().and_then(|slot| {
        slot
          .as_ref()
          .filter(|cached| {
            cached
              .maybe_valid_until
              .is_none_or(|until| until > Instant::now())
          })
          .map(|cached| (cached.server_url.clone(), cached.token.clone()))
      }),
    }
  }

  fn remember(&self, maybe_token: Option<CachedToken>) {
    if let Ok(mut slot) = self.last.lock() {
      *slot = maybe_token;
    }
  }
}

/// [`TokenCache`] limited to one server.
#[derive(Clone)]
pub struct ServerTokens {
  cache: TokenCache,
  server_url: String,
}

impl AccessTokenSource for ServerTokens {
  fn access_token(&self) -> Result<String, ApiError> {
    self
      .cache
      .current()
      .filter(|(server_url, _)| *server_url == self.server_url)
      .map(|(_, token)| token)
      .ok_or_else(|| ApiError::Token("not signed in".to_owned()))
  }
}

/// A database key that comes from the OS keystore, or a fixed one in tests.
pub enum KeyChoice {
  Keyring {
    service: String,
    account: String,
  },
  /// A fixed key (tests).
  #[cfg(test)]
  Fixed(Vec<u8>),
}

impl DatabaseKeySource for KeyChoice {
  fn database_key(&self) -> Result<SymmetricKey, KeySourceError> {
    match self {
      Self::Keyring { service, account } => KeyringKeySource::new(service, account).database_key(),
      #[cfg(test)]
      Self::Fixed(bytes) => SymmetricKey::from_slice(bytes).ok_or(KeySourceError::InvalidKey(bytes.len())),
    }
  }
}

/// Receives the gateway's call events (start, update, end, ring) on the core thread.
pub type CallEventHandler = Arc<dyn Fn(GatewayEvent) + Send + Sync>;

/// Everything joining a conversation's call needs from the core (e2ee.md §8).
pub struct CallPreparation {
  pub join: CallJoinTokenResponse,
  pub user_id: UserId,
  pub device_id: DeviceId,
  /// Signs this device's per-call DAVE key; the device key itself stays in the core.
  pub signer: CallKeySigner,
  /// The conversation's devices (this one included) whose certificates verified.
  pub devices: Vec<VerifiedDevice>,
}

type Api = HttpServerApi<ServerTokens>;
type Gateway = <Api as ServerApi>::Gateway;
type Store = Arc<SqlCipherStore>;

// Short-lived messages on a channel: boxing the one large variant would only add an allocation.
#[allow(clippy::large_enum_variant)]
enum Request {
  Status(oneshot::Sender<CoreStatus>),
  Enroll(EnrollRequest, oneshot::Sender<Result<(), String>>),
  Snapshot(oneshot::Sender<Result<ChatSnapshot, String>>),
  UpdatePolicy(oneshot::Sender<Option<ClientUpdatePolicy>>),
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
  // Boxed: ChatAction is large and every other request would pay for its size.
  Action(Box<ChatAction>, oneshot::Sender<Result<(), String>>),
  Query(ChatQuery, oneshot::Sender<Result<ChatAnswer, String>>),
  PrepareCall(ConversationId, oneshot::Sender<Result<CallPreparation, String>>),
  CallJoinToken(
    ConversationId,
    oneshot::Sender<Result<CallJoinTokenResponse, String>>,
  ),
  BackgroundPicture(BackgroundId, oneshot::Sender<Result<Vec<u8>, String>>),
  OrgBackgroundPicture(OrgBackgroundId, oneshot::Sender<Result<Vec<u8>, String>>),
  LinkStart {
    device_name: String,
    reply: oneshot::Sender<Result<(), String>>,
  },
  LinkStatus(oneshot::Sender<LinkStatus>),
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
  /// The network changed or the computer woke up: check the connection now.
  NetworkChanged,
  /// Ends the core loop, closing the database and releasing the store lock.
  Shutdown,
}

/// Handle to the core thread. Dropping it stops the thread.
pub struct CoreRuntime {
  requests: mpsc::UnboundedSender<Request>,
  /// Read without asking the core thread, which may be busy connecting.
  connection: ConnectionStatus,
  /// Taken by [`CoreRuntime::shutdown`] or on drop.
  thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl CoreRuntime {
  /// Starts the core for one signed-in user. `on_change` is called after every store change,
  /// `on_call_event` with every call event from the gateway.
  pub fn spawn(
    config: CoreConfig,
    user: UserId,
    key: KeyChoice,
    tokens: TokenCache,
    on_change: Arc<dyn Fn() + Send + Sync>,
    on_call_event: CallEventHandler,
  ) -> Self {
    let (requests, receiver) = mpsc::unbounded_channel();
    let connection = ConnectionStatus::new();
    let status = connection.clone();
    let thread = std::thread::Builder::new()
      .name("teamcraft-core".to_owned())
      .spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
          Ok(runtime) => runtime,
          Err(error) => {
            tracing::error!(%error, "could not start the core runtime");
            return;
          },
        };
        let core = Core::new(config, user, key, tokens, on_change, on_call_event, status);
        runtime.block_on(core.run(receiver));
      })
      .ok();
    Self {
      requests,
      connection,
      thread: Mutex::new(thread),
    }
  }

  /// Stops the core thread and waits until it closed the database and released the store lock,
  /// even while other handles to this runtime are still around (they then get "not running").
  pub fn shutdown(&self) {
    drop(self.requests.send(Request::Shutdown));
    let maybe_thread = self.thread.lock().ok().and_then(|mut slot| slot.take());
    if let Some(thread) = maybe_thread {
      drop(thread.join());
    }
  }

  /// Where the gateway connection stands.
  pub fn connection_state(&self) -> ConnectionState {
    self.connection.get()
  }

  /// Reconnects at once instead of after the backoff: the network changed or the computer woke up.
  pub fn network_changed(&self) {
    drop(self.requests.send(Request::NetworkChanged));
  }

  pub async fn status(&self) -> CoreStatus {
    self.ask(Request::Status).await.unwrap_or(CoreStatus::Failed {
      message: "the core is not running".into(),
    })
  }

  pub async fn enroll(&self, request: EnrollRequest) -> Result<(), String> {
    self
      .ask(|reply| Request::Enroll(request, reply))
      .await
      .unwrap_or_else(|| Err(stopped()))
  }

  pub async fn snapshot(&self) -> Result<ChatSnapshot, String> {
    self
      .ask(Request::Snapshot)
      .await
      .unwrap_or_else(|| Err(stopped()))
  }

  /// The update rules of the workspace policy this device verified (docs/updates.md §4.4).
  pub async fn client_update_policy(&self) -> Option<ClientUpdatePolicy> {
    self.ask(Request::UpdatePolicy).await.flatten()
  }

  pub async fn messages(
    &self,
    conversation: ConversationId,
    start: usize,
    len: usize,
  ) -> Result<MessageWindow, String> {
    self
      .ask(|reply| Request::Messages {
        conversation,
        start,
        len,
        reply,
      })
      .await
      .unwrap_or_else(|| Err(stopped()))
  }

  pub async fn thread(&self, conversation: ConversationId, thread: ThreadId) -> Result<Vec<Message>, String> {
    self
      .ask(|reply| Request::Thread {
        conversation,
        thread,
        reply,
      })
      .await
      .unwrap_or_else(|| Err(stopped()))
  }

  pub async fn action(&self, action: ChatAction) -> Result<(), String> {
    self
      .ask(|reply| Request::Action(Box::new(action), reply))
      .await
      .unwrap_or_else(|| Err(stopped()))
  }

  /// New device: starts showing a link code and waiting for an existing device.
  pub async fn link_start(&self, device_name: String) -> Result<(), String> {
    self
      .ask(|reply| Request::LinkStart { device_name, reply })
      .await
      .unwrap_or_else(|| Err(stopped()))
  }

  /// Answers a read beyond snapshot and windows (search, saved items, files, message extras, ...).
  pub async fn query(&self, query: ChatQuery) -> Result<ChatAnswer, String> {
    self
      .ask(|reply| Request::Query(query, reply))
      .await
      .unwrap_or_else(|| Err(stopped()))
  }

  pub async fn link_status(&self) -> LinkStatus {
    self.ask(Request::LinkStatus).await.unwrap_or_default()
  }

  pub fn link_cancel(&self) {
    drop(self.requests.send(Request::LinkCancel));
  }

  /// Existing device: joins the new device's link with its code. The short code is shown only on the
  /// new device; the user has to type it into [`Self::approval_confirm`].
  pub async fn approval_start(&self, code: String) -> Result<(), String> {
    self
      .ask(|reply| Request::ApprovalStart { code, reply })
      .await
      .unwrap_or_else(|| Err(stopped()))
  }

  /// Existing device: hands the keys over if `short_code` is the code the new device shows. The check
  /// is here, not in the UI: a script in the webview cannot approve a link it did not see on the
  /// new device (docs/security-review.md C3).
  pub async fn approval_confirm(&self, short_code: String) -> Result<(), String> {
    self
      .ask(|reply| Request::ApprovalConfirm { short_code, reply })
      .await
      .unwrap_or_else(|| Err(stopped()))
  }

  pub fn approval_cancel(&self) {
    drop(self.requests.send(Request::ApprovalCancel));
  }

  pub async fn prepare_call(&self, conversation: ConversationId) -> Result<CallPreparation, String> {
    self
      .ask(|reply| Request::PrepareCall(conversation, reply))
      .await
      .unwrap_or_else(|| Err(stopped()))
  }

  /// A fresh join token for the conversation's call, to rejoin after a lost connection.
  pub async fn call_join_token(&self, conversation: ConversationId) -> Result<CallJoinTokenResponse, String> {
    self
      .ask(|reply| Request::CallJoinToken(conversation, reply))
      .await
      .unwrap_or_else(|| Err(stopped()))
  }

  /// One of the user's own background pictures, downloaded and decrypted.
  pub async fn background_picture(&self, id: BackgroundId) -> Result<Vec<u8>, String> {
    self
      .ask(|reply| Request::BackgroundPicture(id, reply))
      .await
      .unwrap_or_else(|| Err(stopped()))
  }

  /// An organization background, checked against the hash in the verified workspace policy.
  pub async fn org_background_picture(&self, id: OrgBackgroundId) -> Result<Vec<u8>, String> {
    self
      .ask(|reply| Request::OrgBackgroundPicture(id, reply))
      .await
      .unwrap_or_else(|| Err(stopped()))
  }

  async fn ask<T>(&self, build: impl FnOnce(oneshot::Sender<T>) -> Request) -> Option<T> {
    let (reply, answer) = oneshot::channel();
    self.requests.send(build(reply)).ok()?;
    answer.await.ok()
  }
}

impl Drop for CoreRuntime {
  fn drop(&mut self) {
    // Closing the channel ends the loop; wait so the database is closed before a new core opens it.
    let (closed, _) = mpsc::unbounded_channel();
    drop(std::mem::replace(&mut self.requests, closed));
    let maybe_thread = match self.thread.get_mut() {
      Ok(slot) => slot.take(),
      Err(poisoned) => poisoned.into_inner().take(),
    };
    if let Some(thread) = maybe_thread {
      drop(thread.join());
    }
  }
}

fn stopped() -> String {
  "the core is not running".to_owned()
}

struct Core {
  config: CoreConfig,
  user: UserId,
  key: KeyChoice,
  tokens: TokenCache,
  on_change: Arc<dyn Fn() + Send + Sync>,
  on_call_event: CallEventHandler,
  store: Option<Store>,
  /// Keeps a second process off the store while this one has it open (e2ee.md §9.4).
  store_lock: Option<StoreLock>,
  client: Option<Client<Api, Store>>,
  backend: Option<CoreChatBackend<Store>>,
  connection: ConnectionSupervisor<Gateway>,
  status: CoreStatus,
  upkeep: Upkeep,
  link: Option<LinkRun>,
  /// Why the last link failed, until the user tries again.
  link_failure: LinkStatus,
  approval: Option<LinkApproval<<Api as ServerApi>::Rendezvous>>,
  approval_misses: u8,
  /// The last attention report, repeated after a reconnect (the server forgets it with the
  /// connection).
  maybe_attention: Option<bool>,
  /// Set while opening waits for an access token.
  maybe_open_retry: Option<OpenRetry>,
}

/// Why the client could not be opened.
enum OpenError {
  /// No access token yet: retried, the user is not stuck.
  NoToken(String),
  Failed(String),
}

/// The next attempt to open the client without an access token.
struct OpenRetry {
  attempt: u32,
  at: tokio::time::Instant,
}

impl OpenRetry {
  fn after(maybe_previous: Option<&Self>) -> Self {
    let attempt = maybe_previous.map_or(0, |previous| previous.attempt.saturating_add(1));
    let delay = OPEN_RETRY_FIRST
      .saturating_mul(2u32.saturating_pow(attempt.min(16)))
      .min(OPEN_RETRY_MAX);
    Self {
      attempt,
      at: tokio::time::Instant::now() + delay,
    }
  }
}

/// A link in progress on this (new) device.
struct LinkRun {
  device_name: String,
  view: Rc<RefCell<LinkStatus>>,
  future: Pin<Box<dyn Future<Output = Result<LinkedSecrets, String>>>>,
}

impl Core {
  fn new(
    config: CoreConfig,
    user: UserId,
    key: KeyChoice,
    tokens: TokenCache,
    on_change: Arc<dyn Fn() + Send + Sync>,
    on_call_event: CallEventHandler,
    status: ConnectionStatus,
  ) -> Self {
    let mut connection = ConnectionSupervisor::new(ConnectionSettings::default(), status);
    let notify = on_change.clone();
    connection.set_observer(Box::new(move |_| notify()));
    Self {
      config,
      user,
      key,
      tokens,
      on_change,
      on_call_event,
      store: None,
      store_lock: None,
      client: None,
      backend: None,
      connection,
      status: CoreStatus::NotStarted,
      upkeep: Upkeep::new(ACCOUNT_SYNC),
      link: None,
      link_failure: LinkStatus::Idle,
      approval: None,
      approval_misses: 0,
      maybe_attention: None,
      maybe_open_retry: None,
    }
  }

  async fn run(mut self, mut requests: mpsc::UnboundedReceiver<Request>) {
    self.open().await;
    let mut flush = tokio::time::interval(FLUSH_INTERVAL);
    loop {
      self.connect().await;
      let maybe_deadline = self.connection_deadline();
      tokio::select! {
        request = requests.recv() => match request {
          Some(Request::Shutdown) | None => break,
          Some(request) => self.handle(request).await,
        },
        frame = self.connection.receive() => self.on_frame(frame).await,
        _ = flush.tick(), if self.client.is_some() => {
          self.connection.on_tick(SystemClock.now_ms()).await;
          self.upkeep().await;
        },
        () = sleep_for(maybe_deadline), if maybe_deadline.is_some() => {
          self.connection.on_tick(SystemClock.now_ms()).await;
        },
        linked = poll_link(&mut self.link), if self.link.is_some() => self.on_linked(linked).await,
        () = sleep_until_retry(self.maybe_open_retry.as_ref()), if self.maybe_open_retry.is_some() => {
          self.open().await;
          self.changed();
        },
      }
    }
  }

  /// Opens the database and the client of an already enrolled device. Without an access token yet
  /// (sign-in still settling) it stays `NotStarted` and tries again soon, never a sticky `Failed`.
  async fn open(&mut self) {
    match self.try_open().await {
      Ok(status) => {
        self.status = status;
        self.maybe_open_retry = None;
      },
      Err(OpenError::NoToken(message)) => {
        let retry = OpenRetry::after(self.maybe_open_retry.as_ref());
        tracing::info!(%message, attempt = retry.attempt, "no access token yet; opening the client again soon");
        self.status = CoreStatus::NotStarted;
        self.maybe_open_retry = Some(retry);
      },
      Err(OpenError::Failed(message)) => {
        tracing::error!(%message, "could not open the local client");
        self.status = CoreStatus::Failed { message };
        self.maybe_open_retry = None;
      },
    }
  }

  async fn try_open(&mut self) -> Result<CoreStatus, OpenError> {
    std::fs::create_dir_all(&self.config.data_dir).map_err(|error| OpenError::Failed(error.to_string()))?;
    let path = self.config.data_dir.join(format!("{}.db", self.user));
    if self.store_lock.is_none() {
      self.store_lock = Some(lock_store(&path).await.map_err(OpenError::Failed)?);
    }
    let store = match &self.store {
      Some(store) => store.clone(),
      None => {
        let store = Arc::new(
          SqlCipherStore::open(&path, &self.key).map_err(|error| OpenError::Failed(error.to_string()))?,
        );
        self.store = Some(store.clone());
        store
      },
    };
    match Client::open(
      self.api(),
      store,
      Box::new(SystemClock),
      ClientSettings::default(),
    )
    .map_err(|e| OpenError::Failed(e.to_string()))?
    {
      Some(client) => {
        let status = self.adopt(client);
        Ok(status)
      },
      None => {
        let has_existing_identity = match self.api().identity_key(self.user).await {
          Ok(_) => true,
          Err(error) if error.is_not_found() => false,
          Err(error @ ApiError::Token(_)) => return Err(OpenError::NoToken(error.to_string())),
          Err(error) => return Err(OpenError::Failed(error.to_string())),
        };
        Ok(CoreStatus::NeedsEnrollment {
          has_existing_identity,
          was_data_lost: false,
        })
      },
    }
  }

  /// Another device may have enrolled the account since the database was opened; the enrollment
  /// screen must then offer recovery and linking instead of a new identity.
  async fn refresh_enrollment_hint(&mut self) {
    if self.client.is_none()
      && self.link.is_none()
      && let CoreStatus::NeedsEnrollment { .. } = self.status
      && let Ok(has_existing_identity) = match self.api().identity_key(self.user).await {
        Ok(_) => Ok(true),
        Err(error) if error.is_not_found() => Ok(false),
        Err(error) => Err(error),
      }
    {
      self.status = CoreStatus::NeedsEnrollment {
        has_existing_identity,
        was_data_lost: false,
      };
    }
  }

  fn adopt(&mut self, mut client: Client<Api, Store>) -> CoreStatus {
    let on_call_event = self.on_call_event.clone();
    client.set_call_event_sink(Box::new(move |event| on_call_event(event.clone())));
    let status = CoreStatus::Ready {
      identity_fingerprint: client.identity_fingerprint(),
    };
    self.backend = Some(CoreChatBackend::new(
      client.store().clone(),
      client.user_id(),
      Box::new(SystemClock),
    ));
    self.client = Some(client);
    status
  }

  fn api(&self) -> Api {
    HttpServerApi::new(
      self.config.server_url.clone(),
      self.tokens.for_server(&self.config.server_url),
    )
  }

  async fn handle(&mut self, request: Request) {
    match request {
      Request::Status(reply) => {
        // Asked for the status (the UI starting the core): try at once, the token may be there now.
        if self.maybe_open_retry.is_some() {
          self.open().await;
        }
        self.refresh_enrollment_hint().await;
        drop(reply.send(self.status.clone()));
      },
      Request::Enroll(request, reply) => {
        let result = self.enroll(request).await;
        if result.is_ok() {
          self.changed();
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
      Request::LinkCancel => {
        self.link = None;
        self.link_failure = LinkStatus::Idle;
      },
      Request::ApprovalStart { code, reply } => drop(reply.send(self.approval_start(&code).await)),
      Request::ApprovalConfirm { short_code, reply } => {
        drop(reply.send(self.approval_confirm(&short_code).await))
      },
      Request::ApprovalCancel => self.approval = None,
      Request::NetworkChanged => self.connection.network_changed(SystemClock.now_ms()).await,
      // Handled by the loop in `run`.
      Request::Shutdown => {},
      Request::Snapshot(reply) => drop(reply.send(self.snapshot())),
      Request::UpdatePolicy(reply) => drop(
        reply.send(
          self
            .client
            .as_ref()
            .and_then(|client| teamcraft_core::workspace::verified_client_update_policy(client.store())),
        ),
      ),
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
          .ok_or_else(|| "this device is not set up yet".to_owned());
        drop(reply.send(result));
      },
      Request::Action(action, reply) => {
        let result = self.action(*action).await;
        self.changed();
        drop(reply.send(result));
      },
      Request::Query(query, reply) => {
        let result = self
          .backend
          .as_ref()
          .map(|backend| query.answer(backend))
          .ok_or_else(|| "this device is not set up yet".to_owned());
        drop(reply.send(result));
        // A read can ask for data it doesn't have yet (messages found only in shared index
        // segments, last seen, the call history): tell the UI to read again when it arrived.
        if self.run_queued_commands().await > 0 {
          self.changed();
        }
      },
      Request::PrepareCall(conversation, reply) => drop(reply.send(self.prepare_call(conversation).await)),
      Request::CallJoinToken(conversation, reply) => {
        drop(reply.send(self.call_join_token(conversation).await))
      },
      Request::BackgroundPicture(id, reply) => drop(reply.send(self.background_picture(id).await)),
      Request::OrgBackgroundPicture(id, reply) => drop(reply.send(self.org_background_picture(id).await)),
    }
  }

  async fn background_picture(&mut self, id: BackgroundId) -> Result<Vec<u8>, String> {
    let client = self.client.as_mut().ok_or("this device is not set up yet")?;
    let picture = client.background_picture(id).await.map_err(describe)?;
    Ok(picture.expose_secret().to_vec())
  }

  async fn org_background_picture(&mut self, id: OrgBackgroundId) -> Result<Vec<u8>, String> {
    let client = self.client.as_mut().ok_or("this device is not set up yet")?;
    client.org_background_picture(id).await.map_err(describe)
  }

  async fn call_join_token(&mut self, conversation: ConversationId) -> Result<CallJoinTokenResponse, String> {
    let client = self.client.as_mut().ok_or("this device is not set up yet")?;
    client.call_join_token(conversation).await.map_err(describe)
  }

  async fn prepare_call(&mut self, conversation: ConversationId) -> Result<CallPreparation, String> {
    let client = self.client.as_mut().ok_or("this device is not set up yet")?;
    let join = client.call_join_token(conversation).await.map_err(describe)?;
    let devices = client
      .verified_call_devices(conversation)
      .await
      .map_err(describe)?;
    Ok(CallPreparation {
      join,
      user_id: client.user_id(),
      device_id: client.device_id(),
      signer: client.call_key_signer(),
      devices,
    })
  }

  async fn enroll(&mut self, request: EnrollRequest) -> Result<(), String> {
    if self.client.is_some() {
      return Ok(());
    }
    let store = self.store.clone().ok_or("the local database is not open")?;
    let options = EnrollmentOptions {
      device_name: request.device_name,
      secret: EnrollmentSecret::Passphrase(SecretBytes::new(request.passphrase.into_bytes())),
      argon2: self.config.argon2,
      maybe_attestation: None,
      // The desktop flow only offers passphrase enrollment so far; linking and org recovery follow.
    };
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
    self.status = self.adopt(client);
    Ok(())
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
    let future = Box::pin(link_task(self.api(), offer, view.clone(), self.on_change.clone()));
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
      Ok(secrets) => self.enroll_linked(run.device_name, secrets).await,
      Err(message) => Err(message),
    };
    match outcome {
      Ok(()) => self.link_failure = LinkStatus::Idle,
      Err(message) => self.link_failure = LinkStatus::Failed { message },
    }
    self.changed();
  }

  async fn enroll_linked(&mut self, device_name: String, secrets: LinkedSecrets) -> Result<(), String> {
    let store = self.store.clone().ok_or("the local database is not open")?;
    let options = EnrollmentOptions {
      device_name,
      secret: EnrollmentSecret::Linked(secrets),
      argon2: self.config.argon2,
      maybe_attestation: None,
    };
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
    self.status = self.adopt(client);
    Ok(())
  }

  /// Existing device: joins the new device's link; the user compares the short code.
  async fn approval_start(&mut self, code: &str) -> Result<(), String> {
    let client = self.client.as_ref().ok_or("this device is not set up yet")?;
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
    let client = self.client.as_mut().ok_or("this device is not set up yet")?;
    client.complete_link_approval(approval).await.map_err(describe)
  }

  fn snapshot(&self) -> Result<ChatSnapshot, String> {
    let backend = self.backend.as_ref().ok_or("this device is not set up yet")?;
    Ok(ChatSnapshot::read(backend, self.security_snapshot()?))
  }

  /// Fingerprints of the keys this device pinned, and of keys the server showed since.
  fn security_snapshot(&self) -> Result<SecuritySnapshot, String> {
    let client = self.client.as_ref().ok_or("this device is not set up yet")?;
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
    let backend = self.backend.as_ref().ok_or("this device is not set up yet")?;
    Ok(MessageWindow::read(backend, conversation, start, len))
  }

  async fn action(&mut self, action: ChatAction) -> Result<(), String> {
    let client = self.client.as_mut().ok_or("this device is not set up yet")?;
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
      ChatAction::OpenGroupDm { users } => {
        return client.open_group_dm(users).await.map(|_| ()).map_err(describe);
      },
      ChatAction::AddMember { conversation, user } => {
        return client.add_member(*conversation, *user).await.map_err(describe);
      },
      ChatAction::ChangePassphrase { passphrase } => {
        let passphrase = SecretBytes::new(passphrase.clone().into_bytes());
        return client
          .upload_key_bag_with(&passphrase, self.config.argon2)
          .await
          .map_err(describe);
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
        return client.accept_identity_change(*user).map_err(describe);
      },
      ChatAction::PublishPolicy {
        base,
        policy,
        owners,
        admins,
      } => {
        return publish_policy(client, base, policy.clone(), owners, admins).await;
      },
      ChatAction::PublishRoster {
        base_version,
        members,
      } => {
        let roles = teamcraft_core::workspace::roster_roles(members);
        return client
          .publish_roster_based_on(*base_version, &roles)
          .await
          .map_err(describe);
      },
      ChatAction::AdminJoinChannel { conversation } => {
        let me = client.user_id();
        return client.admin_add_member(*conversation, me).await.map_err(describe);
      },
      _ => {},
    }
    let backend = self.backend.as_ref().ok_or("this device is not set up yet")?;
    match action {
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
      // Everything else goes to the backend, which queues what needs the engine as commands.
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

  /// Runs what the UI adapter queued; returns how many commands there were.
  async fn run_queued_commands(&mut self) -> usize {
    let commands: Vec<CoreCommand> = self
      .backend
      .as_ref()
      .map(CoreChatBackend::drain_commands)
      .unwrap_or_default();
    let Some(client) = self.client.as_mut() else {
      return 0;
    };
    let count = commands.len();
    for command in commands {
      if let Err(error) = client.run_ui_command(command).await {
        tracing::warn!(%error, "UI command failed");
      }
    }
    count
  }

  async fn flush(&mut self) {
    let Some(client) = self.client.as_mut() else {
      return;
    };
    if upkeep::flush(client).await {
      self.changed();
    }
  }

  /// The periodic work besides the gateway, the same as in the other apps (teamcraft-core upkeep).
  async fn upkeep(&mut self) {
    let Some(client) = self.client.as_mut() else {
      return;
    };
    if self.upkeep.tick(client).await {
      self.changed();
    }
  }

  async fn connect(&mut self) {
    let Some(client) = self.client.as_mut() else {
      return;
    };
    if self.connection.connect_if_due(client).await.is_some() {
      self.repeat_attention().await;
      self.changed();
    }
  }

  /// How long until the connection needs the loop (`None`: it doesn't, or there is no client yet).
  fn connection_deadline(&self) -> Option<Duration> {
    self.client.as_ref()?;
    let deadline_ms = self.connection.next_deadline_ms()?;
    let wait_ms = deadline_ms.saturating_sub(SystemClock.now_ms()).max(0);
    Some(Duration::from_millis(wait_ms.unsigned_abs()))
  }

  async fn on_frame(&mut self, frame: Option<Result<teamcraft_proto::GatewayServerMessage, ApiError>>) {
    let Some(client) = self.client.as_mut() else {
      return;
    };
    if self.connection.on_frame(client, frame).await {
      self.changed();
    }
  }

  /// Tells the server whether this device is idle (docs/gateway.md); a failure ends the connection
  /// like a failed heartbeat.
  async fn send_activity(&mut self, is_idle: bool) {
    let activity = if is_idle {
      DeviceActivity::Idle
    } else {
      DeviceActivity::Active
    };
    let message = GatewayClientMessage::SetActivity { activity };
    if self.connection.is_connected() && !self.connection.send(SystemClock.now_ms(), &message).await {
      tracing::warn!("could not report the device activity");
    }
  }

  /// Tells the server whether the user is looking at this session (notifications.md, "Attention").
  /// Remembered so a new connection repeats it.
  async fn send_attention(&mut self, is_attentive: bool) {
    self.maybe_attention = Some(is_attentive);
    let message = GatewayClientMessage::Attention { is_attentive };
    if self.connection.is_connected() && !self.connection.send(SystemClock.now_ms(), &message).await {
      tracing::warn!("could not report the attention");
    }
  }

  async fn repeat_attention(&mut self) {
    if let Some(is_attentive) = self.maybe_attention {
      self.send_attention(is_attentive).await;
    }
  }

  fn changed(&self) {
    (self.on_change)();
  }
}

/// Sleeps until the next attempt to open the client (only polled when there is one).
async fn sleep_until_retry(maybe_retry: Option<&OpenRetry>) {
  match maybe_retry {
    Some(retry) => tokio::time::sleep_until(retry.at).await,
    None => std::future::pending().await,
  }
}

/// Sleeps `maybe_duration` (only polled when there is one).
async fn sleep_for(maybe_duration: Option<Duration>) {
  match maybe_duration {
    Some(duration) => tokio::time::sleep(duration).await,
    None => std::future::pending().await,
  }
}

/// Error text for the UI: the cause, without internal type names.
fn describe(error: CoreError) -> String {
  error.to_string()
}

/// Signs and publishes a policy version. A workspace without a signed roster gets one first, with
/// the signer as owner (the root of trust, e2ee.md §11); later policy changes only append.
async fn publish_policy(
  client: &mut Client<Api, Store>,
  base: &WorkspacePolicy,
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
  client
    .publish_policy_based_on(base, policy)
    .await
    .map_err(describe)
}

/// Waits for the existing device to answer, shows the short code, then waits for the keys.
async fn link_task(
  api: Api,
  offer: LinkOffer,
  view: Rc<RefCell<LinkStatus>>,
  on_change: Arc<dyn Fn() + Send + Sync>,
) -> Result<LinkedSecrets, String> {
  let link = NewDeviceLink::connect(&api, &offer).await.map_err(describe)?;
  *view.borrow_mut() = LinkStatus::Confirm {
    code: offer.to_code(),
    short_code: link.short_code(),
  };
  on_change();
  link.receive_secrets().await.map_err(describe)
}

async fn poll_link(link: &mut Option<LinkRun>) -> Result<LinkedSecrets, String> {
  match link {
    Some(run) => run.future.as_mut().await,
    None => std::future::pending().await,
  }
}

/// Takes the store's lock; another instance of the app holding it is an error the user sees.
async fn lock_store(path: &std::path::Path) -> Result<StoreLock, String> {
  for _ in 1..STORE_LOCK_ATTEMPTS {
    match StoreLock::acquire(path) {
      Err(StoreError::InUse) => tokio::time::sleep(STORE_LOCK_RETRY).await,
      outcome => return outcome.map_err(|error| error.to_string()),
    }
  }
  StoreLock::acquire(path).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_core_never_gets_the_token_of_another_server() {
    let cache = TokenCache::default();
    let tokens = cache.for_server("https://a.example.com");
    assert!(tokens.access_token().is_err());
    cache.set(Some(("https://b.example.com".into(), "token-b".into())));
    assert!(tokens.access_token().is_err());
    cache.set(Some(("https://a.example.com".into(), "token-a".into())));
    assert_eq!(tokens.access_token().ok().as_deref(), Some("token-a"));
  }

  mod live_source {
    use std::sync::atomic::{AtomicBool, Ordering};

    use secrecy::SecretString;

    use super::*;

    const SERVER: &str = "https://a.example.com";

    #[test]
    fn a_busy_session_falls_back_to_a_token_that_is_still_valid() {
      let cache = cache_with(Duration::from_secs(60));
      let tokens = cache.for_server(SERVER);

      assert_eq!(tokens.access_token().ok().as_deref(), Some("token-a"));
      assert_eq!(tokens.access_token().ok().as_deref(), Some("token-a"), "busy");
    }

    #[test]
    fn a_busy_session_never_falls_back_to_an_expired_token() {
      let cache = cache_with(Duration::ZERO);
      let tokens = cache.for_server(SERVER);

      assert!(tokens.access_token().is_ok());
      assert!(
        tokens.access_token().is_err(),
        "busy, and the token seen last has run out"
      );
    }

    /// Ready with a token valid for `valid_for` once, busy afterwards.
    fn cache_with(valid_for: Duration) -> TokenCache {
      let was_read = Arc::new(AtomicBool::new(false));
      TokenCache {
        last: Arc::default(),
        maybe_source: Some(Arc::new(move || {
          if was_read.swap(true, Ordering::SeqCst) {
            TokenPeek::Busy
          } else {
            TokenPeek::Ready {
              server_url: SERVER.into(),
              token: SecretString::from("token-a"),
              valid_for,
            }
          }
        })),
      }
    }
  }
}
