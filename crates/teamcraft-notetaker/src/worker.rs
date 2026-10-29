//! The task that owns the encrypted client (e2ee.md §8.2). It keeps the gateway connection, so
//! the bot receives welcomes, the members' requests and the call events of its conversations;
//! decides from the verified policy and the requests when to join a call; keeps the transcript of
//! every call it is in in its encrypted store; and posts the join notice and the result.
//!
//! - **Joining** only when a request allows it ([`NotetakerLedger::join_in_force`]), the policy
//!   names this bot and is on, and a call people started is running: the notetaker never starts a
//!   call.
//! - **Leaving** on a stop request, when the policy turns it off, when it leaves the conversation,
//!   and for the session's own reasons ([`StopReason`]).
//! - **Restarts**: the store keeps each call's transcript as it grows and the result until it is
//!   posted; at the next start a call still running is joined again and the transcript continues,
//!   the others are finished and posted.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use teamcraft_core::api::{GatewayConnection, ServerApi};
use teamcraft_core::client::OutgoingFile;
use teamcraft_core::notetaker::{JoinRequest, NotetakerLedger, notetaker_policy};
use teamcraft_core::records;
use teamcraft_core::store::{LocalStore, Table};
use teamcraft_core::{Client, CoreError};
use teamcraft_media::call_native::JoinTicketSource;
use teamcraft_media::call_resilience::JoinTicket;
use teamcraft_proto::{
  CallId, ConversationId, GatewayClientMessage, GatewayEvent, NotetakerPolicy, NotetakerScope, UserId,
};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

use crate::capture::CaptureSettings;
use crate::keys::{Credentials, KeyError};
use crate::openrouter::{OpenRouterClient, OpenRouterError, Usage};
use crate::report::{ReportInput, compose, joined_notice};
use crate::session::{
  CallAccess, SessionEvent, SessionEvents, SessionHandle, SessionLimits, SessionOutcome, SessionParams,
  StopReason,
};
use crate::summary::{SummaryInput, SummarySettings, summarize};
use crate::texts::Texts;
use crate::transcript::{Names, Transcript};

/// How often the worker looks at the gateway, the requests and the calls.
const TICK: Duration = Duration::from_millis(500);
/// Keep-alive for the gateway (the server expects one every 30 seconds).
const HEARTBEAT: Duration = Duration::from_secs(20);
const RECONNECT_BACKOFF_MIN: Duration = Duration::from_secs(1);
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(30);
/// While a request waits and no call is known, the server is asked this often.
const CALL_CHECK_INTERVAL: Duration = Duration::from_secs(15);
/// A conversation whose call could not be joined is tried again after this long.
const START_RETRY: Duration = Duration::from_secs(30);
/// Store keys: a call in progress or not posted yet, and what was served last, per conversation.
const SESSION_PREFIX: &str = "notetaker/session/";
const SERVED_PREFIX: &str = "notetaker/served/";

/// What the worker takes from the configuration.
#[derive(Debug, Clone, Copy)]
pub struct WorkerSettings {
  pub capture: CaptureSettings,
  pub limits: SessionLimits,
  pub summary: SummarySettings,
  /// At shutdown, how long sessions get to finish and post.
  pub shutdown_drain: Duration,
}

/// Fresh join tickets for a conversation's call, for rejoining after a lost connection.
pub trait TicketFactory: Send + Sync {
  fn for_conversation(&self, conversation: ConversationId) -> Arc<dyn JoinTicketSource>;
}

pub struct Worker<A: ServerApi, S: LocalStore> {
  client: Client<A, S>,
  settings: WorkerSettings,
  credentials: Credentials,
  tickets: Arc<dyn TicketFactory>,
  /// OpenRouter requests at the same time, all calls together.
  limiter: Arc<Semaphore>,
  gateway: Option<A::Gateway>,
  next_connect: Instant,
  backoff: Duration,
  last_heartbeat: Instant,
  last_flush: Instant,
  call_events: mpsc::UnboundedReceiver<GatewayEvent>,
  /// Calls running in the bot's conversations, as the gateway said.
  running_calls: HashMap<ConversationId, CallId>,
  /// When the server was last asked about a conversation's call, or a start last failed.
  next_check: HashMap<ConversationId, Instant>,
  sessions: HashMap<ConversationId, ActiveSession>,
  session_sender: SessionEvents,
  session_events: mpsc::UnboundedReceiver<(ConversationId, SessionEvent)>,
  reports: JoinSet<FinishedReport>,
  has_resumed: bool,
}

struct ActiveSession {
  handle: SessionHandle,
  join_at_ms: i64,
}

/// A call the notetaker is in or hasn't posted yet, as the store keeps it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionRecord {
  conversation: ConversationId,
  call_id: CallId,
  /// The request it joined for (its server time).
  join_at_ms: i64,
  /// When the notetaker joined (Unix milliseconds).
  started_at_ms: i64,
  /// The policy when it joined: models and language stay those of the notice.
  policy: NotetakerPolicy,
  has_announced: bool,
  transcript: Transcript,
  /// The result, composed and waiting to be posted.
  #[serde(rename = "report", default, skip_serializing_if = "Option::is_none")]
  maybe_report: Option<StoredReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredReport {
  body: String,
  #[serde(rename = "file_name", default, skip_serializing_if = "Option::is_none")]
  maybe_file_name: Option<String>,
  #[serde(rename = "file_markdown", default, skip_serializing_if = "Option::is_none")]
  maybe_file_markdown: Option<String>,
}

/// What was served last in a conversation, so a request isn't served twice.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct Served {
  /// The newest `this_call` request served (its server time).
  #[serde(rename = "this_call_at_ms", default, skip_serializing_if = "Option::is_none")]
  maybe_this_call_at_ms: Option<i64>,
  /// The last call joined.
  #[serde(rename = "last_call", default, skip_serializing_if = "Option::is_none")]
  maybe_last_call: Option<CallId>,
}

/// A summary written in the background.
struct FinishedReport {
  conversation: ConversationId,
  report: StoredReport,
  usage: Usage,
  is_key_refused: bool,
}

impl<A: ServerApi, S: LocalStore> Worker<A, S> {
  pub fn new(
    mut client: Client<A, S>,
    settings: WorkerSettings,
    credentials: Credentials,
    tickets: Arc<dyn TicketFactory>,
    max_concurrent_requests: usize,
  ) -> Self {
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
      credentials,
      tickets,
      limiter: Arc::new(Semaphore::new(max_concurrent_requests.max(1))),
      gateway: None,
      next_connect: now,
      backoff: RECONNECT_BACKOFF_MIN,
      last_heartbeat: now,
      last_flush: now,
      call_events,
      running_calls: HashMap::new(),
      next_check: HashMap::new(),
      sessions: HashMap::new(),
      session_sender,
      session_events,
      reports: JoinSet::new(),
      has_resumed: false,
    }
  }

  /// Runs until `stop` resolves, then lets the calls in progress finish and post (for at most the
  /// drain time); what is left is posted at the next start.
  pub async fn run_until(mut self, stop: impl Future<Output = ()>) -> Client<A, S> {
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tokio::pin!(stop);
    loop {
      tokio::select! {
        () = &mut stop => break,
        _ = tick.tick() => self.on_tick().await,
        Some(event) = self.call_events.recv() => self.on_call_event(event),
        Some((conversation, event)) = self.session_events.recv() => {
          self.on_session_event(conversation, event).await;
        },
        Some(done) = self.reports.join_next(), if !self.reports.is_empty() => match done {
          Ok(finished) => self.on_report(finished).await,
          Err(error) => warn!(%error, "writing a summary failed"),
        },
      }
    }
    self.shut_down().await;
    self.client
  }

  async fn on_tick(&mut self) {
    let now = Instant::now();
    self.keep_gateway(now).await;
    if self.gateway.is_none() {
      return;
    }
    if !self.has_resumed {
      self.has_resumed = true;
      self.resume_stored().await;
    }
    let me = self.client.user_id();
    let policy = notetaker_policy(self.client.store()).unwrap_or_default();
    let is_active = policy.is_notetaker(me);
    let now_ms = unix_ms();
    let groups: HashSet<ConversationId> = self
      .client
      .group_conversations()
      .unwrap_or_default()
      .into_iter()
      .collect();
    self.check_sessions(is_active, &groups);
    if is_active {
      self.start_sessions(&policy, now_ms).await;
    }
    if now.saturating_duration_since(self.last_flush) >= Duration::from_secs(1) {
      self.last_flush = now;
      self.retry_posts().await;
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

  /// Sessions leave when they must: a stop request after their join, the policy off, the bot no
  /// longer in the conversation.
  fn check_sessions(&mut self, is_active: bool, groups: &HashSet<ConversationId>) {
    let mut stops = Vec::new();
    for (conversation, session) in &self.sessions {
      if session.handle.is_stopping() {
        continue;
      }
      let maybe_reason = if !is_active {
        Some(StopReason::Disabled)
      } else if !groups.contains(conversation) {
        Some(StopReason::Requested)
      } else if NotetakerLedger::load(self.client.store(), *conversation)
        .is_ok_and(|ledger| ledger.is_stopped_after(session.join_at_ms))
      {
        Some(StopReason::Requested)
      } else {
        None
      };
      if let Some(reason) = maybe_reason {
        stops.push((*conversation, reason));
      }
    }
    for (conversation, reason) in stops {
      if let Some(session) = self.sessions.get(&conversation) {
        info!(conversation_id = %conversation, ?reason, "asking the session to leave");
        session.handle.stop(reason);
      }
    }
  }

  /// Joins the calls that requests ask for.
  async fn start_sessions(&mut self, policy: &NotetakerPolicy, now_ms: i64) {
    let conversations = match self.client.group_conversations() {
      Ok(conversations) => conversations,
      Err(error) => {
        warn!(%error, "listing the conversations failed");
        return;
      },
    };
    for conversation in conversations {
      if self.sessions.contains_key(&conversation) || self.has_unposted(conversation) {
        continue;
      }
      let served = self.served(conversation);
      let maybe_join = NotetakerLedger::load(self.client.store(), conversation)
        .ok()
        .and_then(|ledger| ledger.join_in_force(now_ms, served.maybe_this_call_at_ms));
      let Some(join) = maybe_join else {
        continue;
      };
      let Some(call_id) = self.known_call(conversation).await else {
        continue;
      };
      if join.scope == NotetakerScope::EveryCall && served.maybe_last_call == Some(call_id) {
        continue;
      }
      self
        .start_session(conversation, call_id, join, policy, None)
        .await;
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

  /// Joins `call_id`, if people are in it, the key is usable and the call can be prepared.
  /// `maybe_resumed`: the record of an earlier run of this session.
  async fn start_session(
    &mut self,
    conversation: ConversationId,
    call_id: CallId,
    join: JoinRequest,
    policy: &NotetakerPolicy,
    maybe_resumed: Option<SessionRecord>,
  ) {
    let me = self.client.user_id();
    // Check with the server that the call runs and has people: a join token alone would start a
    // call, which the notetaker never does.
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
    let openrouter = match self.credentials.client(policy).await {
      Ok(client) => client,
      Err(_) => {
        self.delay_start(conversation);
        return;
      },
    };
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
    let mut served = self.served(conversation);
    served.maybe_last_call = Some(call_id);
    if join.scope == NotetakerScope::ThisCall {
      served.maybe_this_call_at_ms = Some(join.at_ms);
    }
    self.save(&served_key(conversation), &served);
    let now_ms = unix_ms();
    let record = maybe_resumed.unwrap_or_else(|| SessionRecord {
      conversation,
      call_id,
      join_at_ms: join.at_ms,
      started_at_ms: now_ms,
      policy: policy.clone(),
      has_announced: false,
      transcript: Transcript::default(),
      maybe_report: None,
    });
    self.save(&session_key(conversation), &record);
    let prior_elapsed = Duration::from_millis(u64::try_from(now_ms - record.started_at_ms).unwrap_or(0));
    let params = SessionParams {
      conversation,
      call_id,
      user_id: preparation.user_id,
      device_id: preparation.device_id,
      policy: record.policy.clone(),
      capture: self.settings.capture,
      limits: self.settings.limits,
      prior: record.transcript.clone(),
      prior_elapsed,
    };
    let access = CallAccess {
      ticket: JoinTicket::from(preparation.join),
      signer: preparation.signer,
      devices: preparation.devices,
      tickets: self.tickets.for_conversation(conversation),
    };
    info!(conversation_id = %conversation, call_id = %call_id, requested_by = %join.by, "joining a call");
    let handle = crate::session::start(
      params,
      access,
      openrouter,
      Arc::clone(&self.limiter),
      self.session_sender.clone(),
    );
    self.sessions.insert(
      conversation,
      ActiveSession {
        handle,
        join_at_ms: record.join_at_ms,
      },
    );
  }

  async fn on_session_event(&mut self, conversation: ConversationId, event: SessionEvent) {
    match event {
      SessionEvent::Secured => self.announce(conversation).await,
      SessionEvent::Segment(segment) => {
        self.update_record(conversation, |record| record.transcript.add(segment))
      },
      SessionEvent::PieceFailed => {
        self.update_record(conversation, |record| record.transcript.failed_pieces += 1)
      },
      SessionEvent::Ended(outcome) => {
        self.sessions.remove(&conversation);
        self.finish(*outcome).await;
      },
    }
  }

  /// Posts the join notice once per call.
  async fn announce(&mut self, conversation: ConversationId) {
    let Some(record) = self.record(conversation) else {
      return;
    };
    if record.has_announced {
      return;
    }
    let texts = Texts::for_language(&record.policy.language);
    let notice = joined_notice(&texts, &record.policy);
    if let Err(error) = self.client.send_text(conversation, None, &notice).await {
      // The text is in the outbox and goes out with a later flush.
      debug!(%error, conversation_id = %conversation, "the join notice is delayed");
    }
    self.update_record(conversation, |record| record.has_announced = true);
  }

  /// A session ended: a call it never heard gets a short notice, the others a summary.
  async fn finish(&mut self, outcome: SessionOutcome) {
    let conversation = outcome.conversation;
    let Some(mut record) = self.record(conversation) else {
      warn!(conversation_id = %conversation, "a session ended without its record");
      return;
    };
    record.transcript = outcome.transcript.clone();
    if !outcome.was_secured && outcome.transcript.is_empty() {
      if outcome.reason == StopReason::JoinTimeout {
        let texts = Texts::for_language(&record.policy.language);
        if let Err(error) = self
          .client
          .send_text(conversation, None, &texts.get("join-failed"))
          .await
        {
          debug!(%error, "the notice is delayed");
        }
      }
      warn!(conversation_id = %conversation, reason = ?outcome.reason, "never heard the call");
      self.delete(&session_key(conversation));
      return;
    }
    self.save(&session_key(conversation), &record);
    let names = self.names(conversation);
    let maybe_openrouter = self.credentials.client(&record.policy).await.ok();
    let minutes = outcome.elapsed.as_secs().div_ceil(60);
    let reason = outcome.reason.clone();
    let settings = self.settings.summary;
    self.reports.spawn(write_report(
      record,
      names,
      maybe_openrouter,
      minutes,
      reason,
      outcome.usage,
      settings,
    ));
  }

  async fn on_report(&mut self, finished: FinishedReport) {
    if finished.is_key_refused {
      self.credentials.forget();
    }
    info!(
      conversation_id = %finished.conversation,
      requests = finished.usage.requests,
      audio_seconds = finished.usage.audio_seconds,
      input_tokens = finished.usage.input_tokens,
      output_tokens = finished.usage.output_tokens,
      cost_usd = finished.usage.cost_usd,
      "the call's notes are written"
    );
    let conversation = finished.conversation;
    self.update_record(conversation, |record| record.maybe_report = Some(finished.report));
    self.post(conversation).await;
  }

  /// Posts a record's result; the record goes once it is posted.
  async fn post(&mut self, conversation: ConversationId) {
    let Some(record) = self.record(conversation) else {
      return;
    };
    let Some(report) = record.maybe_report else {
      return;
    };
    if !self.client.has_group(conversation).unwrap_or(false) {
      warn!(conversation_id = %conversation, "not in the conversation any more; the notes can't be posted");
      self.delete(&session_key(conversation));
      return;
    }
    let sent = match (report.maybe_file_name, report.maybe_file_markdown) {
      (Some(file_name), Some(markdown)) => self
        .client
        .send_text_with_file(
          conversation,
          &report.body,
          OutgoingFile {
            file_name,
            media_type: "text/markdown".to_owned(),
            content: markdown.into_bytes(),
          },
        )
        .await
        .map(drop),
      _ => self
        .client
        .send_text(conversation, None, &report.body)
        .await
        .map(drop),
    };
    match sent {
      Ok(()) => {
        info!(conversation_id = %conversation, "posted the call's notes");
        self.delete(&session_key(conversation));
      },
      // An upload that failed is tried again; a text that failed is in the outbox already.
      Err(CoreError::Api(error)) if error.is_unreachable() => {
        debug!(%error, "posting the notes is delayed");
      },
      Err(error) => {
        warn!(%error, conversation_id = %conversation, "posting the notes failed; trying again");
      },
    }
  }

  /// Results whose posting failed before.
  async fn retry_posts(&mut self) {
    let waiting: Vec<ConversationId> = self
      .records()
      .into_iter()
      .filter(|record| record.maybe_report.is_some() && !self.sessions.contains_key(&record.conversation))
      .map(|record| record.conversation)
      .collect();
    for conversation in waiting {
      self.post(conversation).await;
    }
  }

  /// After a restart: calls still running are joined again (the transcript continues), the
  /// others are finished from what the store has.
  async fn resume_stored(&mut self) {
    let policy = notetaker_policy(self.client.store()).unwrap_or_default();
    let is_active = policy.is_notetaker(self.client.user_id());
    let now_ms = unix_ms();
    for record in self.records() {
      if record.maybe_report.is_some() {
        continue;
      }
      let conversation = record.conversation;
      let is_running = matches!(
        self.client.running_call(conversation).await,
        Ok(Some(call)) if call.call_id == record.call_id
      );
      let maybe_join = NotetakerLedger::load(self.client.store(), conversation)
        .ok()
        .filter(|ledger| !ledger.is_stopped_after(record.join_at_ms))
        .map(|_| JoinRequest {
          by: self.client.user_id(),
          scope: NotetakerScope::EveryCall,
          at_ms: record.join_at_ms,
        });
      match (is_active && is_running, maybe_join) {
        (true, Some(join)) => {
          info!(conversation_id = %conversation, "joining the call again after a restart");
          let call_id = record.call_id;
          self
            .start_session(conversation, call_id, join, &policy, Some(record))
            .await;
        },
        _ => {
          info!(conversation_id = %conversation, "finishing a call from before a restart");
          let elapsed = Duration::from_millis(u64::try_from(now_ms - record.started_at_ms).unwrap_or(0));
          let outcome = SessionOutcome {
            conversation,
            call_id: record.call_id,
            reason: StopReason::Shutdown,
            transcript: record.transcript.clone(),
            usage: Usage::default(),
            elapsed: elapsed.min(Duration::from_secs(self.settings.limits.max_call.as_secs())),
            was_secured: true,
          };
          self.finish(outcome).await;
        },
      }
    }
  }

  /// Stops the sessions and lets them finish and post within the drain time.
  async fn shut_down(&mut self) {
    for session in self.sessions.values() {
      session.handle.stop(StopReason::Shutdown);
    }
    let deadline = tokio::time::Instant::now() + self.settings.shutdown_drain;
    while !self.sessions.is_empty() || !self.reports.is_empty() {
      tokio::select! {
        () = tokio::time::sleep_until(deadline) => {
          warn!("calls still finishing at shutdown; they are posted at the next start");
          break;
        },
        Some((conversation, event)) = self.session_events.recv() => {
          self.on_session_event(conversation, event).await;
        },
        Some(done) = self.reports.join_next(), if !self.reports.is_empty() => {
          if let Ok(finished) = done {
            self.on_report(finished).await;
          }
        },
      }
    }
    if let Err(error) = self.client.flush_outbox().await {
      warn!(%error, "flushing the outbox at shutdown failed");
    }
  }

  /// Connects (with backoff), processes what the server sent, and sends heartbeats.
  async fn keep_gateway(&mut self, now: Instant) {
    if self.gateway.is_none() {
      if now < self.next_connect {
        return;
      }
      match self.client.connect_gateway().await {
        Ok((connection, outcome)) => {
          info!(?outcome, "gateway connected");
          self.gateway = Some(connection);
          self.backoff = RECONNECT_BACKOFF_MIN;
          self.last_heartbeat = now;
          // Calls may have ended while the connection was away.
          self.running_calls.clear();
        },
        Err(error) => {
          warn!(%error, retry_in = ?self.backoff, "gateway connection failed");
          self.next_connect = now + self.backoff;
          self.backoff = (self.backoff * 2).min(RECONNECT_BACKOFF_MAX);
          return;
        },
      }
    }
    let Some(connection) = self.gateway.as_mut() else {
      return;
    };
    if let Err(error) = self.client.pump(connection).await {
      warn!(%error, "gateway connection lost");
      self.gateway = None;
      self.next_connect = now + RECONNECT_BACKOFF_MIN;
      return;
    }
    if now.saturating_duration_since(self.last_heartbeat) >= HEARTBEAT {
      self.last_heartbeat = now;
      if let Some(connection) = self.gateway.as_mut()
        && let Err(error) = connection.send(&GatewayClientMessage::Heartbeat).await
      {
        warn!(%error, "heartbeat failed");
        self.gateway = None;
      }
    }
  }

  fn delay_start(&mut self, conversation: ConversationId) {
    self.next_check.insert(conversation, Instant::now() + START_RETRY);
    self.running_calls.remove(&conversation);
  }

  fn has_unposted(&self, conversation: ConversationId) -> bool {
    self.record(conversation).is_some()
  }

  fn names(&mut self, conversation: ConversationId) -> HashMap<UserId, String> {
    match self.client.group_member_names(conversation) {
      Ok(names) => names
        .into_iter()
        .filter_map(|(user, maybe_name)| maybe_name.map(|name| (user, name)))
        .collect(),
      Err(error) => {
        warn!(%error, "the members' names are unreadable");
        HashMap::new()
      },
    }
  }

  fn served(&self, conversation: ConversationId) -> Served {
    records::get_json(self.client.store(), Table::SyncState, &served_key(conversation))
      .ok()
      .flatten()
      .unwrap_or_default()
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

  fn records(&self) -> Vec<SessionRecord> {
    match records::list_json_with_prefix::<SessionRecord>(
      self.client.store(),
      Table::SyncState,
      SESSION_PREFIX,
    ) {
      Ok(records) => records.into_iter().map(|(_, record)| record).collect(),
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

/// Writes the summary (if there is a key) and composes the result.
async fn write_report(
  record: SessionRecord,
  known_names: HashMap<UserId, String>,
  maybe_openrouter: Option<Arc<OpenRouterClient>>,
  minutes: u64,
  reason: StopReason,
  mut usage: Usage,
  settings: SummarySettings,
) -> FinishedReport {
  // The texts are not `Send`: made before and after the requests, not held across them.
  let names = Names::new(
    known_names,
    &record.transcript,
    &Texts::for_language(&record.policy.language),
  );
  let date = crate::report::date_time(record.started_at_ms);
  let participants: Vec<String> = record
    .transcript
    .speakers()
    .into_iter()
    .map(|user| names.name(user).to_owned())
    .collect();
  let lines = record.transcript.lines(&names);
  let mut is_key_refused = false;
  let summary = match (&maybe_openrouter, record.transcript.is_empty()) {
    (_, true) => Err(String::new()),
    (None, false) => Err(KeyError::NotStored.to_string()),
    (Some(client), false) => {
      let input = SummaryInput {
        date: &date,
        participants: &participants,
        lines: &lines,
      };
      match summarize(client, &record.policy, input, settings).await {
        Ok((text, summary_usage)) => {
          usage.add(&summary_usage);
          Ok(text)
        },
        Err(error) => {
          is_key_refused = matches!(error, OpenRouterError::Unauthorized);
          warn!(%error, conversation_id = %record.conversation, "the summary could not be written");
          Err(error.to_string())
        },
      }
    },
  };
  let texts = Texts::for_language(&record.policy.language);
  let report = compose(&ReportInput {
    texts: &texts,
    policy: &record.policy,
    started_at_ms: record.started_at_ms,
    minutes,
    names: &names,
    transcript: &record.transcript,
    reason: &reason,
    summary: summary.as_deref().map_err(String::as_str),
  });
  let (maybe_file_name, maybe_file_markdown) = match report.maybe_file {
    Some(file) => (
      Some(file.file_name),
      Some(String::from_utf8_lossy(&file.content).into_owned()),
    ),
    None => (None, None),
  };
  FinishedReport {
    conversation: record.conversation,
    report: StoredReport {
      body: report.body,
      maybe_file_name,
      maybe_file_markdown,
    },
    usage,
    is_key_refused,
  }
}

fn session_key(conversation: ConversationId) -> String {
  format!("{SESSION_PREFIX}{conversation}")
}

fn served_key(conversation: ConversationId) -> String {
  format!("{SERVED_PREFIX}{conversation}")
}

fn unix_ms() -> i64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_or(0, |since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
}
