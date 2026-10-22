//! The engine thread: owns the store, the client and the sync loop, and runs the jobs the
//! [`crate::TeamCraft`] handle posts, one at a time, on a current-thread runtime.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use teamcraft_client_api::backend::{ChatBackend, SearchBackend};
use teamcraft_client_api::model::Message as MessageView;
use teamcraft_client_api::search::parse_query;
use teamcraft_core::api::{AccessTokenSource, ApiError, HttpRendezvous, HttpServerApi, ServerApi};
use teamcraft_core::client::LinkApproval;
use teamcraft_core::client::NewDeviceLink;
use teamcraft_core::clock::SystemClock;
use teamcraft_core::push_preview::push_preview;
use teamcraft_core::runner::{RunnerCommand, RunnerHandle, RunnerSettings, SyncRunner};
use teamcraft_core::store::{SealedSqliteStore, StaticKeySource};
use teamcraft_core::ui_backend::{CoreChatBackend, CoreCommand};
use teamcraft_core::{Client, ClientSettings, EnrollmentOptions, LinkOffer, PendingEnrollment};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_crypto::secret::{SecretBytes, SymmetricKey};
use teamcraft_proto::{ConversationId, Id, ThreadId};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, LocalSet};

use crate::error::TeamCraftError;
use crate::types::{
  AccountState, AppConfig, ConversationItem, ConversationKind, MessageItem, Presence, PushNotification,
  PushService, SearchHitItem,
};
use crate::{EventListener, TokenProvider};

/// A boxed future that stays on the engine thread.
pub(crate) type LocalFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;
/// Work for the engine thread.
pub(crate) type Job = Box<dyn for<'a> FnOnce(&'a mut Engine) -> LocalFuture<'a, ()> + Send>;

/// Outbox messages get IDs with this bit set in the UI view model (teamcraft-core ui_backend).
const PENDING_ID_BIT: i64 = 1 << 62;
/// Linked devices get the backup key from the other device; the passphrase isn't used.
const LINKED_DEVICE_PASSPHRASE: &str = "unused: keys come from the linked device";

type Api = HttpServerApi<ForeignTokens>;
type Store = Arc<SealedSqliteStore>;
type CoreClient = Client<Api, Store>;

pub(crate) struct Engine {
  server_url: String,
  tokens: ForeignTokens,
  listener: Arc<dyn EventListener>,
  store: Store,
  client: ClientSlot,
  maybe_backend: Option<CoreChatBackend<Store>>,
  /// The enrolled identity's fingerprint (empty before enrollment).
  identity_fingerprint: String,
  maybe_link_offer: Option<LinkOffer>,
  maybe_new_link: Option<NewDeviceLink<HttpRendezvous>>,
  maybe_approval: Option<PendingApproval>,
}

/// Where the client is: not enrolled yet, held by the engine, or running in the sync loop.
enum ClientSlot {
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
}

/// The app's [`TokenProvider`] as the core's token source.
#[derive(Clone)]
struct ForeignTokens(Arc<dyn TokenProvider>);

/// Starts the engine thread and waits until the store is open.
pub(crate) fn spawn(
  config: AppConfig,
  tokens: Arc<dyn TokenProvider>,
  listener: Arc<dyn EventListener>,
) -> Result<mpsc::UnboundedSender<Job>, TeamCraftError> {
  let (ready, opened) = std::sync::mpsc::channel();
  let (jobs, mut queue) = mpsc::unbounded_channel::<Job>();
  std::thread::Builder::new()
    .name("teamcraft-engine".into())
    .spawn(move || {
      let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
          // The constructor waits for this; if it has gone, nobody needs the answer.
          let _ = ready.send(Err(TeamCraftError::Core(error.to_string())));
          return;
        },
      };
      LocalSet::new().block_on(&runtime, async move {
        let mut engine = match Engine::open(config, ForeignTokens(tokens), listener) {
          Ok(engine) => engine,
          Err(error) => {
            let _ = ready.send(Err(error));
            return;
          },
        };
        let _ = ready.send(Ok(()));
        while let Some(job) = queue.recv().await {
          job(&mut engine).await;
        }
        engine.stop_runner().await;
      });
    })
    .map_err(|error| TeamCraftError::Core(error.to_string()))?;
  opened.recv().map_err(|_| TeamCraftError::EngineStopped)??;
  Ok(jobs)
}

impl Engine {
  fn open(
    config: AppConfig,
    tokens: ForeignTokens,
    listener: Arc<dyn EventListener>,
  ) -> Result<Self, TeamCraftError> {
    let key = SymmetricKey::from_slice(&config.database_key)
      .ok_or_else(|| TeamCraftError::InvalidInput("the database key must be 32 bytes".into()))?;
    if config.account.is_empty() || config.account.contains(['/', '\\']) || config.account.contains("..") {
      return Err(TeamCraftError::InvalidInput("account name".into()));
    }
    let directory = PathBuf::from(&config.data_dir);
    std::fs::create_dir_all(&directory).map_err(|error| TeamCraftError::Storage(error.to_string()))?;
    let path = directory.join(format!("{}.teamcraft.db", config.account));
    let store = Arc::new(SealedSqliteStore::open(&path, &StaticKeySource::new(key))?);
    let mut engine = Self {
      server_url: config.server_url,
      tokens,
      listener,
      store,
      client: ClientSlot::Missing,
      maybe_backend: None,
      identity_fingerprint: String::new(),
      maybe_link_offer: None,
      maybe_new_link: None,
      maybe_approval: None,
    };
    if let Some(client) = Client::open(
      engine.api(),
      engine.store.clone(),
      Box::new(SystemClock),
      ClientSettings::default(),
    )? {
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
    let api = self.api();
    let me = api.me().await.map_err(teamcraft_core::CoreError::from)?;
    let has_existing_identity = match api.identity_key(me.id).await {
      Ok(_) => true,
      Err(error) if error.is_not_found() => false,
      Err(error) => return Err(teamcraft_core::CoreError::from(error).into()),
    };
    Ok(AccountState::NeedsEnrollment {
      has_existing_identity,
    })
  }

  pub(crate) async fn enroll(
    &mut self,
    device_name: String,
    passphrase: String,
  ) -> Result<AccountState, TeamCraftError> {
    let options = enrollment_options(device_name, passphrase);
    self.complete_enrollment(options).await
  }

  pub(crate) async fn start_device_link(&mut self) -> Result<String, TeamCraftError> {
    let offer = LinkOffer::generate();
    let code = offer.to_code();
    self.maybe_link_offer = Some(offer);
    self.maybe_new_link = None;
    Ok(code)
  }

  pub(crate) async fn wait_for_link_peer(&mut self) -> Result<String, TeamCraftError> {
    let offer = self
      .maybe_link_offer
      .as_ref()
      .ok_or(TeamCraftError::NoLinkInProgress)?;
    let link = NewDeviceLink::connect(&self.api(), offer).await?;
    let short_code = link.short_code();
    self.maybe_new_link = Some(link);
    Ok(short_code)
  }

  pub(crate) async fn finish_device_link(
    &mut self,
    device_name: String,
  ) -> Result<AccountState, TeamCraftError> {
    let link = self
      .maybe_new_link
      .take()
      .ok_or(TeamCraftError::NoLinkInProgress)?;
    self.maybe_link_offer = None;
    let mut options = enrollment_options(device_name, LINKED_DEVICE_PASSPHRASE.into());
    options.maybe_linked = Some(link.receive_secrets().await?);
    self.complete_enrollment(options).await
  }

  pub(crate) async fn approve_device_link(&mut self, code: String) -> Result<String, TeamCraftError> {
    let was_running = matches!(self.client, ClientSlot::Running { .. });
    let client = self.take_client().await?;
    let result = client.start_link_approval(&code).await;
    self.client = ClientSlot::Idle(client);
    let approval = result?;
    let short_code = approval.short_code();
    self.maybe_approval = Some(PendingApproval {
      approval,
      was_running,
    });
    Ok(short_code)
  }

  pub(crate) async fn confirm_device_link(&mut self) -> Result<(), TeamCraftError> {
    let pending = self
      .maybe_approval
      .take()
      .ok_or(TeamCraftError::NoLinkInProgress)?;
    let mut client = self.take_client().await?;
    let result = client.complete_link_approval(pending.approval).await;
    self.client = ClientSlot::Idle(client);
    if pending.was_running {
      self.start().await?;
    }
    Ok(result?)
  }

  pub(crate) async fn start(&mut self) -> Result<(), TeamCraftError> {
    let ClientSlot::Idle(client) = std::mem::replace(&mut self.client, ClientSlot::Missing) else {
      // Already running, or nothing to run yet.
      return match &self.client {
        ClientSlot::Missing => Err(TeamCraftError::NotEnrolled),
        _ => Ok(()),
      };
    };
    let listener = self.listener.clone();
    let (runner, handle) = SyncRunner::new(
      *client,
      RunnerSettings::default(),
      Box::new(move || listener.on_change()),
    );
    let task = tokio::task::spawn_local(runner.run());
    self.client = ClientSlot::Running { handle, task };
    Ok(())
  }

  pub(crate) async fn stop(&mut self) -> Result<(), TeamCraftError> {
    self.stop_runner().await;
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
    Ok(
      conversations
        .into_iter()
        .map(|conversation| ConversationItem {
          id: conversation.id.to_string(),
          maybe_space_id: conversation.maybe_space.map(|space| space.to_string()),
          maybe_space_name: conversation
            .maybe_space
            .and_then(|space| spaces.get(&space).cloned()),
          kind: ConversationKind::from_view(conversation.kind),
          name: conversation.name,
          topic: conversation.topic,
          unread_count: conversation.unread_count,
          mention_count: conversation.mention_count,
          is_favorite: conversation.is_favorite,
          is_muted: conversation.is_muted,
          can_post: conversation.can_post,
        })
        .collect(),
    )
  }

  pub(crate) fn messages(
    &self,
    conversation_id: &str,
    limit: u32,
  ) -> Result<Vec<MessageItem>, TeamCraftError> {
    let backend = self.backend()?;
    let conversation: ConversationId = parse_id(conversation_id)?;
    let count = backend.message_count(conversation);
    let len = usize::try_from(limit).unwrap_or(usize::MAX).min(count);
    let messages = backend.messages(conversation, count - len, len);
    Ok(self.message_items(backend, &messages))
  }

  pub(crate) fn thread(
    &self,
    conversation_id: &str,
    thread_id: &str,
  ) -> Result<Vec<MessageItem>, TeamCraftError> {
    let backend = self.backend()?;
    let messages = backend.thread(parse_id(conversation_id)?, parse_id(thread_id)?);
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
    let result = client.register_push(service.into(), &token).await;
    self.client = ClientSlot::Idle(client);
    if was_running {
      self.start().await?;
    }
    Ok(result?)
  }

  pub(crate) async fn notification_for_push(
    &mut self,
    payload: Vec<u8>,
  ) -> Result<PushNotification, TeamCraftError> {
    let api = self.api();
    // Without a token the preview still names conversation and sender.
    let maybe_api = self.tokens.0.access_token().map(|_| &api);
    let preview = push_preview(self.store.as_ref(), maybe_api, &payload).await?;
    Ok(preview.into())
  }

  async fn complete_enrollment(
    &mut self,
    options: EnrollmentOptions,
  ) -> Result<AccountState, TeamCraftError> {
    let client = PendingEnrollment::new()
      .complete(
        self.api(),
        self.store.clone(),
        Box::new(SystemClock),
        ClientSettings::default(),
        options,
      )
      .await?;
    self.adopt(client);
    self.listener.on_change();
    self.account_state().await
  }

  fn adopt(&mut self, client: CoreClient) {
    self.identity_fingerprint = client.identity_fingerprint();
    self.maybe_backend = Some(CoreChatBackend::new(
      self.store.clone(),
      client.user_id(),
      Box::new(SystemClock),
    ));
    self.client = ClientSlot::Idle(Box::new(client));
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
          if let Ok(Some(client)) = Client::open(
            self.api(),
            self.store.clone(),
            Box::new(SystemClock),
            ClientSettings::default(),
          ) {
            self.client = ClientSlot::Idle(Box::new(client));
          }
        },
      }
    }
  }

  /// Hands the UI adapter's queued commands to the sync loop, or runs them right away.
  async fn dispatch_commands(&mut self) -> Result<(), TeamCraftError> {
    let commands = self.backend()?.drain_commands();
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

  fn backend(&self) -> Result<&CoreChatBackend<Store>, TeamCraftError> {
    self.maybe_backend.as_ref().ok_or(TeamCraftError::NotEnrolled)
  }

  fn message_items(&self, backend: &CoreChatBackend<Store>, messages: &[MessageView]) -> Vec<MessageItem> {
    messages
      .iter()
      .map(|message| self.message_item(backend, message))
      .collect()
  }

  fn message_item(&self, backend: &CoreChatBackend<Store>, message: &MessageView) -> MessageItem {
    let author_name = backend
      .user(message.author)
      .map(|user| user.display_name)
      .unwrap_or_default();
    let is_pending = message.id.snowflake().get() & PENDING_ID_BIT != 0;
    MessageItem::from_view(message, author_name, is_pending)
  }

  fn api(&self) -> Api {
    HttpServerApi::new(self.server_url.clone(), self.tokens.clone())
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
async fn run_offline_tolerant(client: &mut CoreClient, command: CoreCommand) -> Result<(), TeamCraftError> {
  match client.run_ui_command(command).await {
    Ok(()) => Ok(()),
    Err(error) if error.is_transient() => Ok(()),
    Err(error) => Err(error.into()),
  }
}

fn enrollment_options(device_name: String, passphrase: String) -> EnrollmentOptions {
  EnrollmentOptions {
    device_name,
    passphrase: SecretBytes::new(passphrase.into_bytes()),
    argon2: Argon2Settings::default(),
    maybe_attestation: None,
    maybe_org_recovery: None,
    maybe_linked: None,
  }
}

fn parse_id<T: Id + std::str::FromStr>(text: &str) -> Result<T, TeamCraftError> {
  text
    .parse()
    .map_err(|_| TeamCraftError::InvalidInput(format!("not an ID: {text}")))
}
