//! The task that owns the recorder bot's encrypted client (e2ee.md §8.3, #2273). It keeps the
//! gateway connection, so the bot receives welcomes and the call events of its conversations;
//! decides from the verified policy when to join a call; and posts each take into the call's
//! conversation through the core's upload queue.
//!
//! - **Joining** only while the owner-signed policy lets this account record
//!   ([`call_recording_allowance`]: it names the bot and recording is on), into calls people
//!   started in conversations the bot is a member of: the bot never starts a call. So it holds a
//!   call's keys only for calls it may record.
//! - **Leaving** when the policy no longer lets it record, when it leaves the conversation, and
//!   for the session's own reasons ([`StopReason`]). A call a host removed it from, or that it
//!   couldn't record (its keys never came, a full disk, a notice the server didn't confirm), isn't
//!   joined again; the conversation is told why.
//! - **Posting**: a take is decrypted into the upload staging folder and queued with its message;
//!   the core uploads it in parts, retries, and keeps it queued across restarts, then deletes the
//!   copy. The sealed take is deleted once the copy is queued.
//! - **Restarts**: a take's key lives only in the process, so a crash loses the take in progress.
//!   The store remembers the calls being recorded; at the next start the bot joins a call still
//!   running again and says that an earlier part was lost, and says so for a call that ended
//!   meanwhile.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use teamcraft_bot_runtime::call::CallAccess;
use teamcraft_bot_runtime::gateway::{GatewayKeeper, GatewayStep};
use teamcraft_bot_runtime::health::Liveness;
use teamcraft_bot_runtime::tickets::TicketFactory;
use teamcraft_bot_runtime::unix_ms;
use teamcraft_core::Client;
use teamcraft_core::api::{ApiError, ServerApi};
use teamcraft_core::client::files::{BlobUpload, StagedFile, sweep_staged_copies};
use teamcraft_core::records;
use teamcraft_core::store::{LocalStore, Table};
use teamcraft_core::workspace::call_recording_allowance;
use teamcraft_media::call_recording::{CallRecordingTake, delete_recording};
use teamcraft_media::call_resilience::JoinTicket;
use teamcraft_proto::{CallId, ConversationId, GatewayEvent};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::disk;
use crate::session::{
  FinishedTake, SessionEvent, SessionEvents, SessionHandle, SessionLimits, SessionOutcome, SessionParams,
  StopReason,
};
use crate::texts::{self, Texts};

/// How often the worker looks at the gateway, the policy and the calls.
const TICK: Duration = Duration::from_millis(500);
/// How often the outbox is flushed and the next upload started.
const FLUSH_INTERVAL: Duration = Duration::from_secs(1);
/// Each conversation's call is asked for at most this often: the gateway announces calls, this is
/// a backstop (and the check after a reconnect).
const CALL_CHECK_INTERVAL: Duration = Duration::from_secs(300);
/// A conversation whose call could not be joined is tried again after this long.
const START_RETRY: Duration = Duration::from_secs(30);
/// Store keys: a call being recorded, and the call not to join again, per conversation.
const SESSION_PREFIX: &str = "recorder/session/";
const SKIPPED_PREFIX: &str = "recorder/skipped/";
/// The posted file's type: Ogg Opus, as every app's recording.
const MEDIA_TYPE: &str = "audio/ogg";

/// What the worker takes from the configuration.
#[derive(Debug, Clone)]
pub struct WorkerSettings {
  pub limits: SessionLimits,
  /// Sealed takes.
  pub folder: PathBuf,
  /// Decrypted copies waiting for their upload.
  pub staging: PathBuf,
  pub min_free_bytes: u64,
  pub language: String,
  /// At shutdown, how long sessions get to finish their takes.
  pub shutdown_drain: Duration,
}

pub struct Worker<A: ServerApi, S: LocalStore> {
  client: Client<A, S>,
  settings: WorkerSettings,
  tickets: Arc<dyn TicketFactory>,
  gateway: GatewayKeeper<A::Gateway>,
  last_flush: Instant,
  call_events: mpsc::UnboundedReceiver<GatewayEvent>,
  /// Calls running in the bot's conversations, as the gateway said.
  running_calls: HashMap<ConversationId, CallId>,
  /// When the server was last asked about a conversation's call, or a start last failed.
  next_check: HashMap<ConversationId, Instant>,
  sessions: HashMap<ConversationId, SessionHandle>,
  session_sender: SessionEvents,
  session_events: mpsc::UnboundedReceiver<(ConversationId, SessionEvent)>,
  has_resumed: bool,
  /// The container health check's file, rewritten while the loop runs.
  maybe_liveness: Option<Liveness>,
}

/// A call the bot is recording, as the store keeps it: what a restart needs to tell the
/// conversation that a take was lost.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionRecord {
  call_id: CallId,
  /// When the bot joined (Unix milliseconds).
  started_at_ms: i64,
  /// The bot restarted during this call: the first take says an earlier part was lost.
  has_lost_earlier: bool,
}

impl<A: ServerApi, S: LocalStore> Worker<A, S> {
  pub fn new(mut client: Client<A, S>, settings: WorkerSettings, tickets: Arc<dyn TicketFactory>) -> Self {
    let (call_sender, call_events) = mpsc::unbounded_channel();
    client.set_call_event_sink(Box::new(move |event| {
      // The receiver lives as long as the worker.
      let _ = call_sender.send(event.clone());
    }));
    let (session_sender, session_events) = mpsc::unbounded_channel();
    let now = Instant::now();
    Self {
      client,
      settings,
      tickets,
      gateway: GatewayKeeper::new(now),
      last_flush: now,
      call_events,
      running_calls: HashMap::new(),
      next_check: HashMap::new(),
      sessions: HashMap::new(),
      session_sender,
      session_events,
      has_resumed: false,
      maybe_liveness: None,
    }
  }

  /// Rewrites `liveness` on every tick (the container health check reads it).
  pub fn with_liveness(mut self, liveness: Liveness) -> Self {
    self.maybe_liveness = Some(liveness);
    self
  }

  /// Runs until `stop` resolves, then lets the calls in progress finish their takes and queue
  /// them (for at most the drain time). Uploads go on at the next start.
  pub async fn run_until(mut self, stop: impl Future<Output = ()>) -> Client<A, S> {
    self.prepare_folders();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // One upload at a time, driven here so the loop stays responsive while a large take goes up.
    let mut maybe_upload: Option<BlobUpload> = None;
    tokio::pin!(stop);
    loop {
      tokio::select! {
        () = &mut stop => break,
        _ = tick.tick() => {
          self.on_tick().await;
          if maybe_upload.is_none() && self.gateway.is_connected() {
            maybe_upload = self.next_upload().await;
          }
        },
        Some(event) = self.call_events.recv() => self.on_call_event(event),
        Some((conversation, event)) = self.session_events.recv() => {
          self.on_session_event(conversation, event).await;
        },
        sent = upload_sent(&mut maybe_upload) => {
          if let Some(upload) = maybe_upload.take() {
            self.finish_upload(upload, sent).await;
          }
        },
      }
    }
    self.shut_down().await;
    self.client
  }

  /// The folders exist and hold nothing a crash left: sealed takes whose key is gone, copies no
  /// queued upload names.
  fn prepare_folders(&self) {
    for folder in [&self.settings.folder, &self.settings.staging] {
      if let Err(error) = create_private_dir(folder) {
        warn!(%error, path = %folder.display(), "a folder of the recorder can't be made");
      }
    }
    disk::sweep(&self.settings.folder);
    if let Err(error) = sweep_staged_copies(self.client.store(), &self.settings.staging) {
      warn!(%error, "the staged uploads can't be swept");
    }
  }

  async fn on_tick(&mut self) {
    let now = Instant::now();
    if let Some(liveness) = &mut self.maybe_liveness {
      liveness.touch(now);
    }
    match self.gateway.keep(&mut self.client, now).await {
      GatewayStep::Down => return,
      // Calls may have ended or begun while the connection was away: ask again.
      GatewayStep::Connected => {
        self.running_calls.clear();
        self.next_check.clear();
      },
      GatewayStep::Up => {},
    }
    if !self.has_resumed {
      self.has_resumed = true;
      self.resume_stored().await;
    }
    let me = self.client.user_id();
    let now_ms = unix_ms();
    let maybe_max_secs = call_recording_allowance(self.client.store(), me, now_ms);
    // Unreadable now: no session is stopped or started for it, the next tick reads again.
    let groups: HashSet<ConversationId> = match self.client.group_conversations() {
      Ok(conversations) => conversations.into_iter().collect(),
      Err(error) => {
        warn!(%error, "listing the conversations failed");
        return;
      },
    };
    self.check_sessions(maybe_max_secs.is_some(), &groups);
    if let Some(max_secs) = maybe_max_secs {
      self.start_sessions(max_secs, &groups).await;
    }
    if now.saturating_duration_since(self.last_flush) >= FLUSH_INTERVAL {
      self.last_flush = now;
      if let Err(error) = self.client.flush_outbox().await {
        debug!(%error, "flushing the outbox failed");
      }
    }
  }

  fn on_call_event(&mut self, event: GatewayEvent) {
    match event {
      GatewayEvent::CallStart {
        conversation_id,
        call_id,
        ..
      }
      | GatewayEvent::CallUpdate {
        conversation_id,
        call_id,
        ..
      } => {
        self.running_calls.insert(conversation_id, call_id);
      },
      GatewayEvent::CallEnd { conversation_id, .. } => {
        self.running_calls.remove(&conversation_id);
      },
      _ => {},
    }
  }

  /// Sessions leave when the bot may no longer record, or is no longer in the conversation.
  fn check_sessions(&mut self, may_record: bool, groups: &HashSet<ConversationId>) {
    for (conversation, session) in &self.sessions {
      if session.is_stopping() {
        continue;
      }
      let maybe_reason = if !may_record {
        Some(StopReason::Disabled)
      } else if !groups.contains(conversation) {
        Some(StopReason::NotMember)
      } else {
        None
      };
      if let Some(reason) = maybe_reason {
        info!(conversation_id = %conversation, ?reason, "asking the session to leave");
        session.stop(reason);
      }
    }
  }

  /// Joins the calls running in the bot's conversations.
  async fn start_sessions(&mut self, max_secs: u32, groups: &HashSet<ConversationId>) {
    for conversation in groups.iter().copied() {
      if self.sessions.contains_key(&conversation) {
        continue;
      }
      let Some(call_id) = self.known_call(conversation).await else {
        continue;
      };
      if self.skipped(conversation) == Some(call_id) {
        continue;
      }
      self.start_session(conversation, call_id, max_secs, false).await;
    }
  }

  /// The conversation's running call: as the gateway said, else from the server now and then.
  async fn known_call(&mut self, conversation: ConversationId) -> Option<CallId> {
    if let Some(call_id) = self.running_calls.get(&conversation) {
      return Some(*call_id);
    }
    let now = Instant::now();
    if self.next_check.get(&conversation).is_some_and(|next| now < *next) {
      return None;
    }
    self.next_check.insert(conversation, now + CALL_CHECK_INTERVAL);
    match self.client.running_call(conversation).await {
      Ok(Some(call)) => {
        self.running_calls.insert(conversation, call.call_id);
        Some(call.call_id)
      },
      Ok(None) => None,
      Err(error) => {
        debug!(%error, conversation_id = %conversation, "asking for the call failed");
        None
      },
    }
  }

  /// Joins `call_id`, if people are in it, the disk has room and the call can be prepared.
  async fn start_session(
    &mut self,
    conversation: ConversationId,
    call_id: CallId,
    max_secs: u32,
    has_lost_earlier: bool,
  ) {
    let me = self.client.user_id();
    // Check with the server that the call runs and has people: a join token alone would start a
    // call, which the recorder never does.
    match self.client.running_call(conversation).await {
      Ok(Some(call))
        if call.call_id == call_id && call.participants.iter().any(|entry| entry.user_id != me) => {},
      Ok(_) => {
        self.running_calls.remove(&conversation);
        return;
      },
      Err(error) => {
        debug!(%error, conversation_id = %conversation, "asking for the call failed");
        return;
      },
    }
    if !disk::has_room(&self.settings.folder, self.settings.min_free_bytes) {
      warn!(conversation_id = %conversation, "too little free space to record a call");
      self.skip(conversation, call_id);
      self.post_text(conversation, "disk-full").await;
      return;
    }
    let preparation = match self.client.prepare_call(conversation).await {
      Ok(preparation) => preparation,
      Err(error) => {
        warn!(%error, conversation_id = %conversation, "preparing the call failed");
        self.delay_start(conversation);
        return;
      },
    };
    if preparation.join.call_id != call_id {
      // The call ended and another began meanwhile; the next tick looks again.
      self.running_calls.remove(&conversation);
      return;
    }
    let params = SessionParams {
      conversation,
      call_id,
      user_id: preparation.user_id,
      device_id: preparation.device_id,
      max_take: Duration::from_secs(u64::from(max_secs)),
      folder: self.settings.folder.clone(),
      limits: self.settings.limits,
    };
    let access = CallAccess {
      ticket: JoinTicket::from(preparation.join),
      signer: preparation.signer,
      devices: preparation.devices,
      tickets: self.tickets.for_conversation(conversation),
    };
    let config = match crate::session::native_config(&params, access) {
      Ok(config) => config,
      Err(error) => {
        warn!(%error, conversation_id = %conversation, "the call's keys can't be bound to this device");
        self.delay_start(conversation);
        return;
      },
    };
    let record = SessionRecord {
      call_id,
      started_at_ms: unix_ms(),
      has_lost_earlier,
    };
    self.save(&session_key(conversation), &record);
    info!(conversation_id = %conversation, call_id = %call_id, "joining a call to record it");
    let handle = crate::session::start(params, config, self.session_sender.clone());
    self.sessions.insert(conversation, handle);
  }

  async fn on_session_event(&mut self, conversation: ConversationId, event: SessionEvent) {
    match event {
      SessionEvent::Take(finished) => self.post_take(conversation, finished).await,
      SessionEvent::Ended(outcome) => {
        self.sessions.remove(&conversation);
        self.on_ended(conversation, outcome).await;
      },
    }
  }

  /// Decrypts the take into the staging folder and queues it with its message; the sealed take is
  /// deleted either way.
  async fn post_take(&mut self, conversation: ConversationId, finished: FinishedTake) {
    let maybe_record = self.record(conversation);
    let has_lost_earlier = maybe_record
      .as_ref()
      .is_some_and(|record| record.has_lost_earlier);
    let call_id = maybe_record.map_or_else(|| "call".to_owned(), |record| record.call_id.to_string());
    let maybe_part = (finished.is_at_limit || finished.part > 1).then_some(finished.part);
    let duration_ms = finished.take.duration_ms;
    let file_name = format!("call-recording-{call_id}-{}.ogg", finished.part);
    let staging = self.settings.staging.clone();
    let staged = tokio::task::spawn_blocking(move || stage_take(finished.take, &staging)).await;
    let (path, size_bytes) = match staged {
      Ok(Ok(staged)) => staged,
      Ok(Err(error)) => {
        warn!(%error, conversation_id = %conversation, "a take could not be decrypted for posting");
        self.post_text(conversation, "write-failed").await;
        return;
      },
      Err(error) => {
        warn!(%error, conversation_id = %conversation, "decrypting a take failed");
        return;
      },
    };
    if !self.client.has_group(conversation).unwrap_or(false) {
      warn!(conversation_id = %conversation, "not in the conversation any more; the take can't be posted");
      remove_file(&path);
      return;
    }
    let message = texts::take_message(&self.texts(), duration_ms, maybe_part, has_lost_earlier);
    let file = StagedFile {
      path: path.clone(),
      file_name,
      media_type: MEDIA_TYPE.to_owned(),
      size_bytes,
      maybe_duration_ms: Some(duration_ms),
    };
    match self.client.queue_staged_file(conversation, &message, file) {
      Ok(_) => {
        info!(conversation_id = %conversation, duration_secs = duration_ms / 1000, "queued a take for posting");
        if has_lost_earlier {
          self.update_record(conversation, |record| record.has_lost_earlier = false);
        }
      },
      Err(error) => {
        warn!(%error, conversation_id = %conversation, "a take could not be queued for posting");
        remove_file(&path);
      },
    }
  }

  /// A session ended: the conversation hears why the bot couldn't record, and a call it shouldn't
  /// try again is remembered.
  async fn on_ended(&mut self, conversation: ConversationId, outcome: SessionOutcome) {
    self.delete(&session_key(conversation));
    let maybe_text = match &outcome.reason {
      StopReason::RemovedByHost => Some("removed"),
      StopReason::JoinTimeout if !outcome.was_secured => Some("join-failed"),
      StopReason::NoticeUnconfirmed => Some("notice-unconfirmed"),
      StopReason::WriteFailed(_) => Some("write-failed"),
      StopReason::ConnectionLost(_) => {
        self.delay_start(conversation);
        None
      },
      StopReason::Disabled
      | StopReason::NotMember
      | StopReason::EndedByHost
      | StopReason::Alone
      | StopReason::Shutdown
      | StopReason::JoinTimeout => None,
    };
    info!(conversation_id = %conversation, reason = ?outcome.reason, "a recorded call ended");
    if let Some(key) = maybe_text {
      self.skip(conversation, outcome.call_id);
      self.post_text(conversation, key).await;
    }
  }

  /// After a restart: the take in progress died with the process. A call still running is joined
  /// again, and its first take says that an earlier part was lost; for the others, the conversation
  /// is told now.
  async fn resume_stored(&mut self) {
    let me = self.client.user_id();
    let may_record = call_recording_allowance(self.client.store(), me, unix_ms());
    for (conversation, record) in self.records() {
      let is_running = matches!(
        self.client.running_call(conversation).await,
        Ok(Some(call)) if call.call_id == record.call_id
      );
      match (may_record, is_running) {
        (Some(max_secs), true) => {
          info!(conversation_id = %conversation, "joining the call again after a restart");
          self.delete(&session_key(conversation));
          self
            .start_session(conversation, record.call_id, max_secs, true)
            .await;
        },
        _ => {
          info!(conversation_id = %conversation, "a call's take was lost to a restart");
          self.delete(&session_key(conversation));
          self.post_text(conversation, "lost-at-restart").await;
        },
      }
    }
  }

  /// Stops the sessions and lets them finish and queue their takes within the drain time.
  async fn shut_down(&mut self) {
    for session in self.sessions.values() {
      session.stop(StopReason::Shutdown);
    }
    let deadline = tokio::time::Instant::now() + self.settings.shutdown_drain;
    while !self.sessions.is_empty() {
      tokio::select! {
        () = tokio::time::sleep_until(deadline) => {
          warn!("calls still finishing at shutdown; their takes are lost");
          break;
        },
        Some((conversation, event)) = self.session_events.recv() => {
          self.on_session_event(conversation, event).await;
        },
      }
    }
    if let Err(error) = self.client.flush_outbox().await {
      warn!(%error, "flushing the outbox at shutdown failed");
    }
  }

  /// The next queued file to upload, if any.
  async fn next_upload(&mut self) -> Option<BlobUpload> {
    match self.client.start_upload().await {
      Ok(maybe_upload) => maybe_upload,
      Err(error) => {
        debug!(%error, "starting an upload failed; trying again");
        None
      },
    }
  }

  async fn finish_upload(&mut self, upload: BlobUpload, sent: Result<(), ApiError>) {
    match self.client.finish_upload(upload, sent).await {
      Ok(step) => debug!(?step, "upload step"),
      Err(error) => debug!(%error, "an upload step failed; it is retried"),
    }
    if let Err(error) = self.client.flush_outbox().await {
      debug!(%error, "flushing the outbox failed");
    }
  }

  async fn post_text(&mut self, conversation: ConversationId, key: &str) {
    let text = self.texts().get(key);
    if let Err(error) = self.client.send_text(conversation, None, &text).await {
      // The text is in the outbox and goes out with a later flush.
      debug!(%error, conversation_id = %conversation, "a notice is delayed");
    }
  }

  fn texts(&self) -> Texts {
    texts::for_language(&self.settings.language)
  }

  fn delay_start(&mut self, conversation: ConversationId) {
    self.next_check.insert(conversation, Instant::now() + START_RETRY);
    self.running_calls.remove(&conversation);
  }

  fn skip(&self, conversation: ConversationId, call_id: CallId) {
    self.save(&skipped_key(conversation), &call_id);
  }

  fn skipped(&self, conversation: ConversationId) -> Option<CallId> {
    records::get_json(self.client.store(), Table::SyncState, &skipped_key(conversation))
      .ok()
      .flatten()
  }

  fn record(&self, conversation: ConversationId) -> Option<SessionRecord> {
    match records::get_json(self.client.store(), Table::SyncState, &session_key(conversation)) {
      Ok(maybe_record) => maybe_record,
      Err(error) => {
        warn!(%error, "a call's record is unreadable");
        None
      },
    }
  }

  fn records(&self) -> Vec<(ConversationId, SessionRecord)> {
    match records::list_json_with_prefix::<SessionRecord>(
      self.client.store(),
      Table::SyncState,
      SESSION_PREFIX,
    ) {
      Ok(records) => records
        .into_iter()
        .filter_map(|(key, record)| {
          let conversation = key.strip_prefix(SESSION_PREFIX)?.parse().ok()?;
          Some((conversation, record))
        })
        .collect(),
      Err(error) => {
        warn!(%error, "the calls' records are unreadable");
        Vec::new()
      },
    }
  }

  fn update_record(&mut self, conversation: ConversationId, change: impl FnOnce(&mut SessionRecord)) {
    if let Some(mut record) = self.record(conversation) {
      change(&mut record);
      self.save(&session_key(conversation), &record);
    }
  }

  fn save<T: Serialize>(&self, key: &str, value: &T) {
    if let Err(error) = records::put_json(self.client.store(), Table::SyncState, key, value) {
      warn!(%error, "writing to the store failed");
    }
  }

  fn delete(&self, key: &str) {
    if let Err(error) = self.client.store().delete(Table::SyncState, key) {
      warn!(%error, "deleting from the store failed");
    }
  }
}

/// Waits for the running upload's request; never resolves without one.
async fn upload_sent(maybe_upload: &mut Option<BlobUpload>) -> Result<(), ApiError> {
  match maybe_upload {
    Some(upload) => upload.sent().await,
    None => std::future::pending().await,
  }
}

/// Decrypts `take` into a new file in `staging`, synced, and deletes the sealed take (its key goes
/// with it). Returns the copy and its size. A copy that fails is removed.
fn stage_take(take: CallRecordingTake, staging: &Path) -> std::io::Result<(PathBuf, u64)> {
  let result = write_copy(&take, staging);
  delete_recording(&take.path);
  result
}

fn write_copy(take: &CallRecordingTake, staging: &Path) -> std::io::Result<(PathBuf, u64)> {
  create_private_dir(staging)?;
  let (path, file) = new_staging_file(staging)?;
  let mut output = BufWriter::new(file);
  let result = take
    .decrypt_to(&mut output)
    .map_err(std::io::Error::other)
    .and_then(|size| {
      output.flush()?;
      output.get_ref().sync_all()?;
      Ok(size)
    });
  match result {
    Ok(size) => Ok((path, size)),
    Err(error) => {
      drop(output);
      remove_file(&path);
      Err(error)
    },
  }
}

/// A new file in `staging` under a name nobody else has, readable by this user only.
fn new_staging_file(staging: &Path) -> std::io::Result<(PathBuf, File)> {
  let stamp = unix_ms();
  for attempt in 0..100u32 {
    let path = staging.join(format!("recording-{stamp}-{attempt}.ogg"));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
      use std::os::unix::fs::OpenOptionsExt;
      options.mode(0o600);
    }
    match options.open(&path) {
      Ok(file) => return Ok((path, file)),
      Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {},
      Err(error) => return Err(error),
    }
  }
  Err(std::io::Error::other("no free name in the staging folder"))
}

/// Makes `folder` (and its parents), readable by this user only.
fn create_private_dir(folder: &Path) -> std::io::Result<()> {
  fs::create_dir_all(folder)?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(folder, fs::Permissions::from_mode(0o700))?;
  }
  Ok(())
}

fn remove_file(path: &Path) {
  if let Err(error) = fs::remove_file(path) {
    debug!(%error, "a staged copy could not be deleted");
  }
}

fn session_key(conversation: ConversationId) -> String {
  format!("{SESSION_PREFIX}{conversation}")
}

fn skipped_key(conversation: ConversationId) -> String {
  format!("{SKIPPED_PREFIX}{conversation}")
}
