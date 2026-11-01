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
mod http_fetch;
mod join_requests;
mod markup;
mod media_viewer;
mod server_entry;
mod types;
mod video;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Instant;

pub use calls::{
  CallEndReason, CallListener, CallMoveInfo, CallPhase, CallSession, CallStateInfo, EncodedVideoListener,
  IncomingCallInfo, RemoteVideoFrame, RemoteVideoTrack, VideoListener, VideoTrackInfo,
};
pub use error::TeamCraftError;
pub use http_fetch::{
  HttpFetchError, HttpFetchHeader, HttpFetchMethod, HttpFetchRedirects, HttpFetchRequest, HttpFetchResponse,
  http_fetch,
};
pub use join_requests::{
  JoinHistory, JoinRequestAlertChanges, JoinRequestItem, JoinRequestOutcome, JoinRequestStanding,
};
pub use markup::{
  MarkupBlock, MarkupLine, MarkupListKind, MarkupRun, MarkupRunKind, is_long_message, message_markup,
};
pub use media_viewer::{
  AlbumLayout, MediaScope, MediaViewer, MediaViewerItem, MediaViewerLimits, MediaViewerState,
  OriginalDownload, album_layout, fetches_original_automatically, media_viewer_limits,
};
pub use server_entry::{
  CheckedServer, ServerAddress, ServerEntryError, SignInOption, SignInPageInfo, StoredServer,
  check_server_info, normalize_server_address, sign_in_page, sso_only_sign_in_page, stored_server,
};
use teamcraft_core::api::proxy::SystemProxy;
use teamcraft_core::clock::{Clock as _, SystemClock};
use teamcraft_core::connection::{ConnectionStatus, NetworkChanges};
use teamcraft_core::device_activity::ActivityReports;
use teamcraft_media::openh264_library::{self, OpenH264Binary};
use tokio::sync::{mpsc, oneshot};
pub use types::{
  AccountState, AppActivity, AppConfig, BrandAccent, BrandLinks, ConnectionState, ConversationItem,
  ConversationKind, EmojiSuggestion, LinkPlatform, LinkRequestItem, LoginBrand, MentionSettings, MessageItem,
  MessagePage, NetworkInterception, NetworkProxy, NotificationAttachmentKind, NotificationContent,
  NotificationContentSettings, NotificationGap, NotificationSuppression, NotifiedConversation,
  NotifiedMessage, OrgRecoveryStart, Presence, PushDiagnosticsInfo, PushNotification, PushNotificationKind,
  PushService, ReactionItem, SearchHitItem, SegmentPrefetchMode, SegmentPrefetchResult, StoreProblem,
  TestPushOutcome, TestPushResult,
};

pub use video::{CameraFrame, VideoFrameRgba};

use crate::engine::{Engine, Job, PushOpening, PushReader};

uniffi::setup_scaffolding!();

/// The app's handle on the client core of one signed-in account.
#[derive(uniffi::Object)]
pub struct TeamCraft {
  /// The engine thread's queue, set once the engine runs: unset while the local store is
  /// unreadable, until [`Self::reset_store`] (issue #414).
  jobs: OnceLock<mpsc::UnboundedSender<Job>>,
  /// While the local store is unreadable: why, and what opens the engine after a reset. `None` once
  /// the engine runs (only ever taken away, under its lock, when `jobs` is set).
  recovery: Mutex<Option<StoreRecovery>>,
  /// Network changes the app reports; the sync loop (and later calls) listen.
  network: NetworkChanges,
  connection: ConnectionStatus,
  /// What the app last reported about its use; input repeats at most every 30 s (issue #472).
  activity: Mutex<ActivityReports>,
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

/// What the handle keeps while the local store is unreadable (issue #414).
struct StoreRecovery {
  problem: StoreProblem,
  start: engine::EngineStart,
}

#[uniffi::export]
impl TeamCraft {
  /// Opens the encrypted store of `config.account` and the client if this device is enrolled. A
  /// store this version can't read is no error here: [`Self::account_state`] says so, and
  /// [`Self::reset_store`] sets it aside.
  #[uniffi::constructor]
  pub fn new(
    config: AppConfig,
    tokens: Arc<dyn TokenProvider>,
    listener: Arc<dyn EventListener>,
  ) -> Result<Arc<Self>, TeamCraftError> {
    let network = NetworkChanges::new();
    let connection = ConnectionStatus::new();
    let signals = engine::SyncSignals {
      network: network.clone(),
      connection: connection.clone(),
      catch_up_focus: Default::default(),
    };
    let start = engine::EngineStart::new(config, tokens, listener, signals)?;
    let (jobs, maybe_recovery) = match engine::spawn(&start) {
      Ok(jobs) => (OnceLock::from(jobs), None),
      Err(engine::OpenError::Unreadable(problem)) => {
        (OnceLock::new(), Some(StoreRecovery { problem, start }))
      },
      Err(engine::OpenError::Failed(error)) => return Err(error),
    };
    Ok(Arc::new(Self {
      jobs,
      recovery: Mutex::new(maybe_recovery),
      network,
      connection,
      activity: Mutex::default(),
    }))
  }

  /// The network changed (Android `NetworkCallback`, iOS `NWPathMonitor`) or the app came back
  /// from the background: the gateway checks its connection at once instead of after the backoff.
  /// Cheap; call it on every change.
  pub fn network_changed(&self) {
    self.network.notify();
  }

  /// Where the gateway connection stands, for the banner. Changes are announced through
  /// [`EventListener::on_change`].
  pub fn connection_state(&self) -> ConnectionState {
    self.connection.get().into()
  }

  /// What the user does with the app, for presence (issue #472): `Foreground` when the app comes
  /// to the foreground, `Background` when it leaves it, `Interaction` on every touch or key press.
  /// The server derives the user's presence from it and ends an away timeout at once when the app
  /// comes back. Cheap and never waits: input reports at most every 30 s.
  pub fn report_activity(&self, activity: AppActivity) {
    // Plain data: a panic elsewhere while it was locked leaves nothing half-changed.
    let maybe_due = self
      .activity
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .next(activity.into(), Instant::now());
    let Some(device_activity) = maybe_due else {
      return;
    };
    let job: Job = Box::new(move |engine| Box::pin(async move { engine.report_activity(device_activity) }));
    // No engine while the store is unreadable, and a closing handle refuses: nothing to report to.
    if let Some(jobs) = self.jobs.get() {
      let _ = jobs.send(job);
    }
  }

  pub async fn account_state(&self) -> Result<AccountState, TeamCraftError> {
    if let Some(problem) = self.unreadable_problem() {
      return Ok(AccountState::StoreUnreadable { problem });
    }
    self.call(|engine| Box::pin(engine.account_state())).await
  }

  /// "Set up this device again" while the local data can't be read
  /// ([`AccountState::StoreUnreadable`], issue #414): sets the database and its log aside beside
  /// themselves (renamed, never deleted) and opens a new, empty store; the account state is then
  /// `NeedsEnrollment`. Refused while the store is readable. Waits while the new store opens, like
  /// the constructor: call it off the main thread.
  pub fn reset_store(&self) -> Result<(), TeamCraftError> {
    // Held throughout, so two resets can't run at once; `account_state` waits the moment it takes.
    let mut recovery = self.recovery.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(unreadable) = recovery.as_ref() else {
      return Err(TeamCraftError::InvalidInput(
        "the local data is readable; nothing to reset".into(),
      ));
    };
    engine::set_store_aside(&unreadable.start.store_path(), SystemClock.now_ms())?;
    let jobs = engine::spawn(&unreadable.start).map_err(|error| match error {
      // A new, empty store that doesn't open: no room or no permission, not the old data.
      engine::OpenError::Unreadable(_) => {
        TeamCraftError::Storage("the new local database can't be read".into())
      },
      engine::OpenError::Failed(error) => error,
    })?;
    // Invariant: `jobs` is unset while `recovery` holds something, and only set here.
    let _ = self.jobs.set(jobs);
    *recovery = None;
    Ok(())
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

  /// New device: the text for the QR code another device of the user scans (e2ee.md §5). The
  /// `device_name` and `platform` are what [`Self::announce_device_link`] tells the other
  /// devices.
  pub async fn start_device_link(
    &self,
    device_name: String,
    platform: LinkPlatform,
  ) -> Result<String, TeamCraftError> {
    self
      .call(move |engine| Box::pin(engine.start_device_link(device_name, platform)))
      .await
  }

  /// New device: asks the user's other devices to approve this one with a prompt. This hands the
  /// code's key to the server, which scanning does not: call it only when the user chose "confirm
  /// from another device".
  pub async fn announce_device_link(&self) -> Result<(), TeamCraftError> {
    self.call(|engine| Box::pin(engine.announce_device_link())).await
  }

  /// New device: waits until the other device joined; returns the short code both show. The other
  /// calls go on meanwhile, [`Self::announce_device_link`] above all (issue #999).
  pub async fn wait_for_link_peer(&self) -> Result<String, TeamCraftError> {
    let peer = self
      .call(|engine| Box::pin(async move { engine.wait_for_link_peer() }))
      .await?;
    // A new link or a sign-out ended the wait.
    peer.await.map_err(|_| TeamCraftError::NoLinkInProgress)?
  }

  /// New phone: scanned the code a signed-in computer shows for it (*Link a phone*, issue #998).
  /// Joins that device instead of waiting for one; returns the short code to show, which the user
  /// types on the computer. A wait of [`Self::wait_for_link_peer`] still running ends (it fails with
  /// `NoLinkInProgress`). A code of another new device is `LinkCodeForSignedInDevice`, before
  /// anything joins. Then [`Self::finish_device_link`] as for any link.
  pub async fn join_scanned_device_link(&self, code: String) -> Result<String, TeamCraftError> {
    let peer = self
      .call(move |engine| Box::pin(async move { engine.join_scanned_device_link(&code) }))
      .await?;
    peer.await.map_err(|_| TeamCraftError::NoLinkInProgress)?
  }

  /// New device: receives the keys after the user confirmed the code, and enrolls.
  pub async fn finish_device_link(&self, device_name: String) -> Result<AccountState, TeamCraftError> {
    self
      .call(move |engine| Box::pin(engine.finish_device_link(device_name)))
      .await
  }

  /// Existing device: scanned (or typed) `code`; joins the new device. The short code appears only
  /// on the new device: the user types it into [`Self::confirm_device_link`].
  pub async fn approve_device_link(&self, code: String) -> Result<(), TeamCraftError> {
    self
      .call(move |engine| Box::pin(engine.approve_device_link(code)))
      .await
  }

  /// Existing device: the user typed the short code the new device shows; sends the keys if it is
  /// right. A wrong code is `LinkCodeRejected` (try again); after the last try it is
  /// `LinkAttemptEnded`. Returns once the new device confirmed the keys (up to a minute if it went
  /// away); syncing and the other calls go on meanwhile (issue #415).
  pub async fn confirm_device_link(&self, typed_code: String) -> Result<(), TeamCraftError> {
    let receipt = self
      .call(move |engine| Box::pin(engine.confirm_device_link(typed_code)))
      .await?;
    receipt.await.map_err(|_| TeamCraftError::EngineStopped)?
  }

  /// New devices of this account that ask to be linked (e2ee.md §5). Read it when the app is told
  /// about a change; the list is empty while nothing waits.
  pub async fn link_requests(&self) -> Result<Vec<LinkRequestItem>, TeamCraftError> {
    self
      .call(|engine| Box::pin(async move { Ok(engine.link_requests()) }))
      .await
  }

  /// Existing device: takes up a request from the prompt and joins its relay. The other devices
  /// drop their prompt. The user then types the code the new device shows into
  /// [`Self::confirm_device_link`].
  pub async fn review_link_request(&self, id: String) -> Result<(), TeamCraftError> {
    self
      .call(move |engine| Box::pin(engine.review_link_request(id)))
      .await
  }

  /// Existing device: denies a request; the new device is told.
  pub async fn reject_link_request(&self, id: String) -> Result<(), TeamCraftError> {
    self
      .call(move |engine| Box::pin(engine.reject_link_request(id)))
      .await
  }

  /// Existing device: the user left the comparison without confirming; nothing is sent and
  /// syncing resumes. A confirmation still waiting for the new device ends as cancelled.
  pub async fn cancel_device_link_approval(&self) -> Result<(), TeamCraftError> {
    self
      .call(|engine| Box::pin(engine.cancel_device_link_approval()))
      .await
  }

  /// Sign-out before this device is set up (issue #415): drops the keys an unfinished setup kept
  /// and asks the server to revoke the device it registered, waiting a few seconds at most. Call it
  /// before the session ends; it changes nothing on a device that is set up.
  pub async fn forget_unfinished_setup(&self) -> Result<(), TeamCraftError> {
    self
      .call(|engine| Box::pin(engine.forget_unfinished_setup()))
      .await
  }

  /// Where the app copies a picked file before sending it; the core deletes the copy once the
  /// upload finished (or failed for good).
  pub async fn upload_staging_dir(&self) -> Result<String, TeamCraftError> {
    self
      .call(|engine| Box::pin(async move { engine.upload_staging_dir() }))
      .await
  }

  /// Deletes this account's plaintext files on the device (decrypted downloads, staged uploads);
  /// for sign-out. The sealed store stays.
  pub async fn delete_plaintext_files(&self) -> Result<(), TeamCraftError> {
    // An unreadable store has no engine, but its account's plaintext files are still there.
    let maybe_unreadable_dirs = self
      .recovery
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .as_ref()
      .map(|recovery| recovery.start.plaintext_dirs());
    if let Some(directories) = maybe_unreadable_dirs {
      return engine::delete_plaintext_dirs(directories.iter().map(PathBuf::as_path));
    }
    self
      .call(|engine| Box::pin(async move { engine.delete_plaintext_files() }))
      .await
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

  /// Where a share from another app can go: the conversations the user may post in, their notes
  /// first, then by last activity (issue #829).
  pub async fn share_targets(&self) -> Result<Vec<ConversationItem>, TeamCraftError> {
    self
      .call(|engine| Box::pin(async move { engine.share_targets() }))
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

  /// What a conversation list that pages back shows (issue #1058): every message from the oldest
  /// one it holds (`maybe_oldest_id`, sent at `oldest_sent_at_ms`) to the newest, and `older`
  /// messages before it. Rereading with the same anchor after a change keeps the rows paged to;
  /// growing `older` pages further back. Without an anchor it is the newest `older` messages.
  pub async fn messages_from(
    &self,
    conversation_id: String,
    maybe_oldest_id: Option<String>,
    oldest_sent_at_ms: i64,
    older: u32,
  ) -> Result<MessagePage, TeamCraftError> {
    self
      .call(move |engine| {
        Box::pin(async move {
          engine.messages_from(
            &conversation_id,
            maybe_oldest_id.as_deref(),
            oldest_sent_at_ms,
            older,
          )
        })
      })
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

  /// Registers the device's push channel (e2ee.md §7): with a fresh push key the first time, under
  /// the key the device already holds when the same service delivers its token again (every app
  /// start, a UnifiedPush endpoint re-sent by the distributor). Safe to call repeatedly.
  pub async fn register_push(&self, service: PushService, token: String) -> Result<(), TeamCraftError> {
    self
      .call(move |engine| Box::pin(engine.register_push(service, token)))
      .await
  }

  /// Seals the registered APNs/FCM device token for the push gateway again when that is due (it
  /// is sealed for one server and expires); call it when the app starts or returns to the
  /// foreground. Returns whether the registration was renewed; a no-op without a registration.
  pub async fn refresh_push(&self) -> Result<bool, TeamCraftError> {
    self.call(|engine| Box::pin(engine.refresh_push())).await
  }

  /// Removes this device's push registration and its push key, and ends a sealed device token at
  /// its gateway (sign-out).
  pub async fn unregister_push(&self) -> Result<(), TeamCraftError> {
    self.call(|engine| Box::pin(engine.unregister_push())).await
  }

  /// What to do for a push payload (an FCM data message, a UnifiedPush message, an APNs payload's
  /// `w`): opened with the push key, names from the store, the text fetched and decrypted if
  /// possible, checked against what was shown before and against mutes and DND.
  /// `utc_offset_minutes` is the device's current offset from UTC (for the DND schedule).
  pub async fn notification_for_push(
    &self,
    payload: Vec<u8>,
    utc_offset_minutes: i16,
  ) -> Result<PushNotification, TeamCraftError> {
    self
      .call(move |engine| {
        Box::pin(engine.notification_for_push(
          payload,
          utc_offset_minutes,
          PushOpening::Received,
          PushReader::App,
        ))
      })
      .await
  }

  /// [`Self::notification_for_push`] for a notification extension running next to the app (iOS):
  /// shows what the store and the one fetched message give, and never catches the conversation up,
  /// so the extension never advances the account's state (MLS, cursors, keys) behind the app's back
  /// (e2ee.md §7.2).
  pub async fn notification_preview_for_push(
    &self,
    payload: Vec<u8>,
    utc_offset_minutes: i16,
  ) -> Result<PushNotification, TeamCraftError> {
    self
      .call(move |engine| {
        Box::pin(engine.notification_for_push(
          payload,
          utc_offset_minutes,
          PushOpening::Received,
          PushReader::Extension,
        ))
      })
      .await
  }

  /// [`Self::notification_for_push`] again for a wake-up whose first try could not read the
  /// message in time (`maybe_gap` timed out, not decrypted, no session): the app retries later to
  /// update the notification it already shows. Not counted as a received push in the diagnostics.
  pub async fn notification_for_push_retry(
    &self,
    payload: Vec<u8>,
    utc_offset_minutes: i16,
  ) -> Result<PushNotification, TeamCraftError> {
    self
      .call(move |engine| {
        Box::pin(engine.notification_for_push(
          payload,
          utc_offset_minutes,
          PushOpening::Retry,
          PushReader::App,
        ))
      })
      .await
  }

  /// What the newest notification showed of a conversation (its name, the notified message),
  /// while this device hasn't caught it up yet: the conversation screen shows it at once instead of
  /// an empty screen without a name (issue #1065). `None` once the store has it all.
  pub async fn notified_conversation(
    &self,
    conversation_id: String,
  ) -> Result<Option<NotifiedConversation>, TeamCraftError> {
    self
      .call(move |engine| Box::pin(async move { engine.notified_conversation(&conversation_id) }))
      .await
  }

  /// What this device saw of pushes (notification diagnostics).
  pub async fn push_diagnostics(&self) -> Result<PushDiagnosticsInfo, TeamCraftError> {
    self
      .call(|engine| Box::pin(async move { engine.push_diagnostics() }))
      .await
  }

  /// Asks the server for a sealed test wake-up to this device; it arrives like any push.
  pub async fn send_test_push(&self) -> Result<TestPushResult, TeamCraftError> {
    self.call(|engine| Box::pin(engine.send_test_push())).await
  }

  /// Keywords, first name and `@channel` preferences for marking pushed messages as mentions.
  pub async fn set_mention_settings(&self, settings: MentionSettings) -> Result<(), TeamCraftError> {
    self
      .call(move |engine| Box::pin(async move { engine.set_mention_settings(settings) }))
      .await
  }

  /// The saved mention preferences (kept in the store, so the push extension applies them too).
  pub async fn mention_settings(&self) -> Result<MentionSettings, TeamCraftError> {
    self
      .call(|engine| Box::pin(async move { Ok(engine.mention_settings()) }))
      .await
  }

  /// How much message notifications show: this device's setting and the workspace policy's limit.
  pub async fn notification_content(&self) -> Result<NotificationContentSettings, TeamCraftError> {
    self
      .call(|engine| Box::pin(async move { Ok(engine.notification_content()) }))
      .await
  }

  /// Sets this device's notification privacy setting; the workspace policy can only lower it.
  pub async fn set_notification_content(&self, content: NotificationContent) -> Result<(), TeamCraftError> {
    self
      .call(move |engine| Box::pin(async move { engine.set_notification_content(content) }))
      .await
  }

  /// When search index segments are prefetched; the platform schedules the work (see
  /// `prefetch_segments`).
  pub async fn segment_prefetch(&self) -> Result<SegmentPrefetchMode, TeamCraftError> {
    self
      .call(|engine| Box::pin(async move { engine.segment_prefetch() }))
      .await
  }

  pub async fn set_segment_prefetch(&self, mode: SegmentPrefetchMode) -> Result<(), TeamCraftError> {
    self
      .call(move |engine| Box::pin(async move { engine.set_segment_prefetch(mode) }))
      .await
  }

  /// Prefetches the search segments like `prefetch_search_segments` if the saved mode allows it
  /// on the current network (`is_network_metered`); fetches nothing otherwise. For background tasks.
  pub async fn prefetch_segments(
    &self,
    is_network_metered: bool,
    max_total_bytes: u64,
  ) -> Result<SegmentPrefetchResult, TeamCraftError> {
    self
      .call(move |engine| Box::pin(engine.prefetch_segments(is_network_metered, max_total_bytes)))
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

  /// [`Self::messages_from`] as JSON (`MessageTail`), with the messages of the JSON bridge.
  pub async fn message_tail(
    &self,
    conversation_id: String,
    maybe_oldest_id: Option<String>,
    oldest_sent_at_ms: i64,
    older: u32,
  ) -> Result<String, TeamCraftError> {
    self
      .call(move |engine| {
        Box::pin(async move {
          engine.tail_json(
            &conversation_id,
            maybe_oldest_id.as_deref(),
            oldest_sent_at_ms,
            older,
          )
        })
      })
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

  /// The profile picture of a person (image bytes, at most `max_bytes`), or `None` without one.
  /// Fetched on demand; the app keeps its own cache.
  pub async fn avatar_picture(
    &self,
    user_id: String,
    max_bytes: u32,
  ) -> Result<Option<Vec<u8>>, TeamCraftError> {
    self
      .call(move |engine| {
        Box::pin(async move { engine.avatar_picture(&user_id, u64::from(max_bytes)).await })
      })
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

  /// "Add people" in the DM or group DM call of `conversation_id` (docs/meetings.md, "A call becomes
  /// a meeting"): starts a meeting room with the people in the call (`in_call`, as the call shows
  /// them) and `added`, founds its encryption with exactly them and moves the call there. The DM
  /// itself doesn't change. `request_key` from [`new_request_key`], kept for retries of this one
  /// action, gets the same room again. Returns the room's conversation; join its call next.
  pub async fn add_people_to_call(
    &self,
    conversation_id: String,
    in_call: Vec<String>,
    added: Vec<String>,
    maybe_title: Option<String>,
    request_key: String,
  ) -> Result<String, TeamCraftError> {
    self
      .call(move |engine| {
        Box::pin(async move {
          engine
            .add_people_to_call(&conversation_id, &in_call, &added, maybe_title, &request_key)
            .await
        })
      })
      .await
  }

  /// The announced move of the call of `from_conversation_id` into `to_conversation_id` (after
  /// `CallListener::on_call_moved`): who moved it and whom they added, as a member of the DM's
  /// encryption group announced it. `None` until that announcement is in; the server's word alone
  /// shows nothing.
  pub async fn call_move_offer(
    &self,
    from_conversation_id: String,
    to_conversation_id: String,
  ) -> Result<Option<CallMoveInfo>, TeamCraftError> {
    self
      .call(move |engine| {
        Box::pin(async move {
          let from = engine::parse_id(&from_conversation_id)?;
          let to = engine::parse_id(&to_conversation_id)?;
          let maybe_record = teamcraft_core::client::call_move_announcement(engine.store.as_ref(), from, to)?;
          Ok(maybe_record.map(|record| CallMoveInfo {
            by_user_id: record.by.to_string(),
            added_user_ids: record.added.iter().map(ToString::to_string).collect(),
            is_requested_here: record.is_requested_here,
          }))
        })
      })
      .await
  }

  /// Checks the move before following it: announced in the DM's group, and the room holds nobody
  /// but the DM's members and the people it named. `CallMoveRefused` otherwise; then stay.
  pub async fn accept_call_move(
    &self,
    from_conversation_id: String,
    to_conversation_id: String,
  ) -> Result<(), TeamCraftError> {
    self
      .call(move |engine| {
        Box::pin(async move {
          engine
            .accept_call_move(&from_conversation_id, &to_conversation_id)
            .await
        })
      })
      .await
  }

  /// Joins (or starts) the conversation's call; returns once connected. `is_muted`: the
  /// microphone stays muted from the first frame (a muted call moving into a meeting room), so
  /// nothing is sent before an unmute.
  #[uniffi::method(default(is_muted = false))]
  pub async fn join_call(
    &self,
    conversation_id: String,
    is_muted: bool,
  ) -> Result<Arc<CallSession>, TeamCraftError> {
    let (preparation, hub) = self
      .call(move |engine| {
        Box::pin(async move {
          let preparation = engine.prepare_call(&conversation_id).await?;
          Ok((preparation, engine.calls().clone()))
        })
      })
      .await?;
    calls::join(
      preparation,
      is_muted,
      hub,
      self.engine_jobs()?.clone(),
      &self.network,
      self.connection.clone(),
    )
    .await
  }
}

/// A fresh key for one user action that may be retried (`add_people_to_call`).
#[uniffi::export]
pub fn new_request_key() -> String {
  teamcraft_proto::IdempotencyKey::random().to_hex()
}

/// Emoji whose names match what the user typed after `:` in the composer (the shared catalog, as
/// on desktop), at most `limit`.
#[uniffi::export]
pub fn emoji_suggestions(query: String, limit: u32) -> Vec<EmojiSuggestion> {
  teamcraft_client_api::emoji_catalog::search(&query)
    .into_iter()
    .take(usize::try_from(limit).unwrap_or(usize::MAX))
    .map(|entry| EmojiSuggestion {
      emoji: entry.emoji.to_string(),
      shortcode: teamcraft_client_api::emoji_catalog::shortcode(entry.emoji)
        .unwrap_or_default()
        .to_string(),
    })
    .collect()
}

/// Replaces emoticons (`:)`) and `:shortcodes:` in a message before sending, as desktop does;
/// code stays as typed.
#[uniffi::export]
pub fn emoji_convert(text: String) -> String {
  teamcraft_client_api::emoji::convert(&text)
}

/// The sign-in screen's brand (docs/branding.md §5) from the server's `GET /api/v1/brand` answer,
/// checked like on every platform: image URLs only of the brand endpoint on `server_url`, https
/// links only. `None` for an unusable answer (the app shows its own name).
#[uniffi::export]
pub fn login_brand(response_json: String, server_url: String, language: String) -> Option<LoginBrand> {
  let response = serde_json::from_str(&response_json).ok()?;
  let brand = teamcraft_client_api::brand::BrandView::from_response(response, &server_url)?;
  Some(LoginBrand {
    display_name: brand.display_name.clone(),
    is_product: brand.is_product(),
    maybe_icon_url: brand.asset("icon", false).map(str::to_owned),
    maybe_icon_dark_url: brand.asset("icon", true).map(str::to_owned),
    maybe_logo_url: brand.asset("logo", false).map(str::to_owned),
    maybe_logo_dark_url: brand.asset("logo", true).map(str::to_owned),
    maybe_title: brand.login_title(&language).map(str::to_owned),
    maybe_text: brand.login_text(&language).map(str::to_owned),
    links: BrandLinks {
      maybe_support: brand.links.maybe_support.clone(),
      maybe_documentation: brand.links.maybe_documentation.clone(),
      maybe_privacy: brand.links.maybe_privacy.clone(),
      maybe_terms: brand.links.maybe_terms.clone(),
      maybe_imprint: brand.links.maybe_imprint.clone(),
    },
    light: brand_accent(brand.properties(false)),
    dark: brand_accent(brand.properties(true)),
  })
}

/// The accent tokens out of the checked CSS properties of one theme.
fn brand_accent(properties: &[(String, String)]) -> BrandAccent {
  let pick = |name: &str| {
    properties
      .iter()
      .find(|(property, _)| property == name)
      .map(|(_, value)| value.clone())
  };
  BrandAccent {
    maybe_accent: pick("--accent"),
    maybe_accent_strong: pick("--accent-strong"),
    maybe_on_accent: pick("--on-accent"),
    maybe_accent_soft: pick("--accent-soft"),
  }
}

/// Whether a link from content other members control (a bookmark) may be opened: web links only.
#[uniffi::export]
pub fn is_openable_link(url: String) -> bool {
  teamcraft_client_api::links::is_openable_link(&url)
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

/// The platform's HTTP proxy for every connection of the core (issue #157): Android
/// `LinkProperties.httpProxy`, iOS `CFNetworkCopySystemProxySettings`. Call it at start and whenever
/// the network changes; `None` when there is none. PAC scripts aren't evaluated: pass the proxy
/// they would pick, or none.
#[uniffi::export]
pub fn set_network_proxy(maybe_proxy: Option<NetworkProxy>) {
  teamcraft_core::api::proxy::set_system_proxy(maybe_proxy.map(|proxy| SystemProxy {
    host: proxy.host,
    port: proxy.port,
    exclusions: proxy.exclusions,
  }));
}

/// The certificate authorities this device trusts (DER), for every TLS connection of the core
/// (issue #157). Android only: call at start; connections opened afterwards use them, earlier ones
/// the Mozilla roots. The app decides which CAs count (docs/operations.md, "Company proxies, TLS
/// inspection and captive portals").
#[uniffi::export]
pub fn set_trusted_certificates(der_certificates: Vec<Vec<u8>>) {
  teamcraft_core::api::set_platform_roots(der_certificates);
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
    self
      .engine_jobs()?
      .send(boxed)
      .map_err(|_| TeamCraftError::EngineStopped)?;
    answer.await.map_err(|_| TeamCraftError::EngineStopped)?
  }

  /// The engine thread's queue; none while the local store is unreadable (issue #414).
  fn engine_jobs(&self) -> Result<&mpsc::UnboundedSender<Job>, TeamCraftError> {
    self
      .jobs
      .get()
      .ok_or_else(|| TeamCraftError::Storage("the local data can't be read; set this device up again".into()))
  }

  /// Why the local store can't be read, while it can't.
  fn unreadable_problem(&self) -> Option<StoreProblem> {
    self
      .recovery
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .as_ref()
      .map(|recovery| recovery.problem)
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_core::store::{LocalStore as _, SealedSqliteStore, StaticKeySource, Table};
  use teamcraft_crypto::secret::SymmetricKey;

  use super::*;

  const ACCOUNT: &str = "alice";
  const KEY: [u8; 32] = [7; 32];

  /// Issue #414: data this version can't read is an account state with a way out, not an error.
  mod store_recovery {
    use super::*;

    #[tokio::test]
    async fn a_damaged_store_is_reported_then_set_aside_for_a_new_one() {
      let directory = TestDir::new("damaged");
      std::fs::write(directory.store(), b"not a database").unwrap();

      let core = open(&directory, KEY).unwrap();
      assert_eq!(
        core.account_state().await.unwrap(),
        AccountState::StoreUnreadable {
          problem: StoreProblem::Damaged
        }
      );
      assert!(matches!(
        core.mention_settings().await,
        Err(TeamCraftError::Storage(_))
      ));

      core.reset_store().unwrap();
      // The engine runs on a new, readable store.
      assert!(core.mention_settings().await.is_ok());
      // Kept as it was, beside the new database (its log, if any, goes along: engine tests).
      let kept = directory.files_containing(".unreadable-");
      assert_eq!(kept.len(), 1, "{kept:?}");
      assert!(
        kept[0]
          .to_string_lossy()
          .contains("alice.teamcraft.db.unreadable-")
      );
      assert_eq!(std::fs::read(&kept[0]).unwrap(), b"not a database");
      assert!(directory.store().exists());
      // A readable store is never reset.
      assert!(matches!(core.reset_store(), Err(TeamCraftError::InvalidInput(_))));
      assert_eq!(directory.files_containing(".unreadable-").len(), 1);
    }

    #[tokio::test]
    async fn a_store_its_key_does_not_open_is_damaged() {
      let directory = TestDir::new("wrong-key");
      let store = SealedSqliteStore::open(
        &directory.store(),
        &StaticKeySource::new(SymmetricKey::from_bytes([1; 32])),
      )
      .unwrap();
      store.put(Table::Users, "1", b"someone").unwrap();
      drop(store);

      let core = open(&directory, [2; 32]).unwrap();
      assert_eq!(
        core.account_state().await.unwrap(),
        AccountState::StoreUnreadable {
          problem: StoreProblem::Damaged
        }
      );
    }

    #[tokio::test]
    async fn a_readable_store_opens_and_is_never_reset() {
      let directory = TestDir::new("readable");
      let core = open(&directory, KEY).unwrap();
      assert!(core.mention_settings().await.is_ok());
      assert!(matches!(core.reset_store(), Err(TeamCraftError::InvalidInput(_))));
      assert!(directory.store().exists());
      assert!(directory.files_containing(".unreadable-").is_empty());
    }

    #[tokio::test]
    async fn sign_out_still_deletes_the_plaintext_files_of_an_unreadable_store() {
      let directory = TestDir::new("plaintext");
      std::fs::write(directory.store(), b"not a database").unwrap();
      let downloads = directory.path().join("downloads").join(ACCOUNT);
      let staged = directory.path().join("outgoing").join(ACCOUNT);
      for folder in [&downloads, &staged] {
        std::fs::create_dir_all(folder).unwrap();
        std::fs::write(folder.join("picture.jpg"), b"plaintext").unwrap();
      }
      let core = open(&directory, KEY).unwrap();

      core.delete_plaintext_files().await.unwrap();

      assert!(!downloads.exists());
      assert!(!staged.exists());
    }

    #[test]
    fn a_bad_key_or_account_name_is_still_refused() {
      let directory = TestDir::new("refused");
      assert!(matches!(
        open(&directory, [0; 31]),
        Err(TeamCraftError::InvalidInput(_))
      ));
      let config = AppConfig {
        account: "../bob".into(),
        ..config(&directory, KEY.to_vec())
      };
      assert!(matches!(
        TeamCraft::new(config, Arc::new(NoTokens), Arc::new(Quiet)),
        Err(TeamCraftError::InvalidInput(_))
      ));
    }
  }

  /// Issue #415: reading the account state drops the key pair of a setup given up on, but not
  /// while a link runs on this device.
  mod abandoned_setup {
    use teamcraft_core::records::{self, PendingDevice};
    use teamcraft_crypto::identity::DeviceKeyPair;
    use teamcraft_proto::{Id as _, Snowflake};

    use super::*;

    #[tokio::test]
    async fn is_dropped_when_the_state_is_read_unless_a_link_runs() {
      let directory = TestDir::new("abandoned");
      let store = SealedSqliteStore::open(
        &directory.store(),
        &StaticKeySource::new(SymmetricKey::from_bytes(KEY)),
      )
      .unwrap();
      let user = teamcraft_proto::UserId::from_snowflake(Snowflake::new(5).unwrap());
      let pending = PendingDevice {
        user_id: user,
        identity_public_key: [1; 32],
        device: DeviceKeyPair::generate(
          teamcraft_crypto::ids::UserId::new(5),
          teamcraft_crypto::ids::DeviceId::new(0),
          &mut rand_core::OsRng,
        ),
      };
      // Stored long before the expiry ran out.
      records::save_pending_device(&store, &pending, 0).unwrap();
      drop(store);

      let core = open(&directory, KEY).unwrap();
      core
        .start_device_link("Phone".into(), LinkPlatform::Android)
        .await
        .unwrap();
      // Signed out here: the state can't be read from the server, but the check ran before.
      assert!(core.account_state().await.is_err());
      assert!(has_pending_device(&directory), "kept while the link runs");
      drop(core);

      let core = open(&directory, KEY).unwrap();
      assert!(core.account_state().await.is_err());
      assert!(
        !has_pending_device(&directory),
        "dropped; its device is revoked once signed in"
      );
    }

    fn has_pending_device(directory: &TestDir) -> bool {
      let store = SealedSqliteStore::open(
        &directory.store(),
        &StaticKeySource::new(SymmetricKey::from_bytes(KEY)),
      )
      .unwrap();
      store.get(Table::Account, "pending_device").unwrap().is_some()
    }
  }

  fn open(directory: &TestDir, key: impl Into<Vec<u8>>) -> Result<Arc<TeamCraft>, TeamCraftError> {
    TeamCraft::new(config(directory, key.into()), Arc::new(NoTokens), Arc::new(Quiet))
  }

  fn config(directory: &TestDir, database_key: Vec<u8>) -> AppConfig {
    AppConfig {
      server_url: "https://chat.example.com".into(),
      data_dir: directory.path().display().to_string(),
      account: ACCOUNT.into(),
      database_key,
      push_gateway_url: None,
    }
  }

  /// Signed out: nothing here talks to a server.
  struct NoTokens;

  impl TokenProvider for NoTokens {
    fn access_token(&self) -> Option<String> {
      None
    }
  }

  struct Quiet;

  impl EventListener for Quiet {
    fn on_change(&self) {}
  }

  /// A fresh folder in the system's temporary directory, removed when dropped.
  struct TestDir(PathBuf);

  impl TestDir {
    fn new(name: &str) -> Self {
      let path = std::env::temp_dir().join(format!("teamcraft-store-{}-{name}", std::process::id()));
      let _ = std::fs::remove_dir_all(&path);
      std::fs::create_dir_all(&path).unwrap();
      Self(path)
    }

    fn path(&self) -> &Path {
      &self.0
    }

    fn store(&self) -> PathBuf {
      self.0.join(format!("{ACCOUNT}.teamcraft.db"))
    }

    fn files_containing(&self, text: &str) -> Vec<PathBuf> {
      std::fs::read_dir(&self.0)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.to_string_lossy().contains(text))
        .collect()
    }
  }

  impl Drop for TestDir {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.0);
    }
  }
}
