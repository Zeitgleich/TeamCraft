//! The engine thread: owns the store, the client and the sync loop, and runs the jobs the
//! [`crate::TeamCraft`] handle posts, one at a time, on a current-thread runtime.

use std::collections::BTreeMap;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use teamcraft_client_api::backend::{ChatBackend, SearchBackend};
use teamcraft_client_api::model::{
  Conversation as ConversationView, ConversationKind as ConversationKindView, Message as MessageView, UserId,
  is_pending,
};
use teamcraft_client_api::notify::{MentionRules, is_mentioned};
use teamcraft_client_api::search::parse_query;
use teamcraft_core::api::{AccessTokenSource, ApiError, HttpRendezvous, HttpServerApi, ServerApi};
use teamcraft_core::client::LinkApproval;
use teamcraft_core::client::NewDeviceLink;
use teamcraft_core::client::files::sweep_staged_copies;
use teamcraft_core::clock::{Clock as _, SystemClock};
use teamcraft_core::notify_rules::{load_mention_preferences, save_mention_preferences};
use teamcraft_core::prefetch::{load_segment_prefetch, save_segment_prefetch};
use teamcraft_core::push_guard;
use teamcraft_core::push_preview::push_preview;
use teamcraft_core::runner::{RunnerCommand, RunnerHandle, RunnerSettings, SyncRunner};
use teamcraft_core::store::{SealedSqliteStore, StaticKeySource};
use teamcraft_core::ui_backend::{CoreChatBackend, CoreCommand};
use teamcraft_core::{
  Client, ClientSettings, EnrollmentOptions, EnrollmentSecret, LinkOffer, PendingEnrollment,
};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_crypto::secret::{SecretBytes, SymmetricKey};
use teamcraft_proto::{ConversationId, Id, ThreadId};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, LocalSet};

use crate::calls::{CallHub, CallPreparation};
use crate::error::TeamCraftError;
use crate::types::{
  AccountState, AppConfig, ConversationItem, ConversationKind, MentionSettings, MessageItem, Presence,
  PushDiagnosticsInfo, PushNotification, PushService, SearchHitItem, SegmentPrefetchMode,
  SegmentPrefetchResult, TestPushResult,
};
use crate::{EventListener, TokenProvider};

/// Decrypted downloads, one folder per account below it.
const DOWNLOADS_DIR: &str = "downloads";
/// Plaintext copies of files waiting to be uploaded, one folder per account below it.
const UPLOAD_STAGING_DIR: &str = "outgoing";

/// A boxed future that stays on the engine thread.
pub(crate) type LocalFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;
/// Work for the engine thread.
pub(crate) type Job = Box<dyn for<'a> FnOnce(&'a mut Engine) -> LocalFuture<'a, ()> + Send>;

pub(crate) type Api = HttpServerApi<ForeignTokens>;
pub(crate) type Store = Arc<SealedSqliteStore>;
pub(crate) type CoreClient = Client<Api, Store>;

pub(crate) struct Engine {
  server_url: String,
  /// Where downloaded files are decrypted to (inside the app's private storage).
  download_dir: PathBuf,
  maybe_push_gateway_url: Option<String>,
  /// Where the app copies files to upload; the core deletes each copy once it went up.
  upload_staging_dir: PathBuf,
  tokens: ForeignTokens,
  listener: Arc<dyn EventListener>,
  pub(crate) store: Store,
  pub(crate) client: ClientSlot,
  maybe_backend: Option<CoreChatBackend<Store>>,
  /// The enrolled identity's fingerprint (empty before enrollment).
  pub(crate) identity_fingerprint: String,
  pub(crate) maybe_recovery: Option<crate::bridge::PendingRecovery>,
  maybe_link_offer: Option<LinkOffer>,
  maybe_new_link: Option<NewDeviceLink<HttpRendezvous>>,
  maybe_approval: Option<PendingApproval>,
  calls: CallHub,
  /// Applied to decrypted push texts (keywords, first name).
  mentions: MentionSettings,
}

/// Where the client is: not enrolled yet, held by the engine, or running in the sync loop.
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
}

/// The app's [`TokenProvider`] as the core's token source.
#[derive(Clone)]
pub(crate) struct ForeignTokens(Arc<dyn TokenProvider>);

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
        // A panic while opening would otherwise only show as `EngineStopped`; Rust's panic output
        // goes nowhere on Android, so the error carries the message and location instead.
        let mut engine = match open_reporting_panics(config, ForeignTokens(tokens), listener) {
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

/// [`Engine::open`], with a panic turned into an error that says where it happened.
fn open_reporting_panics(
  config: AppConfig,
  tokens: ForeignTokens,
  listener: Arc<dyn EventListener>,
) -> Result<Engine, TeamCraftError> {
  let report = Arc::new(Mutex::new(String::new()));
  let previous_hook = std::panic::take_hook();
  let hook_report = report.clone();
  std::panic::set_hook(Box::new(move |info| {
    if let Ok(mut report) = hook_report.lock() {
      *report = info.to_string();
    }
  }));
  let opened = std::panic::catch_unwind(AssertUnwindSafe(|| Engine::open(config, tokens, listener)));
  std::panic::set_hook(previous_hook);
  opened.unwrap_or_else(|_| {
    let message = report.lock().map(|report| report.clone()).unwrap_or_default();
    Err(TeamCraftError::Core(format!(
      "opening the engine panicked: {message}"
    )))
  })
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
    remove_shared_downloads(&directory.join(DOWNLOADS_DIR));
    let path = directory.join(format!("{}.teamcraft.db", config.account));
    let store = Arc::new(SealedSqliteStore::open(&path, &StaticKeySource::new(key))?);
    let upload_staging_dir = directory.join(UPLOAD_STAGING_DIR).join(&config.account);
    // Plaintext copies of files that were never sent (taken out of the composer, app stopped).
    if let Err(error) = sweep_staged_copies(store.as_ref(), &upload_staging_dir) {
      tracing::debug!(%error, "stray staged upload copies could not be listed");
    }
    // A broken record only costs the preferences, not the store.
    let mentions: MentionSettings = load_mention_preferences(store.as_ref())
      .unwrap_or_default()
      .into();
    let mut engine = Self {
      server_url: config.server_url,
      maybe_push_gateway_url: config.push_gateway_url.filter(|url| !url.is_empty()),
      // Per account, so another account signed in on this device never sees these files.
      download_dir: directory.join(DOWNLOADS_DIR).join(&config.account),
      upload_staging_dir,
      tokens,
      listener,
      store,
      client: ClientSlot::Missing,
      maybe_backend: None,
      identity_fingerprint: String::new(),
      maybe_recovery: None,
      maybe_link_offer: None,
      maybe_new_link: None,
      maybe_approval: None,
      calls: CallHub::default(),
      mentions,
    };
    if let Some(client) = Client::open(
      engine.api(),
      engine.store.clone(),
      Box::new(SystemClock),
      engine.client_settings(),
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
    let options = enrollment_options(
      device_name,
      EnrollmentSecret::Passphrase(SecretBytes::new(passphrase.into_bytes())),
    );
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
    let options = enrollment_options(
      device_name,
      EnrollmentSecret::Linked(link.receive_secrets().await?),
    );
    self.complete_enrollment(options).await
  }

  pub(crate) async fn approve_device_link(&mut self, code: String) -> Result<String, TeamCraftError> {
    let was_running = matches!(self.client, ClientSlot::Running { .. });
    let client = self.take_client().await?;
    let result = client.start_link_approval(&code).await;
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

  /// Drops an approval the user abandoned (the codes were never confirmed) and resumes syncing.
  pub(crate) async fn cancel_device_link_approval(&mut self) -> Result<(), TeamCraftError> {
    let Some(pending) = self.maybe_approval.take() else {
      return Ok(());
    };
    if pending.was_running {
      self.start().await?;
    }
    Ok(())
  }

  pub(crate) async fn start(&mut self) -> Result<(), TeamCraftError> {
    let client = match std::mem::replace(&mut self.client, ClientSlot::Missing) {
      ClientSlot::Idle(client) => client,
      // Already running: keep it (the app starts on sign-in and again on every foreground).
      running @ ClientSlot::Running { .. } => {
        self.client = running;
        return Ok(());
      },
      ClientSlot::Missing => return Err(TeamCraftError::NotEnrolled),
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
    let me = backend.me().id;
    Ok(
      conversations
        .into_iter()
        .map(|conversation| ConversationItem {
          maybe_peer_presence: peer_presence(backend, &conversation, me),
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

  /// Seals the registered device token again when that is due; whether it was renewed.
  pub(crate) async fn refresh_push(&mut self) -> Result<bool, TeamCraftError> {
    let was_running = matches!(self.client, ClientSlot::Running { .. });
    let mut client = self.take_client().await?;
    let result = client.refresh_push().await;
    self.client = ClientSlot::Idle(client);
    if was_running {
      self.start().await?;
    }
    Ok(result?)
  }

  pub(crate) async fn unregister_push(&mut self) -> Result<(), TeamCraftError> {
    self
      .with_client(|client| Box::pin(async move { client.unregister_push().await }))
      .await
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

  pub(crate) fn calls(&self) -> &CallHub {
    &self.calls
  }

  pub(crate) async fn notification_for_push(
    &mut self,
    payload: Vec<u8>,
    utc_offset_minutes: i16,
  ) -> Result<PushNotification, TeamCraftError> {
    let api = self.api();
    // Without a token the preview still names conversation and sender.
    let maybe_api = self.tokens.0.access_token().map(|_| &api);
    let preview = push_preview(self.store.as_ref(), maybe_api, &payload).await?;
    let now_ms = SystemClock.now_ms();
    let store = self.store.as_ref();
    push_guard::record_received(store, &preview.wake_up, now_ms);
    let is_duplicate = push_guard::is_duplicate(store, &preview.wake_up)?;
    let maybe_suppression =
      push_guard::local_suppression(store, &preview.wake_up, now_ms, utc_offset_minutes)?;
    let mut notification: PushNotification = preview.into();
    notification.is_duplicate = is_duplicate;
    notification.maybe_suppression = maybe_suppression.map(Into::into);
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

  pub(crate) fn push_diagnostics(&self) -> Result<PushDiagnosticsInfo, TeamCraftError> {
    Ok(push_guard::push_diagnostics(self.store.as_ref())?.into())
  }

  pub(crate) async fn send_test_push(&self) -> Result<TestPushResult, TeamCraftError> {
    let api = self.api();
    Ok(
      push_guard::send_test_push(self.store.as_ref(), &api)
        .await?
        .into(),
    )
  }

  /// Remembers the preferences in the store, so a process that opens the store later (the iOS
  /// notification extension) applies them too.
  pub(crate) fn set_mention_settings(&mut self, settings: MentionSettings) -> Result<(), TeamCraftError> {
    save_mention_preferences(self.store.as_ref(), &settings.clone().into())?;
    self.mentions = settings;
    Ok(())
  }

  pub(crate) fn mention_settings(&self) -> MentionSettings {
    self.mentions.clone()
  }

  pub(crate) fn segment_prefetch(&self) -> Result<SegmentPrefetchMode, TeamCraftError> {
    Ok(load_segment_prefetch(self.store.as_ref())?.into())
  }

  pub(crate) fn set_segment_prefetch(&self, mode: SegmentPrefetchMode) -> Result<(), TeamCraftError> {
    Ok(save_segment_prefetch(self.store.as_ref(), mode.into())?)
  }

  /// Prefetches the search segments if the saved mode allows it on this network; nothing is
  /// fetched otherwise.
  pub(crate) async fn prefetch_segments(
    &mut self,
    is_network_metered: bool,
    max_total_bytes: u64,
  ) -> Result<SegmentPrefetchResult, TeamCraftError> {
    if !load_segment_prefetch(self.store.as_ref())?.allows(is_network_metered) {
      return Ok(SegmentPrefetchResult {
        fetched: 0,
        skipped: 0,
        stored_bytes: 0,
      });
    }
    self.prefetch_search_segments(max_total_bytes).await
  }

  /// Downloads missing search segments, newest first, within `max_total_bytes`; pauses syncing
  /// meanwhile.
  pub(crate) async fn prefetch_search_segments(
    &mut self,
    max_total_bytes: u64,
  ) -> Result<SegmentPrefetchResult, TeamCraftError> {
    let was_running = matches!(self.client, ClientSlot::Running { .. });
    let mut client = self.take_client().await?;
    let result = client.prefetch_index_segments(max_total_bytes).await;
    self.client = ClientSlot::Idle(client);
    if was_running {
      self.start().await?;
    }
    Ok(result?.into())
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
    self.identity_fingerprint = client.identity_fingerprint();
    self.maybe_backend = Some(
      CoreChatBackend::new(self.store.clone(), client.user_id(), Box::new(SystemClock))
        .with_typing(client.typing_state()),
    );
    self.client = ClientSlot::Idle(Box::new(client));
  }

  /// Gateway call events (rings) of `client` go to the app's call listener.
  fn attach_call_sink(&self, client: &mut CoreClient) {
    let calls = self.calls.clone();
    client.set_call_event_sink(Box::new(move |event| calls.on_gateway_event(event)));
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
          if let Ok(Some(mut client)) = Client::open(
            self.api(),
            self.store.clone(),
            Box::new(SystemClock),
            self.client_settings(),
          ) {
            self.attach_call_sink(&mut client);
            self.client = ClientSlot::Idle(Box::new(client));
          }
        },
      }
    }
  }

  /// Runs `operation` on the client with syncing paused, then resumes it and tells the app.
  pub(crate) async fn with_client<F>(&mut self, operation: F) -> Result<(), TeamCraftError>
  where
    F: for<'a> FnOnce(&'a mut CoreClient) -> LocalFuture<'a, Result<(), teamcraft_core::CoreError>>,
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
  pub(crate) async fn dispatch_commands(&mut self) -> Result<(), TeamCraftError> {
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

  /// Where the app puts copies of files to upload (created if missing).
  pub(crate) fn upload_staging_dir(&self) -> Result<String, TeamCraftError> {
    std::fs::create_dir_all(&self.upload_staging_dir)
      .map_err(|error| TeamCraftError::Storage(error.to_string()))?;
    Ok(self.upload_staging_dir.display().to_string())
  }

  /// Deletes this account's plaintext files: decrypted downloads and staged upload copies (at
  /// sign-out; the sealed store stays).
  pub(crate) fn delete_plaintext_files(&self) -> Result<(), TeamCraftError> {
    for directory in [&self.download_dir, &self.upload_staging_dir] {
      match std::fs::remove_dir_all(directory) {
        Ok(()) => {},
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => return Err(TeamCraftError::Storage(error.to_string())),
      }
    }
    Ok(())
  }

  pub(crate) fn backend(&self) -> Result<&CoreChatBackend<Store>, TeamCraftError> {
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
    let is_pending = is_pending(message.id);
    MessageItem::from_view(message, author_name, is_pending)
  }

  fn client_settings(&self) -> ClientSettings {
    ClientSettings {
      maybe_download_dir: Some(self.download_dir.clone()),
      maybe_push_gateway_url: self.maybe_push_gateway_url.clone(),
      maybe_upload_staging_dir: Some(self.upload_staging_dir.clone()),
      ..ClientSettings::default()
    }
  }

  pub(crate) fn api(&self) -> Api {
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
async fn prepare_call(
  client: &mut CoreClient,
  conversation: ConversationId,
) -> Result<CallPreparation, TeamCraftError> {
  let join = client.call_join_token(conversation).await?;
  let devices = client.verified_call_devices(conversation).await?;
  Ok(CallPreparation {
    conversation,
    join,
    user_id: client.user_id(),
    device_id: client.device_id(),
    signer: client.call_key_signer(),
    devices,
  })
}

async fn run_offline_tolerant(client: &mut CoreClient, command: CoreCommand) -> Result<(), TeamCraftError> {
  match client.run_ui_command(command).await {
    Ok(()) => Ok(()),
    Err(error) if error.is_transient() => Ok(()),
    Err(error) => Err(error.into()),
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

pub(crate) fn parse_id<T: Id + std::str::FromStr>(text: &str) -> Result<T, TeamCraftError> {
  text
    .parse()
    .map_err(|_| TeamCraftError::InvalidInput(format!("not an ID: {text}")))
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

/// The other person's presence in a one-to-one DM; `None` for every other conversation.
fn peer_presence(
  backend: &impl ChatBackend,
  conversation: &ConversationView,
  me: UserId,
) -> Option<Presence> {
  if conversation.kind != ConversationKindView::Direct {
    return None;
  }
  let other = conversation.member_ids.iter().find(|member| **member != me)?;
  backend.user(*other).map(|user| Presence::from(user.presence))
}
