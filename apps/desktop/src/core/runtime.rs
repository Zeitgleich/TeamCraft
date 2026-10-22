use std::cell::RefCell;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::model::{Message, ThreadId};
use teamcraft_client_api::remote::{
  ChatAction, ChatAnswer, ChatQuery, ChatSnapshot, ContactKeys, CoreStatus, EnrollRequest, LinkStatus,
  MessageWindow, SecuritySnapshot,
};
use teamcraft_core::api::{AccessTokenSource, ApiError, GatewayConnection, HttpServerApi, ServerApi};
use teamcraft_core::client::{LinkApproval, LinkedSecrets, NewDeviceLink};
use teamcraft_core::clock::SystemClock;
use teamcraft_core::records;
use teamcraft_core::store::{DatabaseKeySource, KeySourceError, KeyringKeySource, SqlCipherStore};
use teamcraft_core::ui_backend::{CoreChatBackend, CoreCommand};
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
  BackgroundId, CallJoinTokenResponse, ConversationId, DeviceActivity, DeviceId, GatewayClientMessage,
  GatewayEvent, OrgBackgroundId, UserId, WorkspacePolicy,
};
use tokio::sync::{mpsc, oneshot};

/// Wrong short codes allowed before a link approval is dropped.
const MAX_APPROVAL_MISSES: u8 = 3;
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
  // Boxed: ChatAction is large and every other request would pay for its size.
  Action(Box<ChatAction>, oneshot::Sender<Result<(), String>>),
  Query(ChatQuery, oneshot::Sender<Result<ChatAnswer, String>>),
  PrepareCall(ConversationId, oneshot::Sender<Result<CallPreparation, String>>),
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
  link: Option<LinkRun>,
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
      link: None,
      link_failure: LinkStatus::Idle,
      approval: None,
      approval_misses: 0,
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
        linked = poll_link(&mut self.link), if self.link.is_some() => self.on_linked(linked).await,
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
    self.reconnect_at = tokio::time::Instant::now();
    status
  }

  fn api(&self) -> Api {
    HttpServerApi::new(self.config.server_url.clone(), self.tokens.clone())
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
        // A search can ask for messages found only in shared index segments.
        self.run_queued_commands().await;
      },
      Request::PrepareCall(conversation, reply) => drop(reply.send(self.prepare_call(conversation).await)),
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
      })
      .collect();
    Ok(SecuritySnapshot {
      my_fingerprint: client.identity_fingerprint(),
      contacts,
      maybe_push_content: Some(teamcraft_core::push_preview::push_content_mode(client.store())),
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
      | ChatAction::AddMember { .. }
      | ChatAction::ChangePassphrase { .. }
      | ChatAction::SetActivity { .. }
      | ChatAction::AcceptKeyChange { .. }
      | ChatAction::PublishPolicy { .. }
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
