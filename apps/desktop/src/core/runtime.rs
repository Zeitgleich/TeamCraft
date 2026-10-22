use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::model::{Message, ThreadId};
use teamcraft_client_api::remote::{ChatAction, ChatSnapshot, CoreStatus, EnrollRequest, MessageWindow};
use teamcraft_core::api::{AccessTokenSource, ApiError, GatewayConnection, HttpServerApi, ServerApi};
use teamcraft_core::clock::SystemClock;
use teamcraft_core::store::{DatabaseKeySource, KeySourceError, KeyringKeySource, SqlCipherStore};
use teamcraft_core::ui_backend::{CoreChatBackend, CoreCommand};
use teamcraft_core::{
  CallKeySigner, Client, ClientSettings, CoreError, EnrollmentOptions, PendingEnrollment,
};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_crypto::identity::VerifiedDevice;
use teamcraft_crypto::secret::{SecretBytes, SymmetricKey};
use teamcraft_proto::{
  CallJoinTokenResponse, ConversationId, DeviceId, GatewayClientMessage, GatewayEvent, UserId,
};
use tokio::sync::{mpsc, oneshot};

/// How often the gateway is kept alive (the server announces its own limit in `hello`).
const HEARTBEAT: Duration = Duration::from_secs(25);
/// Outbox retries while offline or after a failed send.
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);
const RECONNECT_MIN: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(30);

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

/// The newest access token, kept fresh by the login code and read synchronously by the core.
#[derive(Clone, Default)]
pub struct TokenCache(Arc<Mutex<Option<String>>>);

impl TokenCache {
  pub fn set(&self, token: Option<String>) {
    if let Ok(mut slot) = self.0.lock() {
      *slot = token;
    }
  }
}

impl AccessTokenSource for TokenCache {
  fn access_token(&self) -> Result<String, ApiError> {
    self
      .0
      .lock()
      .ok()
      .and_then(|slot| slot.clone())
      .ok_or_else(|| ApiError::Token("not signed in".to_owned()))
  }
}

/// A database key that comes from the OS keystore, or a fixed one in tests.
pub enum KeyChoice {
  Keyring {
    service: String,
    account: String,
  },
  /// A fixed key (dev-stack tests).
  #[cfg(all(test, feature = "integration"))]
  Fixed(Vec<u8>),
}

impl DatabaseKeySource for KeyChoice {
  fn database_key(&self) -> Result<SymmetricKey, KeySourceError> {
    match self {
      Self::Keyring { service, account } => KeyringKeySource::new(service, account).database_key(),
      #[cfg(all(test, feature = "integration"))]
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

type Api = HttpServerApi<TokenCache>;
type Gateway = <Api as ServerApi>::Gateway;
type Store = Arc<SqlCipherStore>;

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
  PrepareCall(ConversationId, oneshot::Sender<Result<CallPreparation, String>>),
}

/// Handle to the core thread. Dropping it stops the thread.
pub struct CoreRuntime {
  requests: mpsc::UnboundedSender<Request>,
  thread: Option<std::thread::JoinHandle<()>>,
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
        runtime.block_on(Core::new(config, user, key, tokens, on_change, on_call_event).run(receiver));
      })
      .ok();
    Self { requests, thread }
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
      .ask(|reply| Request::Action(action, reply))
      .await
      .unwrap_or_else(|| Err(stopped()))
  }

  pub async fn prepare_call(&self, conversation: ConversationId) -> Result<CallPreparation, String> {
    self
      .ask(|reply| Request::PrepareCall(conversation, reply))
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
    if let Some(thread) = self.thread.take() {
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
  client: Option<Client<Api, Store>>,
  backend: Option<CoreChatBackend<Store>>,
  gateway: Option<Gateway>,
  status: CoreStatus,
  backoff: Duration,
  reconnect_at: tokio::time::Instant,
}

impl Core {
  fn new(
    config: CoreConfig,
    user: UserId,
    key: KeyChoice,
    tokens: TokenCache,
    on_change: Arc<dyn Fn() + Send + Sync>,
    on_call_event: CallEventHandler,
  ) -> Self {
    Self {
      config,
      user,
      key,
      tokens,
      on_change,
      on_call_event,
      store: None,
      client: None,
      backend: None,
      gateway: None,
      status: CoreStatus::NotStarted,
      backoff: RECONNECT_MIN,
      reconnect_at: tokio::time::Instant::now(),
    }
  }

  async fn run(mut self, mut requests: mpsc::UnboundedReceiver<Request>) {
    self.open().await;
    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    let mut flush = tokio::time::interval(FLUSH_INTERVAL);
    loop {
      let is_connected = self.gateway.is_some();
      let wants_connection = self.client.is_some() && !is_connected;
      tokio::select! {
        request = requests.recv() => match request {
          Some(request) => self.handle(request).await,
          None => break,
        },
        frame = receive(&mut self.gateway), if is_connected => self.on_frame(frame).await,
        _ = heartbeat.tick(), if is_connected => self.send_heartbeat().await,
        _ = flush.tick(), if self.client.is_some() => self.flush().await,
        () = tokio::time::sleep_until(self.reconnect_at), if wants_connection => self.connect().await,
      }
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
    std::fs::create_dir_all(&self.config.data_dir).map_err(|error| error.to_string())?;
    let path = self.config.data_dir.join(format!("{}.db", self.user));
    let store = Arc::new(SqlCipherStore::open(&path, &self.key).map_err(|error| error.to_string())?);
    self.store = Some(store.clone());
    match Client::open(
      self.api(),
      store,
      Box::new(SystemClock),
      ClientSettings::default(),
    )
    .map_err(|e| e.to_string())?
    {
      Some(client) => {
        let status = self.adopt(client);
        Ok(status)
      },
      None => {
        let has_existing_identity = match self.api().identity_key(self.user).await {
          Ok(_) => true,
          Err(error) if error.is_not_found() => false,
          Err(error) => return Err(error.to_string()),
        };
        Ok(CoreStatus::NeedsEnrollment {
          has_existing_identity,
        })
      },
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
    self.reconnect_at = tokio::time::Instant::now();
    status
  }

  fn api(&self) -> Api {
    HttpServerApi::new(self.config.server_url.clone(), self.tokens.clone())
  }

  async fn handle(&mut self, request: Request) {
    match request {
      Request::Status(reply) => drop(reply.send(self.status.clone())),
      Request::Enroll(request, reply) => {
        let result = self.enroll(request).await;
        if result.is_ok() {
          self.changed();
        }
        drop(reply.send(result));
      },
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
          .ok_or_else(|| "this device is not set up yet".to_owned());
        drop(reply.send(result));
      },
      Request::Action(action, reply) => {
        let result = self.action(action).await;
        self.changed();
        drop(reply.send(result));
      },
      Request::PrepareCall(conversation, reply) => drop(reply.send(self.prepare_call(conversation).await)),
    }
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
      passphrase: SecretBytes::new(request.passphrase.into_bytes()),
      argon2: self.config.argon2,
      maybe_attestation: None,
      // The desktop flow only offers passphrase enrollment so far; linking and org recovery follow.
      maybe_org_recovery: None,
      maybe_linked: None,
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

  fn snapshot(&self) -> Result<ChatSnapshot, String> {
    let backend = self.backend.as_ref().ok_or("this device is not set up yet")?;
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
    })
  }

  fn window(&self, conversation: ConversationId, start: usize, len: usize) -> Result<MessageWindow, String> {
    let backend = self.backend.as_ref().ok_or("this device is not set up yet")?;
    Ok(MessageWindow {
      conversation,
      start,
      total: backend.message_count(conversation),
      messages: backend.messages(conversation, start, len),
    })
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
      ChatAction::AddMember { conversation, user } => {
        return client.add_member(*conversation, *user).await.map_err(describe);
      },
      _ => {},
    }
    let backend = self.backend.as_ref().ok_or("this device is not set up yet")?;
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
      ChatAction::CreateChannel { .. } | ChatAction::OpenDm { .. } | ChatAction::AddMember { .. } => {},
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
    match client.flush_outbox().await {
      Ok(report) if report.sent > 0 => self.changed(),
      Ok(_) => {},
      Err(error) => tracing::debug!(%error, "outbox flush did not finish"),
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
        self.changed();
      },
      Err(error) => {
        tracing::warn!(%error, "gateway connection failed, retrying");
        self.schedule_reconnect();
      },
    }
  }

  fn schedule_reconnect(&mut self) {
    self.gateway = None;
    self.reconnect_at = tokio::time::Instant::now() + self.backoff;
    self.backoff = (self.backoff * 2).min(RECONNECT_MAX);
  }

  async fn on_frame(&mut self, frame: Option<Result<teamcraft_proto::GatewayServerMessage, ApiError>>) {
    let outcome = match (frame, self.client.as_mut()) {
      (Some(Ok(frame)), Some(client)) => client.handle_frame(frame).await,
      (Some(Err(error)), _) => Err(CoreError::Api(error)),
      (None, _) => Err(CoreError::Gateway("connection closed".to_owned())),
      (_, None) => return,
    };
    match outcome {
      Ok(()) => self.changed(),
      Err(error) => {
        tracing::warn!(%error, "gateway ended");
        self.schedule_reconnect();
      },
    }
  }

  async fn send_heartbeat(&mut self) {
    let Some(connection) = self.gateway.as_mut() else {
      return;
    };
    if let Err(error) = connection.send(&GatewayClientMessage::Heartbeat).await {
      tracing::warn!(%error, "heartbeat failed");
      self.schedule_reconnect();
    }
  }

  fn changed(&self) {
    (self.on_change)();
  }
}

async fn receive(
  gateway: &mut Option<Gateway>,
) -> Option<Result<teamcraft_proto::GatewayServerMessage, ApiError>> {
  match gateway {
    Some(connection) => connection.receive().await,
    None => std::future::pending().await,
  }
}

/// Error text for the UI: the cause, without internal type names.
fn describe(error: CoreError) -> String {
  error.to_string()
}
