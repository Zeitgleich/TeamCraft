use std::any::Any;
use std::cell::RefCell;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use secrecy::ExposeSecret;
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::model::{Message, ThreadId};
use teamcraft_client_api::remote::AttachmentPriority;
use teamcraft_client_api::remote::{
  ChatAction, ChatAnswer, ChatQuery, ChatSnapshot, ContactKeys, CoreStatus, EnrollRequest, EnrollSecret,
  LinkRequestView, LinkStatus, MessageWindow, SecuritySnapshot, StoreProblem,
};
use teamcraft_client_api::snapshot_changes::{SnapshotChanges, SnapshotCursor};
use teamcraft_core::api::{AccessTokenSource, ApiError, HttpServerApi, ServerApi};
use teamcraft_core::client::files::{BlobUpload, sweep_staged_copies};
use teamcraft_core::client::{
  CatchUpFocus, LinkAnnouncement, LinkApproval, LinkReceipt, LinkedSecrets, NewDeviceLink, ReceiptCancel,
  SIGN_OUT_REVOKE_PATIENCE,
};
use teamcraft_core::clock::{Clock, SystemClock};
use teamcraft_core::connection::{
  ConnectionSettings, ConnectionState, ConnectionStatus, ConnectionSupervisor,
};
use teamcraft_core::downloads::Downloads;
use teamcraft_core::records;
use teamcraft_core::store::{
  DatabaseKeySource, KeptBackupKey, KeySourceError, KeyringKeySource, SqlCipherStore, StoreError, StoreLock,
  TrackedStore,
};
use teamcraft_core::typing::TypingAnnouncer;
use teamcraft_core::ui_backend::{CoreChatBackend, CoreCommand};
use teamcraft_core::ui_snapshot::SnapshotCache;
use teamcraft_core::upkeep::{self, Upkeep, UploadMode};
use teamcraft_core::{
  CallPreparation, Client, ClientSettings, CoreError, EnrollmentOptions, EnrollmentSecret, LinkOffer,
  PendingEnrollment, UnreadableStore, parse_recovery_key,
};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_crypto::identity::IdentityPublicKey;
use teamcraft_crypto::secret::{SecretBytes, SymmetricKey};
use teamcraft_proto::DeviceLinkPlatform;
use teamcraft_proto::{
  BackgroundId, CallJoinTokenResponse, ClientUpdatePolicy, ConversationId, DeviceActivity, ErrorCode,
  GatewayClientMessage, GatewayEvent, MessageId, OrgBackgroundId, UserId,
};
use tokio::sync::{mpsc, oneshot};
use tokio::task::LocalSet;

use super::media::{KeptKey, MediaSlot};
use crate::auth::{AuthError, AuthService, TokenPeek, TokenStore, token_fingerprint};

/// Decoded messages the core keeps of the conversations read last (`TrackedStore` message cache,
/// issue #1000): SQLCipher decrypts and checks every page it reads, and the UI reads the open
/// conversation several times per change. About 1 KiB each in memory; a busy log channel of 30,000
/// messages fits with a few smaller conversations beside it.
const MESSAGE_CACHE_MESSAGES: usize = 60_000;
/// Wrong short codes allowed before a link approval is dropped.
const MAX_APPROVAL_MISSES: u8 = 3;
/// Retries (outbox, uploads, previews), staggered commits and the other periodic upkeep; also the
/// connection's regular check.
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);
/// How often the UI is told to read again while a file goes up, for its progress.
const TRANSFER_PROGRESS: Duration = Duration::from_millis(500);
/// How long a failed account data sync waits before it is retried; also how often the roster's age
/// is checked locally. Account data itself is read after each connection and on `key_bag_update`
/// (issue #252), not on a timer.
const UPKEEP_INTERVAL: Duration = Duration::from_secs(60);
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
  /// Where "Download" puts files without asking (the user's downloads folder); `None` refuses.
  pub maybe_download_dir: Option<PathBuf>,
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
  maybe_refresher: Option<Refresher>,
}

/// Refreshes the session after the server refused the token with the given fingerprint; see
/// [`AuthService::refresh_rejected`].
type Refresher = Arc<dyn Fn(u64) -> Pin<Box<dyn Future<Output = Result<bool, AuthError>>>> + Send + Sync>;

struct CachedToken {
  server_url: String,
  token: String,
  /// Wall clock (Unix ms), which goes on during a suspend, unlike `Instant` (issue #883). `None`
  /// for a token set by hand, which has no known expiry.
  maybe_valid_until_ms: Option<i64>,
}

impl TokenCache {
  /// Reads the current token of `service` on every request.
  pub fn live<S: TokenStore + 'static>(service: Arc<AuthService<S>>) -> Self {
    let refreshing = Arc::clone(&service);
    Self {
      last: Arc::default(),
      maybe_source: Some(Arc::new(move || service.peek_server_bearer_token())),
      maybe_refresher: Some(Arc::new(move |refused| {
        let service = Arc::clone(&refreshing);
        Box::pin(async move { service.refresh_rejected(refused).await })
      })),
    }
  }

  /// After the server refused the token with fingerprint `refused`: refreshes it once (shared by
  /// concurrent callers, never twice for one refusal). `Ok(true)` means the request is worth
  /// exactly one more try.
  pub async fn refresh_rejected(&self, refused: u64) -> Result<bool, AuthError> {
    match &self.maybe_refresher {
      Some(refresher) => refresher(refused).await,
      None => Ok(false),
    }
  }

  /// The fingerprint of the token a request sent now carries, if there is one.
  fn current_fingerprint(&self) -> Option<u64> {
    self.current().ok().map(|(_, token)| token_fingerprint(&token))
  }

  /// `(server origin, access token)`, or nothing while signed out.
  #[cfg(test)]
  pub fn set(&self, maybe_token: Option<(String, String)>) {
    self.remember(maybe_token.map(|(server_url, token)| CachedToken {
      server_url,
      token,
      maybe_valid_until_ms: None,
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

  /// `(server origin, access token)`, or why there is none. A token that is being refreshed
  /// (expired after a suspend, a refresh running or waiting for the network) is a network problem
  /// for the core, not a sign-in problem: the gateway shows "offline" and retries, queued work
  /// waits, and nothing says the session expired while it is fine (issue #883).
  fn current(&self) -> Result<(String, String), ApiError> {
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
          maybe_valid_until_ms: Some(
            SystemClock
              .now_ms()
              .saturating_add(i64::try_from(valid_for.as_millis()).unwrap_or(i64::MAX)),
          ),
        }));
        Ok((server_url, token))
      },
      Some(TokenPeek::Unavailable) => {
        self.remember(None);
        Err(not_signed_in())
      },
      Some(TokenPeek::Refreshing) => Err(waiting_for_refresh()),
      Some(TokenPeek::Expired) => {
        self.remember(None);
        Err(ApiError::SignInExpired)
      },
      Some(TokenPeek::Busy) | None => {
        let slot = self.last.lock().map_err(|_| not_signed_in())?;
        match slot.as_ref() {
          Some(cached)
            if cached
              .maybe_valid_until_ms
              .is_none_or(|until_ms| until_ms > SystemClock.now_ms()) =>
          {
            Ok((cached.server_url.clone(), cached.token.clone()))
          },
          // Seen before but run out: a refresh holds the session.
          Some(_) => Err(waiting_for_refresh()),
          None => Err(not_signed_in()),
        }
      },
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
    let (server_url, token) = self.cache.current()?;
    if server_url == self.server_url {
      Ok(token)
    } else {
      Err(not_signed_in())
    }
  }
}

fn not_signed_in() -> ApiError {
  ApiError::Token("not signed in".to_owned())
}

fn waiting_for_refresh() -> ApiError {
  ApiError::Network("waiting for a fresh access token".to_owned())
}

/// Keychain service of the account's backup key (e2ee.md §6.5.6); the account name is the
/// database key's.
const BACKUP_KEY_SERVICE: &str = "de.zeitgleich.teamcraft.backup-key";

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

impl KeyChoice {
  /// Where the account's backup key is kept next to the database key (e2ee.md §6.5.6); tests keep
  /// none, so they never touch the real keychain.
  fn kept_backup_key(&self) -> Option<KeptBackupKey> {
    match self {
      Self::Keyring { account, .. } => Some(KeptBackupKey::new(BACKUP_KEY_SERVICE, account.clone())),
      #[cfg(test)]
      Self::Fixed(_) => None,
    }
  }
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

/// Receives a failure no UI request waits for, as `ActionFailure` wire text (issue #322).
pub type FailureHandler = Arc<dyn Fn(String) + Send + Sync>;

/// How the core tells the shell about changes, failures and call events.
struct Hooks {
  on_change: Arc<dyn Fn() + Send + Sync>,
  on_failure: FailureHandler,
  on_call_event: CallEventHandler,
}

type Api = HttpServerApi<ServerTokens>;
type Gateway = <Api as ServerApi>::Gateway;
/// Notes what each write changes, so the UI's snapshot is rebuilt per conversation (issue #246).
type Store = TrackedStore<Arc<SqlCipherStore>>;

// Short-lived messages on a channel: boxing the one large variant would only add an allocation.
#[allow(clippy::large_enum_variant)]
enum Request {
  Status(oneshot::Sender<CoreStatus>),
  Enroll(EnrollRequest, oneshot::Sender<Result<(), String>>),
  Snapshot(oneshot::Sender<Result<ChatSnapshot, String>>),
  SnapshotChanges(
    Option<SnapshotCursor>,
    oneshot::Sender<Result<SnapshotChanges, String>>,
  ),
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
  CallDevices(
    ConversationId,
    oneshot::Sender<Result<Vec<teamcraft_crypto::identity::VerifiedDevice>, String>>,
  ),
  CallJoinToken(
    ConversationId,
    oneshot::Sender<Result<CallJoinTokenResponse, String>>,
  ),
  BackgroundPicture(BackgroundId, oneshot::Sender<Result<Vec<u8>, String>>),
  Attachment {
    conversation: ConversationId,
    message: MessageId,
    file_id: String,
    priority: AttachmentPriority,
    reply: oneshot::Sender<Result<SecretBytes, String>>,
  },
  OrgBackgroundPicture(OrgBackgroundId, oneshot::Sender<Result<Vec<u8>, String>>),
  /// "Save…" (#719): the file goes to the path the user picked in the save dialog.
  SaveAttachment {
    conversation: ConversationId,
    message: MessageId,
    file_id: String,
    path: PathBuf,
    reply: oneshot::Sender<Result<(), String>>,
  },
  LinkStart {
    device_name: String,
    reply: oneshot::Sender<Result<(), String>>,
  },
  LinkStatus(oneshot::Sender<LinkStatus>),
  LinkCancel,
  /// The local database is unreadable: set it aside and start this device afresh (issue #160).
  ResetStore(oneshot::Sender<Result<(), String>>),
  /// The user signs out: drop what an unfinished enrollment kept (issues #168, #415).
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
  /// The network changed or the computer woke up: check the connection now.
  NetworkChanged,
  /// Ends the core loop, closing the database and releasing the store lock.
  Shutdown,
}

/// Handle to the core thread. Dropping it stops the thread.
pub struct CoreRuntime {
  requests: mpsc::UnboundedSender<Request>,
  /// Where the webview stages large files for their upload (`super::staging`).
  staging_dir: PathBuf,
  /// Read without asking the core thread, which may be busy connecting.
  connection: ConnectionStatus,
  /// Shared with the client: the conversation the user opened, set while the core thread syncs.
  focus: CatchUpFocus,
  /// The media cache (#749), opened with the store; read without asking the core thread.
  media: MediaSlot,
  /// Taken by [`CoreRuntime::shutdown`] or on drop.
  thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl CoreRuntime {
  /// Starts the core for one signed-in user. `on_change` is called after every store change,
  /// `on_failure` with every failure the UI did not wait for, `on_call_event` with every call event
  /// from the gateway.
  pub fn spawn(
    config: CoreConfig,
    user: UserId,
    key: KeyChoice,
    tokens: TokenCache,
    on_change: Arc<dyn Fn() + Send + Sync>,
    on_failure: FailureHandler,
    on_call_event: CallEventHandler,
  ) -> Self {
    let (requests, receiver) = mpsc::unbounded_channel();
    let staging_dir = staging_dir(&config, user);
    let connection = ConnectionStatus::new();
    let status = connection.clone();
    let focus = CatchUpFocus::default();
    let shared_focus = focus.clone();
    let media = MediaSlot::default();
    let shared_media = media.clone();
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
        let hooks = Hooks {
          on_change,
          on_failure,
          on_call_event,
        };
        let core = Core::new(
          config,
          user,
          key,
          tokens,
          hooks,
          status,
          shared_focus,
          shared_media,
        );
        // A panic ends the core for good: logged here, and `status` then answers `Stopped`, so the
        // UI shows an internal error instead of a keystore problem. The local set runs what the
        // core starts beside its loop (a link receipt, issue #415) and drops it when the core ends.
        let tasks = LocalSet::new();
        let outcome =
          std::panic::catch_unwind(AssertUnwindSafe(|| tasks.block_on(&runtime, core.run(receiver))));
        if let Err(payload) = outcome {
          tracing::error!(panic = %panic_text(payload.as_ref()), "the client core panicked and stopped");
        }
      })
      .ok();
    Self {
      requests,
      staging_dir,
      connection,
      focus,
      media,
      thread: Mutex::new(thread),
    }
  }

  /// The media cache once the store is open (#749).
  pub fn media(&self) -> &MediaSlot {
    &self.media
  }

  pub fn staging_dir(&self) -> &std::path::Path {
    &self.staging_dir
  }

  /// The user opened `conversation`: a catch-up still running takes it next (issue #247). Set
  /// without the core thread, which doesn't serve requests while it syncs.
  pub fn focus(&self, conversation: ConversationId) {
    self.focus.set(conversation);
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

  /// Where the core stands; `Stopped` when its thread is gone (it panicked or was shut down).
  pub async fn status(&self) -> CoreStatus {
    self.ask(Request::Status).await.unwrap_or(CoreStatus::Stopped)
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

  pub async fn snapshot_changes(
    &self,
    maybe_since: Option<SnapshotCursor>,
  ) -> Result<SnapshotChanges, String> {
    self
      .ask(|reply| Request::SnapshotChanges(maybe_since, reply))
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

  /// The user signs out: drops the keys of an unfinished link and the pending device key, if this
  /// device isn't set up yet, and gives the server a few seconds to revoke the device that key
  /// registered (issue #415).
  pub async fn forget_unfinished_setup(&self) {
    // `None`: the core already stopped, so there is nothing to forget in it.
    let _ = self.ask(Request::ForgetUnfinishedSetup).await;
  }

  /// Sets an unreadable local database aside (renamed, not deleted) and opens a new one, so the
  /// device can be set up again (issue #160).
  pub async fn reset_store(&self) -> Result<(), String> {
    self
      .ask(Request::ResetStore)
      .await
      .unwrap_or_else(|| Err(stopped()))
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
  /// new device (docs/security-review.md C3). Answers once the new device confirmed the keys; the
  /// core goes on meanwhile (issue #415).
  pub async fn approval_confirm(&self, short_code: String) -> Result<(), String> {
    self
      .ask(|reply| Request::ApprovalConfirm { short_code, reply })
      .await
      .unwrap_or_else(|| Err(stopped()))
  }

  pub fn approval_cancel(&self) {
    drop(self.requests.send(Request::ApprovalCancel));
  }

  /// New device: asks the user's other devices to approve this one. This hands the code's key to
  /// the server, so it only happens when the user chooses it (e2ee.md §5).
  pub async fn link_announce(&self) -> Result<(), String> {
    self
      .ask(Request::LinkAnnounce)
      .await
      .unwrap_or_else(|| Err(stopped()))
  }

  /// New devices of this account that wait for a decision (e2ee.md §5, approve prompt).
  pub async fn link_requests(&self) -> Vec<LinkRequestView> {
    self.ask(Request::LinkRequests).await.unwrap_or_default()
  }

  /// Takes up a request and joins its relay; the user then types the code the new device shows
  /// into [`Self::approval_confirm`]. The other devices drop their prompt.
  pub async fn link_request_review(&self, id: String) -> Result<(), String> {
    self
      .ask(|reply| Request::LinkRequestReview { id, reply })
      .await
      .unwrap_or_else(|| Err(stopped()))
  }

  pub async fn link_request_reject(&self, id: String) -> Result<(), String> {
    self
      .ask(|reply| Request::LinkRequestReject { id, reply })
      .await
      .unwrap_or_else(|| Err(stopped()))
  }

  pub async fn prepare_call(&self, conversation: ConversationId) -> Result<CallPreparation, String> {
    self
      .ask(|reply| Request::PrepareCall(conversation, reply))
      .await
      .unwrap_or_else(|| Err(stopped()))
  }

  /// The running call's verified devices afresh: a device added to the conversation during the
  /// call joins it (e2ee.md §8.2).
  pub async fn call_devices(
    &self,
    conversation: ConversationId,
  ) -> Result<Vec<teamcraft_crypto::identity::VerifiedDevice>, String> {
    self
      .ask(|reply| Request::CallDevices(conversation, reply))
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

  /// One file of a message, downloaded and decrypted in memory (the image viewer).
  pub async fn attachment(
    &self,
    conversation: ConversationId,
    message: MessageId,
    file_id: String,
    priority: AttachmentPriority,
  ) -> Result<SecretBytes, String> {
    self
      .ask(|reply| Request::Attachment {
        conversation,
        message,
        file_id,
        priority,
        reply,
      })
      .await
      .unwrap_or_else(|| Err(stopped()))
  }

  /// Saves one file of a message to `path`, which the user picked in a save dialog. It downloads
  /// beside the loop; the file's transfer state shows the progress and where it went.
  pub async fn save_attachment(
    &self,
    conversation: ConversationId,
    message: MessageId,
    file_id: String,
    path: PathBuf,
  ) -> Result<(), String> {
    self
      .ask(|reply| Request::SaveAttachment {
        conversation,
        message,
        file_id,
        path,
        reply,
      })
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

/// Fingerprints of the keys this device pinned, and of keys the server showed since.
fn security_of(client: &Client<Api, Store>) -> Result<SecuritySnapshot, String> {
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

fn stopped() -> String {
  "the core is not running".to_owned()
}

/// The message of a panic payload (`panic!` with a literal or a formatted message).
fn panic_text(payload: &(dyn Any + Send)) -> &str {
  payload
    .downcast_ref::<&str>()
    .copied()
    .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
    .unwrap_or("non-text panic payload")
}

struct Core {
  config: CoreConfig,
  user: UserId,
  key: KeyChoice,
  tokens: TokenCache,
  on_change: Arc<dyn Fn() + Send + Sync>,
  on_failure: FailureHandler,
  on_call_event: CallEventHandler,
  store: Option<Store>,
  /// Keeps a second process off the store while this one has it open (e2ee.md §9.4).
  store_lock: Option<StoreLock>,
  client: Option<Client<Api, Store>>,
  backend: Option<CoreChatBackend<Store>>,
  connection: ConnectionSupervisor<Gateway>,
  /// Handed to every client this core adopts ([`CoreRuntime::focus`]).
  catch_up_focus: CatchUpFocus,
  status: CoreStatus,
  upkeep: Upkeep,
  link: Option<LinkRun>,
  /// Why the last link failed, until the user tries again.
  link_failure: LinkStatus,
  approval: Option<LinkApproval<<Api as ServerApi>::Rendezvous>>,
  approval_misses: u8,
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
  /// Set while opening waits for an access token.
  maybe_open_retry: Option<OpenRetry>,
  /// The unreadable database was set aside: setup explains why it is asked for again.
  was_store_set_aside: bool,
  /// Whether the OS keychain holds the account's backup key (e2ee.md §6.5.6), read once.
  maybe_has_kept_backup_key: Option<bool>,
  /// The UI's snapshot, rebuilt per conversation from what the store's writes touched (#246).
  snapshots: SnapshotCache,
  /// Messages were written to the outbox and answered at once; sending them is still to do.
  has_queued_sends: bool,
  /// The file going up beside the loop, so the UI's requests are answered meanwhile.
  maybe_upload: Option<BlobUpload>,
  /// A file may be waiting for its upload: one was queued, a retry came due, or one finished.
  is_upload_due: bool,
  /// The bytes the transfers in flight had moved when the UI was last told.
  maybe_reported_bytes: Option<u64>,
  /// Attachment downloads, running beside the loop (issue #491).
  downloads: Downloads,
  /// Filled when the store opens, with the store's key (#749).
  media: MediaSlot,
}

/// Why the client could not be opened.
enum OpenError {
  /// No access token yet: retried, the user is not stuck.
  NoToken(String),
  /// The database can't be read by this version (issue #160): the user decides what happens to it.
  Unreadable(UnreadableStore, String),
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
  // The shell shares four handles with the core thread; bundling them would only rename them.
  #[allow(clippy::too_many_arguments)]
  fn new(
    config: CoreConfig,
    user: UserId,
    key: KeyChoice,
    tokens: TokenCache,
    hooks: Hooks,
    status: ConnectionStatus,
    catch_up_focus: CatchUpFocus,
    media: MediaSlot,
  ) -> Self {
    let Hooks {
      on_change,
      on_failure,
      on_call_event,
    } = hooks;
    let mut connection = ConnectionSupervisor::new(ConnectionSettings::default(), status);
    let notify = on_change.clone();
    connection.set_observer(Box::new(move |_| notify()));
    Self {
      config,
      user,
      key,
      tokens,
      on_change,
      on_failure,
      on_call_event,
      store: None,
      store_lock: None,
      client: None,
      backend: None,
      connection,
      catch_up_focus,
      status: CoreStatus::NotStarted,
      upkeep: Upkeep::new(UPKEEP_INTERVAL).with_uploads(UploadMode::Host),
      link: None,
      link_failure: LinkStatus::Idle,
      approval: None,
      approval_misses: 0,
      maybe_receipt: None,
      maybe_attention: None,
      is_idle: false,
      typing: TypingAnnouncer::default(),
      maybe_open_retry: None,
      was_store_set_aside: false,
      maybe_has_kept_backup_key: None,
      snapshots: SnapshotCache::new(SystemClock.now_ms().unsigned_abs()),
      has_queued_sends: false,
      maybe_upload: None,
      is_upload_due: true,
      maybe_reported_bytes: None,
      downloads: Downloads::default(),
      media,
    }
  }

  async fn run(mut self, mut requests: mpsc::UnboundedReceiver<Request>) {
    self.open().await;
    let mut flush = tokio::time::interval(FLUSH_INTERVAL);
    loop {
      if self.connect(&mut requests).await == Flow::Stop {
        break;
      }
      if std::mem::take(&mut self.has_queued_sends) && self.send_queued(&mut requests).await == Flow::Stop {
        break;
      }
      let maybe_deadline = self.connection_deadline();
      let maybe_typing_change = self.typing_change();
      tokio::select! {
        request = requests.recv() => match request {
          Some(Request::Shutdown) | None => break,
          Some(request) => self.handle(request).await,
        },
        frame = self.connection.receive() => self.on_frame(frame).await,
        _ = flush.tick(), if self.client.is_some() => {
          self.connection.on_tick(SystemClock.now_ms()).await;
          self.upkeep().await;
          // Retries of failed uploads come due with time.
          self.is_upload_due = true;
        },
        sent = upload_sent(&mut self.maybe_upload), if self.maybe_upload.is_some() => {
          self.on_uploaded(sent).await;
        },
        finished = self.downloads.next_finished(), if self.downloads.is_running() => {
          if let Some(client) = self.client.as_mut()
            && Downloads::finish(client, finished)
          {
            self.changed();
          }
        },
        // While a file goes up, the UI reads its progress again every so often.
        () = tokio::time::sleep(TRANSFER_PROGRESS), if self.maybe_reported_bytes.is_some() => {},
        () = sleep_for(maybe_deadline), if maybe_deadline.is_some() => {
          self.connection.on_tick(SystemClock.now_ms()).await;
        },
        // Someone stopped typing: no event says so, the indicator runs out.
        () = sleep_for(maybe_typing_change), if maybe_typing_change.is_some() => self.drop_expired_typing(),
        linked = poll_link(&mut self.link), if self.link.is_some() => self.on_linked(linked).await,
        () = sleep_until_retry(self.maybe_open_retry.as_ref()), if self.maybe_open_retry.is_some() => {
          self.open().await;
          self.changed();
        },
      }
      if std::mem::take(&mut self.has_queued_sends) && self.send_queued(&mut requests).await == Flow::Stop {
        break;
      }
      self.start_upload().await;
      self.start_downloads().await;
      self.report_progress();
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
      (self.on_failure)(describe_failure(error));
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
      Err(OpenError::Unreadable(problem, message)) => {
        tracing::error!(%message, ?problem, "the local database is unreadable");
        self.status = CoreStatus::StoreUnreadable {
          problem: store_problem(problem),
        };
        self.maybe_open_retry = None;
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
        let key = KeptKey::new(&self.key);
        let store = TrackedStore::with_message_cache(
          Arc::new(SqlCipherStore::open(&path, &key).map_err(|error| open_error(error.into()))?),
          MESSAGE_CACHE_MESSAGES,
        );
        self.store = Some(store.clone());
        if let Some(database_key) = key.take() {
          self.media.open(&self.config.data_dir, self.user, &database_key);
        }
        store
      },
    };
    match Client::open(
      self.api(),
      store.clone(),
      Box::new(SystemClock),
      self.client_settings(),
    )
    .map_err(open_error)?
    {
      Some(client) => {
        let status = self.adopt(client);
        Ok(status)
      },
      None => {
        if let Some(status) = self.resume_link(store).await? {
          return Ok(status);
        }
        self.drop_abandoned_setup().await;
        let has_existing_identity = match self.api().identity_key(self.user).await {
          Ok(maybe_key) => maybe_key.is_some(),
          // No token yet, one waiting for its refresh, or a refused sign-in (the UI takes it from
          // there): opening is retried.
          Err(error @ (ApiError::Token(_) | ApiError::Network(_) | ApiError::SignInExpired)) => {
            return Err(OpenError::NoToken(error.to_string()));
          },
          Err(error) => return Err(OpenError::Failed(error.to_string())),
        };
        Ok(CoreStatus::NeedsEnrollment {
          has_existing_identity,
          was_data_lost: self.was_store_set_aside,
          can_restore_on_device: has_existing_identity && self.has_kept_backup_key(),
        })
      },
    }
  }

  /// A device link whose keys arrived before the app stopped finishes enrolling now (issue #168).
  async fn resume_link(&mut self, store: Store) -> Result<Option<CoreStatus>, OpenError> {
    let resumed = PendingEnrollment::resume_link(
      self.api(),
      store,
      Box::new(SystemClock),
      self.client_settings(),
      self.config.argon2,
    )
    .await;
    match resumed {
      Ok(Some(client)) => Ok(Some(self.adopt(client))),
      Ok(None) => Ok(None),
      // Kept for the next try, which comes soon.
      Err(error) if error.is_transient() || error.is_sign_in_required() => {
        Err(OpenError::NoToken(error.to_string()))
      },
      // The keys are dropped if they can't work; the user links again.
      Err(error) => {
        tracing::warn!(%error, "could not finish the enrollment of an interrupted device link");
        Ok(None)
      },
    }
  }

  /// Renames the unreadable database (and its journal or write-ahead log) beside itself and opens a
  /// new one. The old files stay for support; nothing is deleted. The log goes with its database:
  /// left behind, it would be read as the new database's.
  async fn reset_store(&mut self) -> Result<(), String> {
    if !matches!(self.status, CoreStatus::StoreUnreadable { .. }) {
      return Err("the local data is readable; nothing to reset".to_owned());
    }
    self.client = None;
    self.backend = None;
    self.store = None;
    let path = self.config.data_dir.join(format!("{}.db", self.user));
    let suffix = format!("unreadable-{}", SystemClock.now_ms());
    let companions = ["db-journal", "db-wal", "db-shm"].map(|extension| {
      (
        path.with_extension(extension),
        path.with_extension(format!("{extension}.{suffix}")),
      )
    });
    let database = (path.clone(), path.with_extension(format!("db.{suffix}")));
    for (file, aside) in std::iter::once(database).chain(companions) {
      match std::fs::rename(&file, &aside) {
        Ok(()) => tracing::warn!(path = %aside.display(), "set the unreadable local database aside"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => return Err(format!("could not set the local database aside: {error}")),
      }
    }
    self.was_store_set_aside = true;
    self.open().await;
    self.changed();
    Ok(())
  }

  /// Another device may have enrolled the account since the database was opened; the enrollment
  /// screen must then offer recovery and linking instead of a new identity. An app left on that
  /// screen also drops a setup given up on meanwhile.
  async fn refresh_enrollment_hint(&mut self) {
    if self.client.is_none()
      && self.link.is_none()
      && let CoreStatus::NeedsEnrollment { .. } = self.status
    {
      self.drop_abandoned_setup().await;
      if let Ok(maybe_key) = self.api().identity_key(self.user).await {
        let has_existing_identity = maybe_key.is_some();
        self.status = CoreStatus::NeedsEnrollment {
          has_existing_identity,
          was_data_lost: self.was_store_set_aside,
          can_restore_on_device: has_existing_identity && self.has_kept_backup_key(),
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
    let settings = self.client_settings();
    if let Err(error) = PendingEnrollment::drop_abandoned(&self.api(), store, &SystemClock, &settings).await {
      tracing::info!(%error, "could not revoke the device of a setup that was given up yet");
    }
  }

  /// Uploads read the webview's staged copies and delete them when done.
  fn client_settings(&self) -> ClientSettings {
    ClientSettings {
      maybe_upload_staging_dir: Some(staging_dir(&self.config, self.user)),
      maybe_download_dir: self.config.maybe_download_dir.clone(),
      creates_notes_to_self: true,
      ..ClientSettings::default()
    }
  }

  fn adopt(&mut self, mut client: Client<Api, Store>) -> CoreStatus {
    // Copies left by an app that stopped before their message was sent (or by a cancelled send).
    match sweep_staged_copies(client.store(), &staging_dir(&self.config, self.user)) {
      Ok(0) => {},
      Ok(removed) => tracing::info!(removed, "deleted staged upload copies nobody needs"),
      Err(error) => tracing::warn!(%error, "could not sweep the staged upload copies"),
    }
    self.keep_backup_key(&client);
    let on_call_event = self.on_call_event.clone();
    client.set_call_event_sink(Box::new(move |event| on_call_event(event.clone())));
    client.share_catch_up_focus(self.catch_up_focus.clone());
    let status = CoreStatus::Ready {
      identity_fingerprint: client.identity_fingerprint(),
    };
    self.backend = Some(
      CoreChatBackend::new(client.store().clone(), client.user_id(), Box::new(SystemClock))
        .with_typing(client.typing_state())
        .with_transfers(client.transfers()),
    );
    self.client = Some(client);
    status
  }

  /// Keeps the account's backup key in the OS keychain (e2ee.md §6.5.6), on every start when it is
  /// missing or differs. Without a keychain the recovery kit stays the way back, so a failure is
  /// only logged.
  fn keep_backup_key(&mut self, client: &Client<Api, Store>) {
    let Some(kept) = self.key.kept_backup_key() else {
      return;
    };
    match kept.keep(&client.backup_secret()) {
      Ok(()) => self.maybe_has_kept_backup_key = Some(true),
      Err(error) => tracing::warn!(%error, "could not keep the backup key in the OS keychain"),
    }
  }

  /// Whether the OS keychain holds a backup key for this account; read once per start, since the
  /// setup screen asks for the status every second.
  fn has_kept_backup_key(&mut self) -> bool {
    if let Some(known) = self.maybe_has_kept_backup_key {
      return known;
    }
    let has_key = self.key.kept_backup_key().is_some_and(|kept| match kept.load() {
      Ok(maybe_key) => maybe_key.is_some(),
      Err(error) => {
        tracing::info!(%error, "no backup key kept in the OS keychain");
        false
      },
    });
    self.maybe_has_kept_backup_key = Some(has_key);
    has_key
  }

  /// What the user chose on the setup screen, as the core takes it.
  fn enrollment_secret(&self, secret: EnrollSecret) -> Result<EnrollmentSecret, String> {
    match secret {
      EnrollSecret::RecoveryKey { recovery_key } => {
        EnrollmentSecret::recovery_key(&recovery_key).map_err(describe_failure)
      },
      EnrollSecret::Passphrase { passphrase } => Ok(EnrollmentSecret::passphrase(passphrase)),
      EnrollSecret::KeptOnDevice => {
        let kept = self
          .key
          .kept_backup_key()
          .ok_or("this computer keeps no key for the account")?;
        match kept.load() {
          Ok(Some(backup_secret)) => Ok(EnrollmentSecret::KeptBackupKey(backup_secret)),
          Ok(None) => Err("this computer keeps no key for the account".to_owned()),
          Err(error) => Err(error.to_string()),
        }
      },
      EnrollSecret::Guest => Err("meeting guests join in the browser".to_owned()),
    }
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
          // (issue #415), the next start of this account does it otherwise.
          let forgotten =
            PendingEnrollment::forget_unfinished(&self.api(), store, &SystemClock, SIGN_OUT_REVOKE_PATIENCE)
              .await;
          if let Err(error) = forgotten {
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
        // A receipt still awaited: its wait ends, and the waiting call hears that it was cancelled.
        self.maybe_receipt = None;
      },
      Request::LinkAnnounce(reply) => drop(reply.send(self.link_announce().await)),
      Request::LinkRequests(reply) => drop(reply.send(self.link_requests())),
      Request::LinkRequestReview { id, reply } => drop(reply.send(self.link_request_review(&id).await)),
      Request::LinkRequestReject { id, reply } => drop(reply.send(self.link_request_reject(&id).await)),
      Request::NetworkChanged => {
        // Pooled connections from before a suspend or on the old network are dead; a command on
        // one would hang until the HTTP/2 keep-alive gives up (issue #883).
        if let Some(client) = &self.client {
          client.api().drop_connections();
        }
        self.connection.network_changed(SystemClock.now_ms()).await
      },
      // Handled by the loop in `run`.
      Request::Shutdown => {},
      Request::Snapshot(reply) => drop(reply.send(self.snapshot())),
      Request::SnapshotChanges(maybe_since, reply) => drop(reply.send(self.snapshot_changes(maybe_since))),
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
      } => {
        // The next catch-up starts with the conversation the user looked at last (issue #247).
        if let Some(client) = &self.client {
          client.catch_up_focus().set(conversation);
        }
        drop(reply.send(self.window(conversation, start, len)));
      },
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
      // A message shows at once: it is answered when it is in the outbox and sent afterwards
      // (`send_queued`).
      Request::Action(action, reply) if action.is_new_message() => {
        let result = match self.backend.as_ref() {
          Some(backend) => {
            if let Some(unhandled) = action.apply_to(backend) {
              tracing::warn!(?unhandled, "chat action not handled");
            }
            self.has_queued_sends = true;
            Ok(())
          },
          None => Err("this device is not set up yet".to_owned()),
        };
        self.changed();
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
        // A failed read command still changed what there is to read (it was answered as failed),
        // and its failure goes to the UI too, which nobody there waits for (issue #322).
        match self.run_queued_commands().await {
          Ok(0) => {},
          Ok(_) => self.changed(),
          Err(error) => {
            self.changed();
            (self.on_failure)(describe_failure(error));
          },
        }
      },
      Request::PrepareCall(conversation, reply) => drop(reply.send(self.prepare_call(conversation).await)),
      Request::CallDevices(conversation, reply) => drop(reply.send(self.call_devices(conversation).await)),
      Request::CallJoinToken(conversation, reply) => {
        drop(reply.send(self.call_join_token(conversation).await))
      },
      Request::BackgroundPicture(id, reply) => drop(reply.send(self.background_picture(id).await)),
      Request::Attachment {
        conversation,
        message,
        file_id,
        priority,
        reply,
      } => self.queue_attachment(conversation, message, &file_id, priority, reply),
      Request::OrgBackgroundPicture(id, reply) => drop(reply.send(self.org_background_picture(id).await)),
      Request::SaveAttachment {
        conversation,
        message,
        file_id,
        path,
        reply,
      } => drop(reply.send(self.queue_save(conversation, message, &file_id, path))),
    }
  }

  /// Queues saving one file to the path the user picked; it downloads beside the loop.
  fn queue_save(
    &mut self,
    conversation: ConversationId,
    message: MessageId,
    file_id: &str,
    path: PathBuf,
  ) -> Result<(), String> {
    let reference = self
      .backend
      .as_ref()
      .and_then(|backend| backend.attachment_ref(conversation, message, file_id))
      .ok_or_else(|| "no such file in this conversation".to_owned())?;
    self
      .downloads
      .save_to(conversation, reference.attachment_id, path);
    Ok(())
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
    let deliver = move |opened: Result<SecretBytes, String>| drop(reply.send(opened));
    self.downloads.view(reference, priority.into(), Box::new(deliver));
  }

  /// Starts the downloads that may run now.
  async fn start_downloads(&mut self) {
    let Some(client) = self.client.as_mut() else {
      return;
    };
    if self
      .connection
      .keep_alive(SystemClock.now_ms(), self.downloads.start_due(client))
      .await
    {
      self.changed();
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

  async fn call_devices(
    &mut self,
    conversation: ConversationId,
  ) -> Result<Vec<teamcraft_crypto::identity::VerifiedDevice>, String> {
    let client = self.client.as_mut().ok_or("this device is not set up yet")?;
    client.refresh_call_devices(conversation).await.map_err(describe)
  }

  async fn prepare_call(&mut self, conversation: ConversationId) -> Result<CallPreparation, String> {
    let client = self.client.as_mut().ok_or("this device is not set up yet")?;
    // With its failure kind: "not in the encryption group yet" is worded and offers a retry.
    client.prepare_call(conversation).await.map_err(describe_failure)
  }

  async fn enroll(&mut self, request: EnrollRequest) -> Result<(), String> {
    if self.client.is_some() {
      return Ok(());
    }
    let store = self.store.clone().ok_or("the local database is not open")?;
    let options = EnrollmentOptions {
      device_name: request.device_name,
      secret: self.enrollment_secret(request.secret)?,
      argon2: self.config.argon2,
      maybe_attestation: None,
    };
    let client = PendingEnrollment::new()
      .complete(
        self.api(),
        store,
        Box::new(SystemClock),
        self.client_settings(),
        options,
      )
      .await
      .map_err(describe_failure)?;
    self.status = self.adopt(client);
    Ok(())
  }

  /// New device: shows a code and waits for an existing device to scan it.
  fn link_start(&mut self, device_name: String) -> Result<(), String> {
    if self.client.is_some() {
      return Err("this device is already set up".to_owned());
    }
    let store = self.store.clone().ok_or("the local database is not open")?;
    let offer = LinkOffer::generate();
    let view = Rc::new(RefCell::new(LinkStatus::WaitingForScan {
      code: offer.to_code(),
    }));
    let future = Box::pin(link_task(
      self.api(),
      store,
      device_name.clone(),
      offer,
      view.clone(),
      self.on_change.clone(),
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
        self.client_settings(),
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
    let approval = client.start_link_approval(code).await.map_err(describe_failure)?;
    self.approval = Some(approval);
    self.approval_misses = 0;
    Ok(())
  }

  async fn link_announce(&mut self) -> Result<(), String> {
    let run = self.link.as_ref().ok_or("no device link in progress")?;
    let code = match &*run.view.borrow() {
      LinkStatus::WaitingForScan { code } | LinkStatus::Confirm { code, .. } => code.clone(),
      _ => return Err("no device link in progress".to_owned()),
    };
    let announcement = LinkAnnouncement {
      device_name: run.device_name.clone(),
      platform: platform(),
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
    let client = self.client.as_mut().ok_or("this device is not set up yet")?;
    let approval = client.review_link_request(id).await.map_err(describe_failure)?;
    self.approval = Some(approval);
    self.approval_misses = 0;
    self.changed();
    Ok(())
  }

  async fn link_request_reject(&mut self, id: &str) -> Result<(), String> {
    let client = self.client.as_mut().ok_or("this device is not set up yet")?;
    client.reject_link_request(id).await.map_err(describe_failure)?;
    self.changed();
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
    let client = self.client.as_mut().ok_or("this device is not set up yet")?;
    client
      .send_link_transfer(approval)
      .await
      .map_err(describe_failure)
  }

  /// Waits for the new device's receipt on a task of its own (issue #415): it can take up to a minute
  /// when the new device went away, and the loop answers everything else meanwhile. `reply` gets
  /// the outcome, so the dialog shows its progress until then. One receipt is awaited at a time; a
  /// wait still running for an earlier link ends as cancelled. The task ends with the core.
  fn wait_for_receipt(
    &mut self,
    receipt: LinkReceipt<<Api as ServerApi>::Rendezvous>,
    reply: oneshot::Sender<Result<(), String>>,
  ) {
    let (waiting, cancel) = receipt.detach();
    self.maybe_receipt = Some(cancel);
    tokio::task::spawn_local(async move {
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
    let (Some(backend), Some(store)) = (self.backend.as_ref(), self.store.as_ref()) else {
      return Err("this device is not set up yet".to_owned());
    };
    Ok(
      self
        .snapshots
        .snapshot(backend, store.take_changes(), || Some(security)),
    )
  }

  fn snapshot_changes(&mut self, maybe_since: Option<SnapshotCursor>) -> Result<SnapshotChanges, String> {
    let Self {
      backend: Some(backend),
      store: Some(store),
      client: Some(client),
      snapshots,
      ..
    } = self
    else {
      return Err("this device is not set up yet".to_owned());
    };
    let security = || security_of(client).ok();
    Ok(snapshots.changes_since(backend, store.take_changes(), security, maybe_since))
  }

  /// Fingerprints of the keys this device pinned, and of keys the server showed since.
  fn security_snapshot(&self) -> Result<SecuritySnapshot, String> {
    security_of(self.client.as_ref().ok_or("this device is not set up yet")?)
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
          .upload_key_bag_with(&passphrase, self.config.argon2)
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
        return client.accept_identity_change(*user).map_err(describe_failure);
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
    let backend = self.backend.as_ref().ok_or("this device is not set up yet")?;
    match action {
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
      // Everything else goes to the backend, which queues what needs the engine as commands.
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
    run_commands(&mut self.connection, client, &self.tokens, commands).await
  }

  async fn flush(&mut self) {
    let Some(client) = self.client.as_mut() else {
      return;
    };
    if flush_sends_of(&mut self.connection, client).await {
      self.changed();
    }
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
        _ => true,
      })
      .collect()
  }

  /// Sends what [`Request::Action`] put into the outbox (link previews first). A failure was not
  /// the action's (that was answered already): it is shown by itself.
  /// Runs what the UI queued and sends the outbox. Reads are answered meanwhile, so a message just
  /// written to the outbox shows as pending while it goes out, not after the server answered (#717).
  async fn send_queued(&mut self, requests: &mut mpsc::UnboundedReceiver<Request>) -> Flow {
    let commands = self.drain_commands();
    let Self {
      client: Some(client),
      connection,
      backend,
      store,
      snapshots,
      status,
      catch_up_focus,
      on_change,
      tokens,
      ..
    } = self
    else {
      return Flow::Go;
    };
    let mut reads = ReadsWhileBusy {
      backend: backend.as_ref(),
      store: store.as_ref(),
      snapshots,
      status,
      focus: catch_up_focus,
      maybe_security: security_of(client).ok(),
      on_change: on_change.as_ref(),
    };
    let work = Box::pin(async {
      let ran = run_commands(connection, client, tokens, commands).await;
      let flushed = flush_sends_of(connection, client).await;
      (ran, flushed)
    });
    let served = serve_reads_during(work, requests, &mut reads).await;
    self.has_queued_sends |= served.has_queued;
    let (ran, _) = served.output;
    if let Err(error) = ran {
      (self.on_failure)(describe_failure(error));
    }
    self.changed();
    self.handle_later(served.later, served.is_closed).await
  }

  /// Starts the next due upload, if none runs: the file is encrypted and announced, then goes up
  /// beside the loop.
  async fn start_upload(&mut self) {
    if !self.is_upload_due {
      return;
    }
    // A cancelled file (or a message deleted meanwhile) stops its upload: dropping it aborts it.
    if let (Some(upload), Some(client)) = (&self.maybe_upload, &self.client)
      && !client.is_upload_current(upload)
    {
      self.maybe_upload = None;
      self.changed();
    }
    if self.maybe_upload.is_some() {
      return;
    }
    let Some(client) = self.client.as_mut() else {
      return;
    };
    self.is_upload_due = false;
    let started = self
      .connection
      .keep_alive(SystemClock.now_ms(), client.start_upload())
      .await;
    match started {
      Ok(Some(upload)) => {
        self.maybe_upload = Some(upload);
        // The pending message shows the file's thumbnail from now on.
        self.changed();
      },
      Ok(None) => {},
      Err(error) => tracing::debug!(%error, "upload not started"),
    }
  }

  /// Tells the UI to read again when the transfers in flight moved on.
  fn report_progress(&mut self) {
    let Some(client) = self.client.as_ref() else {
      return;
    };
    let moved = client.transfers().moved_bytes();
    if moved != self.maybe_reported_bytes {
      self.maybe_reported_bytes = moved;
      self.changed();
    }
  }

  /// The running upload's blob is stored (or failed): the file goes into its message, which is sent
  /// once all its files are up.
  async fn on_uploaded(&mut self, sent: Result<(), ApiError>) {
    let (Some(upload), Some(client)) = (self.maybe_upload.take(), self.client.as_mut()) else {
      return;
    };
    let finished = self
      .connection
      .keep_alive(SystemClock.now_ms(), client.finish_upload(upload, sent))
      .await;
    if let Err(error) = finished {
      tracing::debug!(%error, "upload not finished");
    }
    self.is_upload_due = true;
    self.flush().await;
    self.changed();
  }

  /// The periodic work besides the gateway, the same as in the other apps (teamcraft-core upkeep).
  async fn upkeep(&mut self) {
    let Some(client) = self.client.as_mut() else {
      return;
    };
    // The upkeep may resync many conversations; heartbeats go on meanwhile.
    if self
      .connection
      .keep_alive(SystemClock.now_ms(), self.upkeep.tick(client))
      .await
    {
      self.changed();
    }
  }

  /// Connects the gateway if it is due. The connection and the sync after it can take a while (a
  /// server far away, one that is unreachable until the handshake times out): meanwhile the UI's
  /// reads are answered from the store and new messages go to the outbox, so the window shows what
  /// this device has at once (#717). Everything else waits for the connection, in its order.
  async fn connect(&mut self, requests: &mut mpsc::UnboundedReceiver<Request>) -> Flow {
    let Self {
      client: Some(client),
      connection,
      backend,
      store,
      snapshots,
      status,
      catch_up_focus,
      on_change,
      ..
    } = self
    else {
      return Flow::Go;
    };
    let mut reads = ReadsWhileBusy {
      backend: backend.as_ref(),
      store: store.as_ref(),
      snapshots,
      status,
      focus: catch_up_focus,
      // Taken before the connection borrows the client; a later snapshot reads it afresh.
      maybe_security: security_of(client).ok(),
      on_change: on_change.as_ref(),
    };
    // On the heap: the connection's future holds the whole sync, too big for the core thread's stack.
    let connecting = Box::pin(connection.connect_if_due(client));
    let served = serve_reads_during(connecting, requests, &mut reads).await;
    self.has_queued_sends |= served.has_queued;
    if served.output.is_some() {
      self.repeat_reports().await;
      // Back online: what waited goes out now, not on the next tick.
      self.has_queued_sends = true;
      self.is_upload_due = true;
      self.changed();
    }
    self.handle_later(served.later, served.is_closed).await
  }

  /// Handles what waited for long work, in its order; [`Flow::Stop`] when one of them (or the
  /// closed channel) ends the loop.
  async fn handle_later(&mut self, later: Vec<Request>, is_closed: bool) -> Flow {
    for request in later {
      if matches!(request, Request::Shutdown) {
        return Flow::Stop;
      }
      self.handle(request).await;
    }
    if is_closed { Flow::Stop } else { Flow::Go }
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

  /// Tells the other members that the user types (throttled; dropped while offline).
  async fn announce_typing(&mut self, conversation: ConversationId, maybe_thread: Option<ThreadId>) {
    if self.client.is_some() {
      self
        .typing
        .announce(
          &mut self.connection,
          conversation,
          maybe_thread,
          SystemClock.now_ms(),
        )
        .await;
    }
  }

  /// How long until a typing indicator runs out (`None`: nobody types, or there is no client).
  fn typing_change(&self) -> Option<Duration> {
    let at_ms = self.client.as_ref()?.typing_change_at_ms()?;
    let wait_ms = at_ms.saturating_sub(SystemClock.now_ms()).max(0);
    Some(Duration::from_millis(wait_ms.unsigned_abs()))
  }

  /// Forgets the typing indicators that ran out and tells the UI, which reads who types again.
  fn drop_expired_typing(&self) {
    if self.client.as_ref().is_some_and(Client::drop_expired_typing) {
      self.changed();
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

  fn changed(&self) {
    self.drop_removed_renditions();
    (self.on_change)();
  }

  /// The renditions whose message went away (deleted, or its conversation gone): out of the media
  /// cache (#749).
  fn drop_removed_renditions(&self) {
    let Some(store) = &self.store else {
      return;
    };
    let removed = store.take_removed_renditions();
    let Some(cache) = self.media.get() else {
      return;
    };
    for rendition in removed {
      if let Err(error) = cache.remove(&rendition.into()) {
        tracing::debug!(%error, "could not drop a cached rendition");
      }
    }
  }
}

/// Whether the core loop goes on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flow {
  Go,
  Stop,
}

/// What the core thread can answer while the client is busy (connecting, sending): everything that
/// only reads the store, and new messages, which only write the outbox (they go out next).
struct ReadsWhileBusy<'a> {
  backend: Option<&'a CoreChatBackend<Store>>,
  store: Option<&'a Store>,
  snapshots: &'a mut SnapshotCache,
  status: &'a CoreStatus,
  focus: &'a CatchUpFocus,
  maybe_security: Option<SecuritySnapshot>,
  on_change: &'a (dyn Fn() + Send + Sync),
}

/// What [`serve_reads_during`] leaves to its caller.
struct Served<T> {
  output: T,
  /// Requests that need the client, in their order.
  later: Vec<Request>,
  /// A new message went to the outbox meanwhile.
  has_queued: bool,
  /// Every sender is gone: the core loop ends.
  is_closed: bool,
}

/// Runs `work` to its end, answering what [`ReadsWhileBusy`] can answer meanwhile.
async fn serve_reads_during<T>(
  work: impl Future<Output = T>,
  requests: &mut mpsc::UnboundedReceiver<Request>,
  reads: &mut ReadsWhileBusy<'_>,
) -> Served<T> {
  let mut later = Vec::new();
  let mut has_queued = false;
  let mut is_closed = false;
  let mut work = std::pin::pin!(work);
  let output = loop {
    tokio::select! {
      // Work that has nothing to do (no connection due) is done at once, nothing is read beside it.
      biased;
      output = &mut work => break output,
      maybe_request = requests.recv(), if !is_closed => {
        let Some(request) = maybe_request else {
          is_closed = true;
          continue;
        };
        match reads.answer(request) {
          Answered::Done => {},
          Answered::Queued => {
            has_queued = true;
            (reads.on_change)();
          },
          Answered::Later(request) => later.push(request),
        }
      },
    }
  };
  Served {
    output,
    later,
    has_queued,
    is_closed,
  }
}

/// Runs the UI's queued commands; heartbeats go on meanwhile (a command can take long, a big
/// history page). Returns how many ran, or the first failure.
async fn run_commands(
  connection: &mut ConnectionSupervisor<Gateway>,
  client: &mut Client<Api, Store>,
  tokens: &TokenCache,
  commands: Vec<CoreCommand>,
) -> Result<usize, CoreError> {
  let count = commands.len();
  let mut maybe_failure = None;
  for command in commands {
    let maybe_sent = tokens.current_fingerprint();
    let result = connection
      .keep_alive(SystemClock.now_ms(), client.run_ui_command(command.clone()))
      .await;
    // One more try after a refused token, never more (issue #883). The refresh can wait for the
    // session lock and the provider: heartbeats go on meanwhile.
    let result = match result {
      Err(error) if is_refused_token(&error) => {
        let decision = connection
          .keep_alive(SystemClock.now_ms(), retry_decision(tokens, maybe_sent, error))
          .await;
        match decision {
          Ok(()) => {
            connection
              .keep_alive(SystemClock.now_ms(), client.run_ui_command(command))
              .await
          },
          Err(error) => Err(error),
        }
      },
      other => other,
    };
    if let Err(error) = result {
      tracing::warn!(%error, "UI command failed");
      maybe_failure.get_or_insert(error);
    }
  }
  maybe_failure.map_or(Ok(count), Err)
}

/// The server refused the access token (401): an expired token after a suspend, which one
/// refresh fixes (issue #883). Not when the server said why and a new token can't help: the
/// session ended or timed out, the account is deleted or deactivated.
fn is_refused_token(error: &CoreError) -> bool {
  let CoreError::Api(api @ ApiError::Status { status: 401, .. }) = error else {
    return false;
  };
  !matches!(
    api.error_code(),
    Some(
      ErrorCode::SessionEnded
        | ErrorCode::SessionTimedOut
        | ErrorCode::AccountDeleted
        | ErrorCode::AccountDeactivated
    )
  )
}

/// Refreshes the token once after `error` (a refused token): `Ok(())` when the command is worth
/// one more try, otherwise the error to show. A refresh that got no answer is a network problem
/// and says so; only a refused refresh token leaves the sign-in message.
async fn retry_decision(
  tokens: &TokenCache,
  maybe_sent: Option<u64>,
  error: CoreError,
) -> Result<(), CoreError> {
  let Some(sent) = maybe_sent else {
    return Err(error);
  };
  match tokens.refresh_rejected(sent).await {
    Ok(true) => Ok(()),
    Ok(false) => Err(error),
    Err(AuthError::Network(detail)) => Err(CoreError::Api(ApiError::Network(detail))),
    Err(refresh) => {
      tracing::info!(kind = refresh.kind(), "no fresh token after a refused one");
      Err(error)
    },
  }
}

/// Sends the outbox with heartbeats going on; whether anything changed.
async fn flush_sends_of(
  connection: &mut ConnectionSupervisor<Gateway>,
  client: &mut Client<Api, Store>,
) -> bool {
  connection
    .keep_alive(SystemClock.now_ms(), upkeep::flush_sends(client))
    .await
}

/// How [`ReadsWhileConnecting`] dealt with a request.
enum Answered {
  Done,
  /// A new message is in the outbox.
  Queued,
  /// Needs the client: handled once the connection is up (or failed).
  Later(Request),
}

impl ReadsWhileBusy<'_> {
  fn answer(&mut self, request: Request) -> Answered {
    let not_set_up = || "this device is not set up yet".to_owned();
    match request {
      Request::Status(reply) => drop(reply.send(self.status.clone())),
      Request::Snapshot(reply) => {
        let result = match (self.backend, self.store, &self.maybe_security) {
          (Some(backend), Some(store), Some(security)) => Ok(self.snapshots.snapshot(
            backend,
            store.take_changes(),
            || Some(security.clone()),
          )),
          _ => Err(not_set_up()),
        };
        drop(reply.send(result));
      },
      Request::SnapshotChanges(maybe_since, reply) => {
        let result = match (self.backend, self.store) {
          (Some(backend), Some(store)) => Ok(self.snapshots.changes_since(
            backend,
            store.take_changes(),
            || self.maybe_security.clone(),
            maybe_since,
          )),
          _ => Err(not_set_up()),
        };
        drop(reply.send(result));
      },
      Request::Messages {
        conversation,
        start,
        len,
        reply,
      } => {
        // The catch-up running now takes the conversation the user opened next (issue #247).
        self.focus.set(conversation);
        let result = self
          .backend
          .map(|backend| MessageWindow::read(backend, conversation, start, len))
          .ok_or_else(not_set_up);
        drop(reply.send(result));
      },
      Request::Thread {
        conversation,
        thread,
        reply,
      } => {
        let result = self
          .backend
          .map(|backend| backend.thread(conversation, thread))
          .ok_or_else(not_set_up);
        drop(reply.send(result));
      },
      // Typing arriving while the client is busy would go out late: dropped.
      Request::Action(action, reply) if action.typing_place().is_some() => drop(reply.send(Ok(()))),
      Request::Action(action, reply) if action.is_new_message() => {
        let Some(backend) = self.backend else {
          drop(reply.send(Err(not_set_up())));
          return Answered::Done;
        };
        if let Some(unhandled) = action.apply_to(backend) {
          tracing::warn!(?unhandled, "chat action not handled");
        }
        drop(reply.send(Ok(())));
        return Answered::Queued;
      },
      other => return Answered::Later(other),
    }
    Answered::Done
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
/// Resolves when storage answered the running upload; never while there is none.
async fn upload_sent(maybe_upload: &mut Option<BlobUpload>) -> Result<(), ApiError> {
  match maybe_upload {
    Some(upload) => upload.sent().await,
    None => std::future::pending().await,
  }
}

async fn sleep_for(maybe_duration: Option<Duration>) {
  match maybe_duration {
    Some(duration) => tokio::time::sleep(duration).await,
    None => std::future::pending().await,
  }
}

/// An error of opening the store or the client: unreadable data gets its own status.
/// The staged copies of one user's large files (`super::staging`).
fn staging_dir(config: &CoreConfig, user: UserId) -> PathBuf {
  config.data_dir.join("upload-staging").join(user.to_string())
}

fn open_error(error: CoreError) -> OpenError {
  match error.unreadable_store() {
    Some(problem) => OpenError::Unreadable(problem, error.to_string()),
    None => OpenError::Failed(error.to_string()),
  }
}

fn store_problem(problem: UnreadableStore) -> StoreProblem {
  match problem {
    UnreadableStore::Damaged => StoreProblem::Damaged,
    UnreadableStore::NewerVersion => StoreProblem::NewerVersion,
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
  on_change: Arc<dyn Fn() + Send + Sync>,
) -> Result<LinkedSecrets, String> {
  let link = NewDeviceLink::connect(&api, &offer)
    .await
    .map_err(describe_failure)?;
  *view.borrow_mut() = LinkStatus::Confirm {
    code: offer.to_code(),
    short_code: link.short_code(),
  };
  on_change();
  link
    .receive_secrets(&store, &device_name)
    .await
    .map_err(describe_failure)
}

/// What this build runs on, for the prompt on the user's other devices.
fn platform() -> DeviceLinkPlatform {
  match std::env::consts::OS {
    "macos" => DeviceLinkPlatform::Macos,
    "windows" => DeviceLinkPlatform::Windows,
    "linux" => DeviceLinkPlatform::Linux,
    _ => DeviceLinkPlatform::Other,
  }
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
      let error = tokens.access_token().unwrap_err();
      assert!(
        error.is_transient() && !error.is_sign_in_required(),
        "busy, and the token seen last has run out: waiting for the refresh, not signed out ({error})"
      );
    }

    /// Issue #883: a refused token is refreshed and the command tried once more; a refresh without
    /// an answer reads as a network problem, not as "sign in again".
    #[tokio::test]
    async fn a_refused_token_is_refreshed_once_and_no_answer_is_a_network_error() {
      let refused = || {
        CoreError::Api(ApiError::Status {
          status: 401,
          maybe_body: None,
          maybe_retry_after: None,
        })
      };
      assert!(is_refused_token(&refused()));
      assert!(!is_refused_token(&CoreError::Api(ApiError::Token(
        "not signed in".into()
      ))));
      let ended = CoreError::Api(ApiError::Status {
        status: 401,
        maybe_body: Some(teamcraft_proto::ErrorBody::new(ErrorCode::SessionEnded, "ended")),
        maybe_retry_after: None,
      });
      assert!(
        !is_refused_token(&ended),
        "a new token can't undo a sign-out elsewhere"
      );
      assert!(!is_refused_token(&CoreError::Api(ApiError::Network(
        "down".into()
      ))));

      let refreshed = refresher(|| Ok(true));
      assert!(retry_decision(&refreshed, Some(0), refused()).await.is_ok());

      let offline = refresher(|| Err(AuthError::Network("no route to host".into())));
      let error = retry_decision(&offline, Some(0), refused()).await.unwrap_err();
      assert!(matches!(error, CoreError::Api(ApiError::Network(_))), "{error}");

      let denied = refresher(|| Err(AuthError::Denied("invalid_grant".into())));
      let error = retry_decision(&denied, Some(0), refused()).await.unwrap_err();
      assert!(
        error.is_sign_in_required(),
        "a refused refresh token stays a sign-in error"
      );

      let nothing = TokenCache::default();
      assert!(retry_decision(&nothing, Some(0), refused()).await.is_err());
      assert!(
        retry_decision(&refreshed, None, refused()).await.is_err(),
        "no token was sent: nothing to refresh"
      );
    }

    fn refresher(answer: fn() -> Result<bool, AuthError>) -> TokenCache {
      TokenCache {
        maybe_refresher: Some(Arc::new(move |_| Box::pin(async move { answer() }))),
        ..TokenCache::default()
      }
    }

    /// Ready with a token valid for `valid_for` once, busy afterwards.
    fn cache_with(valid_for: Duration) -> TokenCache {
      let was_read = Arc::new(AtomicBool::new(false));
      TokenCache {
        last: Arc::default(),
        maybe_refresher: None,
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
