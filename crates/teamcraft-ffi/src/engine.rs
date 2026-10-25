//! The engine thread: owns the store, the client and the sync loop, and runs the jobs the
//! [`crate::TeamCraft`] handle posts, one at a time, on a current-thread runtime.

use std::collections::BTreeMap;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
use teamcraft_core::connection::{ConnectionStatus, NetworkChanges};
use teamcraft_core::notify_rules::{load_mention_preferences, save_mention_preferences};
use teamcraft_core::prefetch::{load_segment_prefetch, save_segment_prefetch};
use teamcraft_core::push_guard;
use teamcraft_core::push_preview::{
  PreviewGap, PushPreview, device_push_content, push_content_mode, push_preview, save_device_push_content,
};
use teamcraft_core::runner::{RunnerCommand, RunnerHandle, RunnerSettings, SyncRunner};
use teamcraft_core::store::{SealedSqliteStore, StaticKeySource};
use teamcraft_core::ui_backend::{CoreChatBackend, CoreCommand};
use teamcraft_core::{
  Client, ClientSettings, EnrollmentOptions, EnrollmentSecret, LinkOffer, PendingEnrollment,
};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_crypto::secret::{SecretBytes, SymmetricKey};
use teamcraft_proto::{CallJoinTokenResponse, ConversationId, Id, ThreadId};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, LocalSet};

use crate::calls::{CallHub, CallPreparation};
use crate::error::TeamCraftError;
use crate::types::{
  AccountState, AppConfig, ConversationItem, ConversationKind, MentionSettings, MessageItem,
  NotificationContent, NotificationContentSettings, Presence, PushDiagnosticsInfo, PushNotification,
  PushService, SearchHitItem, SegmentPrefetchMode, SegmentPrefetchResult, TestPushResult,
};
use crate::{EventListener, TokenProvider};

/// Decrypted downloads, one folder per account below it.
const DOWNLOADS_DIR: &str = "downloads";
/// The longest a wake-up is worked on before its notification is shown with what is known by then.
/// FCM gives a high-priority message about 10 s; the app hands over to expedited work at 8 s.
const PUSH_PREVIEW_BUDGET: Duration = Duration::from_secs(7);
/// The part of [`PUSH_PREVIEW_BUDGET`] for fetching and decrypting the message alone; the rest is
/// for catching the conversation up if that fails.
const PUSH_FETCH_BUDGET: Duration = Duration::from_secs(4);
/// What must be left of [`PUSH_PREVIEW_BUDGET`] to start following the groups, which can't be cut
/// short once begun.
const PUSH_FOLLOW_MIN_LEFT: Duration = Duration::from_secs(2);
/// The decrypted downloads an account keeps: the disk cache the apps reopen pictures and files from
/// without downloading them again. Above it the least recently used go at start (a deleted file is
/// fetched again when opened: its transfer shows as remote).
const MAX_DOWNLOADS_BYTES: u64 = 1024 * 1024 * 1024;
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
  signals: SyncSignals,
}

/// What the app's handle shares with every sync loop the engine starts.
#[derive(Clone)]
pub(crate) struct SyncSignals {
  pub(crate) network: NetworkChanges,
  pub(crate) connection: ConnectionStatus,
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

/// [`CoreClient::catch_up_for_push`] within `deadline`, cutting only the cancel-safe steps short.
/// Following the groups (welcomes, MLS) is never dropped halfway: it starts only with
/// [`PUSH_FOLLOW_MIN_LEFT`] to spare and then runs to completion, bounded by its requests' own
/// timeouts; a wake-up that outlasts the receiver's budget continues as expedited work.
async fn catch_up_for_push(
  client: &mut CoreClient,
  conversation: ConversationId,
  deadline: tokio::time::Instant,
) -> Result<(), TeamCraftError> {
  let timed_out = || TeamCraftError::Core("push catch-up timed out".into());
  tokio::time::timeout_at(deadline, client.push_refresh_conversation(conversation))
    .await
    .map_err(|_| timed_out())??;
  if deadline.saturating_duration_since(tokio::time::Instant::now()) < PUSH_FOLLOW_MIN_LEFT {
    return Err(timed_out());
  }
  client.push_follow_groups(conversation).await?;
  tokio::time::timeout_at(deadline, client.push_catch_up_messages(conversation))
    .await
    .map_err(|_| timed_out())??;
  Ok(())
}

/// Starts the engine thread and waits until the store is open.
pub(crate) fn spawn(
  config: AppConfig,
  tokens: Arc<dyn TokenProvider>,
  listener: Arc<dyn EventListener>,
  signals: SyncSignals,
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
        let mut engine = match open_reporting_panics(config, ForeignTokens(tokens), listener, signals) {
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
  signals: SyncSignals,
) -> Result<Engine, TeamCraftError> {
  let report = Arc::new(Mutex::new(String::new()));
  let previous_hook = std::panic::take_hook();
  let hook_report = report.clone();
  std::panic::set_hook(Box::new(move |info| {
    if let Ok(mut report) = hook_report.lock() {
      *report = info.to_string();
    }
  }));
  let opened = std::panic::catch_unwind(AssertUnwindSafe(|| {
    Engine::open(config, tokens, listener, signals)
  }));
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
    signals: SyncSignals,
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
    let download_dir = directory.join(DOWNLOADS_DIR).join(&config.account);
    trim_downloads(&download_dir, MAX_DOWNLOADS_BYTES);
    let mut engine = Self {
      server_url: config.server_url,
      maybe_push_gateway_url: config.push_gateway_url.filter(|url| !url.is_empty()),
      // Per account, so another account signed in on this device never sees these files.
      download_dir,
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
      signals,
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
    // A device link whose keys arrived before the app was stopped finishes enrolling now (#168).
    match PendingEnrollment::resume_link(
      self.api(),
      self.store.clone(),
      Box::new(SystemClock),
      self.client_settings(),
      Argon2Settings::default(),
    )
    .await
    {
      Ok(Some(client)) => {
        self.adopt(client);
        self.listener.on_change();
        return Box::pin(self.account_state()).await;
      },
      Ok(None) => {},
      Err(error) if error.is_transient() || error.is_sign_in_required() => return Err(error.into()),
      // Dropped if they can't work; the user links again.
      Err(error) => tracing::warn!(%error, "could not finish the enrollment of an interrupted device link"),
    }
    let api = self.api();
    let me = api.me().await.map_err(teamcraft_core::CoreError::from)?;
    let has_existing_identity = api
      .identity_key(me.id)
      .await
      .map_err(teamcraft_core::CoreError::from)?
      .is_some();
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
    let secrets = link.receive_secrets(&self.store, &device_name).await?;
    let options = enrollment_options(device_name, EnrollmentSecret::Linked(secrets));
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
    let (mut runner, handle) = SyncRunner::new(
      *client,
      RunnerSettings::default(),
      Box::new(move || listener.on_change()),
    );
    runner.watch_network(&self.signals.network);
    runner.share_connection_status(self.signals.connection.clone());
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

  /// "Add people" in the DM call of `conversation_id`: a meeting room with everybody in the call
  /// and `added`, into which the call moves (docs/meetings.md). The same `request_key` again gets
  /// the same room. Returns the room's chat.
  pub(crate) async fn add_people_to_call(
    &mut self,
    conversation_id: &str,
    in_call: &[String],
    added: &[String],
    maybe_title: Option<String>,
    request_key: &str,
  ) -> Result<String, TeamCraftError> {
    let from: ConversationId = parse_id(conversation_id)?;
    let in_call = parse_ids(in_call)?;
    let added = parse_ids(added)?;
    let key = teamcraft_proto::IdempotencyKey::from_hex(request_key)
      .map_err(|_| TeamCraftError::InvalidInput("not a request key".to_owned()))?;
    let was_running = matches!(self.client, ClientSlot::Running { .. });
    let mut client = self.take_client().await?;
    let result = client
      .add_people_to_call(from, &in_call, &added, maybe_title, key)
      .await;
    self.client = ClientSlot::Idle(client);
    if was_running {
      self.start().await?;
    }
    self.listener.on_change();
    Ok(result?.to_string())
  }

  /// Checks an announced move of the call before this device follows it.
  pub(crate) async fn accept_call_move(&mut self, from: &str, to: &str) -> Result<(), TeamCraftError> {
    let from: ConversationId = parse_id(from)?;
    let to: ConversationId = parse_id(to)?;
    self
      .with_client(move |client| Box::pin(async move { client.accept_call_move(from, to).await }))
      .await
  }

  /// A fresh join token for the conversation's call, to rejoin after a lost connection.
  pub(crate) async fn call_join_token(
    &mut self,
    conversation: ConversationId,
  ) -> Result<CallJoinTokenResponse, TeamCraftError> {
    let was_running = matches!(self.client, ClientSlot::Running { .. });
    let client = self.take_client().await?;
    let result = client.call_join_token(conversation).await;
    self.client = ClientSlot::Idle(client);
    if was_running {
      self.start().await?;
    }
    Ok(result?)
  }

  pub(crate) fn calls(&self) -> &CallHub {
    &self.calls
  }

  pub(crate) async fn notification_for_push(
    &mut self,
    payload: Vec<u8>,
    utc_offset_minutes: i16,
  ) -> Result<PushNotification, TeamCraftError> {
    let preview = self.push_preview_within_budget(&payload).await?;
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

  /// The preview of a wake-up within [`PUSH_PREVIEW_BUDGET`], so the push service's time for the
  /// app (FCM: about 10 s) is never spent on a slow network: a fetch that takes longer, or a catch-up
  /// that doesn't finish, leaves a notification that names the sender instead of none.
  async fn push_preview_within_budget(&mut self, payload: &[u8]) -> Result<PushPreview, TeamCraftError> {
    let deadline = tokio::time::Instant::now() + PUSH_PREVIEW_BUDGET;
    let api = self.api();
    // Without a token the preview still names conversation and sender.
    let maybe_api = self.tokens.0.access_token().map(|_| &api);
    // Cancel-safe: the preview only reads the store.
    let fetched = tokio::time::timeout(
      PUSH_FETCH_BUDGET,
      push_preview(self.store.as_ref(), maybe_api, payload),
    )
    .await;
    let mut preview = match fetched {
      Ok(result) => result?,
      Err(_elapsed) => {
        let mut preview = push_preview::<Api>(self.store.as_ref(), None, payload).await?;
        // Only a missing message is the timeout's fault; a policy or setting cut stays as it is.
        if matches!(
          preview.maybe_gap,
          Some(PreviewGap::NoSession | PreviewGap::NotDecrypted)
        ) {
          preview.maybe_gap = Some(PreviewGap::TimedOut);
        }
        preview
      },
    };
    // The app's own client may catch the conversation up: a conversation this device was just
    // added to, or keys that changed while the app was closed. Not while the sync loop runs: it
    // brings the message itself, and taking the client would cut its connection.
    if preview.needs_catch_up()
      && let ClientSlot::Idle(client) = &mut self.client
    {
      let conversation = preview.wake_up.conversation_id;
      if let Err(error) = catch_up_for_push(client, conversation, deadline).await {
        tracing::debug!(%error, conversation_id = %conversation, "push catch-up stopped");
      }
      let caught_up = push_preview::<Api>(self.store.as_ref(), None, payload).await?;
      // The store-only preview names `NoSession` while the message is still missing: then the first
      // try's reason stays (a failed decryption, a timeout), with any names the catch-up learned.
      if caught_up.maybe_gap != Some(PreviewGap::NoSession) {
        preview = caught_up;
      } else {
        preview.maybe_sender_name = preview.maybe_sender_name.or(caught_up.maybe_sender_name);
        preview.maybe_conversation_name = preview
          .maybe_conversation_name
          .or(caught_up.maybe_conversation_name);
      }
    }
    Ok(preview)
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

  /// This device's notification privacy setting and the workspace policy's limit; both stored, so
  /// a notification extension reads them too.
  pub(crate) fn notification_content(&self) -> NotificationContentSettings {
    let store = self.store.as_ref();
    NotificationContentSettings {
      device: (&device_push_content(store)).into(),
      workspace: (&push_content_mode(store)).into(),
    }
  }

  pub(crate) fn set_notification_content(&self, content: NotificationContent) -> Result<(), TeamCraftError> {
    Ok(save_device_push_content(self.store.as_ref(), &content.into())?)
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
        .with_typing(client.typing_state())
        .with_transfers(client.transfers()),
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
        // What the action wrote locally (a new message in the outbox) shows before the server
        // answers.
        self.listener.on_change();
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
  client.ensure_call_group(conversation).await?;
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

fn parse_ids<T: Id + std::str::FromStr>(texts: &[String]) -> Result<Vec<T>, TeamCraftError> {
  texts.iter().map(|text| parse_id(text)).collect()
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

/// Deletes the least recently used files of `directory` (by modification time, which the apps bump
/// when they open a file) until the rest fit into `max_bytes`.
fn trim_downloads(directory: &std::path::Path, max_bytes: u64) {
  let Ok(entries) = std::fs::read_dir(directory) else {
    return;
  };
  let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
    .flatten()
    .filter_map(|entry| {
      let metadata = entry.metadata().ok()?;
      let modified = metadata.modified().unwrap_or(std::time::UNIX_EPOCH);
      metadata
        .is_file()
        .then(|| (modified, metadata.len(), entry.path()))
    })
    .collect();
  let mut total: u64 = files.iter().map(|(_, size, _)| size).sum();
  files.sort_by_key(|(modified, _, _)| *modified);
  for (_, size, path) in files {
    if total <= max_bytes {
      break;
    }
    match std::fs::remove_file(&path) {
      Ok(()) => total -= size,
      Err(error) => tracing::debug!(%error, "an old download could not be deleted"),
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

#[cfg(test)]
mod tests {
  use std::time::{Duration, SystemTime};

  use super::*;

  #[test]
  fn downloads_above_the_limit_lose_their_least_recently_used_files() {
    let directory = TestDir::new("lru");
    let now = SystemTime::now();
    for (name, age_seconds) in [("old", 300), ("middle", 200), ("new", 100)] {
      let path = directory.path().join(name);
      std::fs::write(&path, [0u8; 10]).unwrap();
      let file = std::fs::File::options().write(true).open(&path).unwrap();
      file.set_modified(now - Duration::from_secs(age_seconds)).unwrap();
    }

    trim_downloads(directory.path(), 20);

    let mut left: Vec<String> = std::fs::read_dir(directory.path())
      .unwrap()
      .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
      .collect();
    left.sort();
    assert_eq!(left, ["middle", "new"]);
  }

  #[test]
  fn downloads_within_the_limit_or_missing_stay_as_they_are() {
    let directory = TestDir::new("within");
    std::fs::write(directory.path().join("one"), [0u8; 10]).unwrap();
    trim_downloads(directory.path(), 10);
    assert!(directory.path().join("one").is_file());
    trim_downloads(&directory.path().join("missing"), 0);
  }

  /// A fresh folder in the system's temporary directory, removed when dropped.
  struct TestDir(PathBuf);

  impl TestDir {
    fn new(name: &str) -> Self {
      let path = std::env::temp_dir().join(format!("teamcraft-downloads-{}-{name}", std::process::id()));
      let _ = std::fs::remove_dir_all(&path);
      std::fs::create_dir_all(&path).unwrap();
      Self(path)
    }

    fn path(&self) -> &std::path::Path {
      &self.0
    }
  }

  impl Drop for TestDir {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.0);
    }
  }
}
