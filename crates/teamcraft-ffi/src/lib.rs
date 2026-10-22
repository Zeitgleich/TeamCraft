//! Mobile bindings over `teamcraft-core` (ARCHITECTURE.md §8, M8): a small, stable API for the
//! Android app now and the iOS app later, generated with UniFFI.
//!
//! The core's futures aren't `Send` (one engine per device, single-threaded like the browser), and
//! UniFFI's async calls need `Send` futures. So one engine thread owns the client, the store and
//! the sync loop; every call posts a job to it and awaits the answer. The app supplies access
//! tokens ([`TokenProvider`]) and is told when to re-read ([`EventListener`]).

mod bridge;
mod calls;
mod engine;
mod error;
mod types;
mod video;

use std::path::Path;
use std::sync::Arc;

pub use calls::{
  CallEndReason, CallListener, CallPhase, CallSession, CallStateInfo, IncomingCallInfo, VideoListener,
  VideoTrackInfo,
};
pub use error::TeamCraftError;
use teamcraft_media::openh264_library::{self, OpenH264Binary};
use tokio::sync::{mpsc, oneshot};
pub use types::{
  AccountState, AppConfig, ConversationItem, ConversationKind, MentionSettings, MessageItem,
  OrgRecoveryStart, Presence, PushNotification, PushService, ReactionItem, SearchHitItem,
  SegmentPrefetchResult,
};

pub use video::{CameraFrame, VideoFrameRgba};

use crate::engine::{Engine, Job};

uniffi::setup_scaffolding!();

/// The app's handle on the client core of one signed-in account.
#[derive(uniffi::Object)]
pub struct TeamCraft {
  jobs: mpsc::UnboundedSender<Job>,
}

/// Supplies the current OIDC access token. The app owns login and refresh (AppAuth on Android);
/// `None` means signed out.
#[uniffi::export(with_foreign)]
pub trait TokenProvider: Send + Sync {
  fn access_token(&self) -> Option<String>;
}

/// Told when the local store changed, so the UI re-reads what it shows. Called on the engine
/// thread; implementations hand over to their UI thread.
#[uniffi::export(with_foreign)]
pub trait EventListener: Send + Sync {
  fn on_change(&self);
}

#[uniffi::export]
impl TeamCraft {
  /// Opens the encrypted store of `config.account` and the client if this device is enrolled.
  #[uniffi::constructor]
  pub fn new(
    config: AppConfig,
    tokens: Arc<dyn TokenProvider>,
    listener: Arc<dyn EventListener>,
  ) -> Result<Arc<Self>, TeamCraftError> {
    let jobs = engine::spawn(config, tokens, listener)?;
    Ok(Arc::new(Self { jobs }))
  }

  pub async fn account_state(&self) -> Result<AccountState, TeamCraftError> {
    self.call(|engine| Box::pin(engine.account_state())).await
  }

  /// First run on this device: creates the identity, or recovers it with the passphrase if the
  /// account already has one.
  pub async fn enroll(
    &self,
    device_name: String,
    passphrase: String,
  ) -> Result<AccountState, TeamCraftError> {
    self
      .call(move |engine| Box::pin(engine.enroll(device_name, passphrase)))
      .await
  }

  /// New device: the text for the QR code another device of the user scans (e2ee.md §5).
  pub async fn start_device_link(&self) -> Result<String, TeamCraftError> {
    self.call(|engine| Box::pin(engine.start_device_link())).await
  }

  /// New device: waits until the other device joined; returns the short code both show.
  pub async fn wait_for_link_peer(&self) -> Result<String, TeamCraftError> {
    self.call(|engine| Box::pin(engine.wait_for_link_peer())).await
  }

  /// New device: receives the keys after the user confirmed the code, and enrolls.
  pub async fn finish_device_link(&self, device_name: String) -> Result<AccountState, TeamCraftError> {
    self
      .call(move |engine| Box::pin(engine.finish_device_link(device_name)))
      .await
  }

  /// Existing device: scanned `code`; returns the short code to compare.
  pub async fn approve_device_link(&self, code: String) -> Result<String, TeamCraftError> {
    self
      .call(move |engine| Box::pin(engine.approve_device_link(code)))
      .await
  }

  /// Existing device: the user confirmed both codes match; sends the keys.
  pub async fn confirm_device_link(&self) -> Result<(), TeamCraftError> {
    self.call(|engine| Box::pin(engine.confirm_device_link())).await
  }

  /// Starts syncing (gateway, outbox, membership changes). Call when the app is in the foreground.
  pub async fn start(&self) -> Result<(), TeamCraftError> {
    self.call(|engine| Box::pin(engine.start())).await
  }

  /// Stops syncing; the store stays open.
  pub async fn stop(&self) -> Result<(), TeamCraftError> {
    self.call(|engine| Box::pin(engine.stop())).await
  }

  /// Channels of every space and the user's DMs, as the sidebar lists them.
  pub async fn conversations(&self) -> Result<Vec<ConversationItem>, TeamCraftError> {
    self
      .call(|engine| Box::pin(async move { engine.conversations() }))
      .await
  }

  /// The newest `limit` top-level messages, oldest first.
  pub async fn messages(
    &self,
    conversation_id: String,
    limit: u32,
  ) -> Result<Vec<MessageItem>, TeamCraftError> {
    self
      .call(move |engine| Box::pin(async move { engine.messages(&conversation_id, limit) }))
      .await
  }

  /// Root and replies of a thread, oldest first.
  pub async fn thread(
    &self,
    conversation_id: String,
    thread_id: String,
  ) -> Result<Vec<MessageItem>, TeamCraftError> {
    self
      .call(move |engine| Box::pin(async move { engine.thread(&conversation_id, &thread_id) }))
      .await
  }

  /// Queues a message (sent right away if online, retried otherwise).
  pub async fn send_message(
    &self,
    conversation_id: String,
    maybe_thread_id: Option<String>,
    body: String,
  ) -> Result<(), TeamCraftError> {
    self
      .call(move |engine| Box::pin(engine.send_message(conversation_id, maybe_thread_id, body)))
      .await
  }

  pub async fn mark_read(&self, conversation_id: String) -> Result<(), TeamCraftError> {
    self
      .call(move |engine| Box::pin(engine.mark_read(conversation_id)))
      .await
  }

  pub async fn set_favorite(&self, conversation_id: String, is_favorite: bool) -> Result<(), TeamCraftError> {
    self
      .call(move |engine| Box::pin(engine.set_favorite(conversation_id, is_favorite)))
      .await
  }

  pub async fn set_presence(&self, presence: Presence) -> Result<(), TeamCraftError> {
    self
      .call(move |engine| Box::pin(engine.set_presence(presence)))
      .await
  }

  /// Full-text search over decrypted messages (`from:`, `in:` and the other filters work).
  pub async fn search(&self, query: String) -> Result<Vec<SearchHitItem>, TeamCraftError> {
    self
      .call(move |engine| Box::pin(async move { engine.search(&query) }))
      .await
  }

  /// Registers the device's push channel with a fresh push key (e2ee.md §7).
  pub async fn register_push(&self, service: PushService, token: String) -> Result<(), TeamCraftError> {
    self
      .call(move |engine| Box::pin(engine.register_push(service, token)))
      .await
  }

  /// What to show for a push payload (an FCM data message or a UnifiedPush message): opened with
  /// the push key, names from the store, the text fetched and decrypted if possible.
  pub async fn notification_for_push(&self, payload: Vec<u8>) -> Result<PushNotification, TeamCraftError> {
    self
      .call(move |engine| Box::pin(engine.notification_for_push(payload)))
      .await
  }

  /// Keywords, first name and `@channel` preferences for marking pushed messages as mentions.
  pub async fn set_mention_settings(&self, settings: MentionSettings) -> Result<(), TeamCraftError> {
    self
      .call(move |engine| {
        Box::pin(async move {
          engine.set_mention_settings(settings);
          Ok(())
        })
      })
      .await
  }

  /// Downloads the conversations' published search segments (e2ee.md §10.1), newest first, while
  /// the segments kept on the device stay within `max_total_bytes`. The app calls it on unmetered
  /// networks while charging.
  pub async fn prefetch_search_segments(
    &self,
    max_total_bytes: u64,
  ) -> Result<SegmentPrefetchResult, TeamCraftError> {
    self
      .call(move |engine| Box::pin(engine.prefetch_search_segments(max_total_bytes)))
      .await
  }

  /// Everything the conversation list and headers show, as JSON (`ChatSnapshot` of
  /// teamcraft-client-api: users, spaces, conversations with unread counts, message counts, keys).
  pub async fn snapshot(&self) -> Result<String, TeamCraftError> {
    self
      .call(|engine| Box::pin(async move { engine.snapshot_json() }))
      .await
  }

  /// `len` messages of a conversation's feed from `start` on, oldest first, as JSON
  /// (`MessageWindow`).
  pub async fn message_window(
    &self,
    conversation_id: String,
    start: u32,
    len: u32,
  ) -> Result<String, TeamCraftError> {
    self
      .call(move |engine| Box::pin(async move { engine.window_json(&conversation_id, start, len) }))
      .await
  }

  /// Runs a `ChatAction` (JSON, tagged by `action`): send with reply/priority/files, react, edit,
  /// delete, forward, pin, save, mark (un)read, drafts, threads, presence and status, channels,
  /// downloads and share links. Admin actions stay on desktop and web.
  pub async fn act(&self, action_json: String) -> Result<(), TeamCraftError> {
    self
      .call(move |engine| Box::pin(async move { engine.act(&action_json).await }))
      .await
  }

  /// Answers a `ChatQuery` (JSON, tagged by `kind`) with a `ChatAnswer` (JSON, tagged by `answer`):
  /// message extras, threads, pins, saved items, drafts, files, channel browser, search, mentions.
  pub async fn query(&self, query_json: String) -> Result<String, TeamCraftError> {
    self
      .call(move |engine| Box::pin(async move { engine.query_json(&query_json) }))
      .await
  }

  /// An authenticated call to a plain server endpoint under `/api/v1` (notification settings,
  /// meetings): `method` is the HTTP verb, `body` JSON; answers the response JSON.
  pub async fn api_request(
    &self,
    method: String,
    path: String,
    maybe_body: Option<String>,
  ) -> Result<String, TeamCraftError> {
    self
      .call(move |engine| Box::pin(async move { engine.api_request(&method, &path, maybe_body).await }))
      .await
  }

  /// New device without the passphrase: asks the organization's admins to recover the account
  /// (e2ee.md §6.3.1). Show the code; the user reads it to an admin.
  pub async fn start_org_recovery(&self) -> Result<OrgRecoveryStart, TeamCraftError> {
    self.call(|engine| Box::pin(engine.start_org_recovery())).await
  }

  /// After the admins approved: enrolls with the recovered key and a new passphrase. Fails with
  /// `RecoveryPending` while they haven't.
  pub async fn finish_org_recovery(
    &self,
    device_name: String,
    new_passphrase: String,
  ) -> Result<AccountState, TeamCraftError> {
    self
      .call(move |engine| Box::pin(engine.finish_org_recovery(device_name, new_passphrase)))
      .await
  }

  /// Rings and the state of this device's call go to `listener` from now on.
  pub async fn set_call_listener(&self, listener: Arc<dyn CallListener>) -> Result<(), TeamCraftError> {
    self
      .call(move |engine| {
        Box::pin(async move {
          engine.calls().set_listener(listener);
          Ok(())
        })
      })
      .await
  }

  /// Joins (or starts) the conversation's call; returns once connected.
  pub async fn join_call(&self, conversation_id: String) -> Result<Arc<CallSession>, TeamCraftError> {
    let (preparation, hub) = self
      .call(move |engine| {
        Box::pin(async move {
          let preparation = engine.prepare_call(&conversation_id).await?;
          Ok((preparation, engine.calls().clone()))
        })
      })
      .await?;
    calls::join(preparation, hub).await
  }
}

/// Whether sending `body` to a conversation with `member_count` members should be confirmed first
/// (`@channel`/`@here`/`@all` and more members than `confirm_over`; `0` never asks).
#[uniffi::export]
pub fn needs_channel_confirmation(body: String, member_count: u32, confirm_over: u32) -> bool {
  teamcraft_client_api::notify::needs_channel_confirmation(
    &body,
    usize::try_from(member_count).unwrap_or(usize::MAX),
    confirm_over,
  )
}

/// Where Cisco's OpenH264 for this phone is downloaded from (receiving video needs it; it can't be
/// shipped with the app, see teamcraft-media `openh264_library`). `None`: no binary for this CPU.
#[uniffi::export]
pub fn video_codec_download_url() -> Option<String> {
  OpenH264Binary::for_this_platform()
    .ok()
    .map(|binary| binary.download_url())
}

/// Uses an earlier download from `directory`; `false` if there is none (or it doesn't check out).
#[uniffi::export]
pub fn use_installed_video_codec(directory: String) -> bool {
  openh264_library::is_available()
    || OpenH264Binary::for_this_platform().is_ok_and(|binary| binary.use_installed(Path::new(&directory)))
}

/// Unpacks the download from [`video_codec_download_url`] into `directory`, checks it against
/// Cisco's published hashes and uses it for calls joined from now on.
#[uniffi::export]
pub fn install_video_codec(directory: String, compressed: Vec<u8>) -> Result<(), TeamCraftError> {
  OpenH264Binary::for_this_platform()
    .and_then(|binary| binary.install(Path::new(&directory), &compressed))
    .map(drop)
    .map_err(|error| TeamCraftError::Call(error.to_string()))
}

impl TeamCraft {
  /// Runs `job` on the engine thread and waits for its result.
  async fn call<R, F>(&self, job: F) -> Result<R, TeamCraftError>
  where
    R: Send + 'static,
    F: for<'a> FnOnce(&'a mut Engine) -> engine::LocalFuture<'a, Result<R, TeamCraftError>> + Send + 'static,
  {
    let (reply, answer) = oneshot::channel();
    let boxed: Job = Box::new(move |engine| {
      Box::pin(async move {
        // The caller may have given up waiting (cancelled coroutine); nothing to tell it then.
        let _ = reply.send(job(engine).await);
      })
    });
    self.jobs.send(boxed).map_err(|_| TeamCraftError::EngineStopped)?;
    answer.await.map_err(|_| TeamCraftError::EngineStopped)?
  }
}
