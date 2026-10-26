//! The task that owns the encrypted client: it keeps the gateway connection (so the bot receives
//! welcomes, commits and content keys), batches log lines, sends the messages, publishes search
//! index segments and keeps the sources announced in the app current (docs/bridge.md, "Sources").

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use teamcraft_core::api::{GatewayConnection, ServerApi};
use teamcraft_core::store::LocalStore;
use teamcraft_core::{Client, HistoryScan};
use teamcraft_proto::{ConversationId, GatewayClientMessage, UserId};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::batch::{BatchLimits, Batcher, LogLine, Mentions};
use crate::http::Submission;
use crate::sources::{Source, SourceRegistry};

/// How often the worker looks at timers, the gateway and the outbox.
const TICK: Duration = Duration::from_millis(200);
/// Keep-alive for the gateway (the server expects one every 30 seconds).
const HEARTBEAT: Duration = Duration::from_secs(20);
const RECONNECT_BACKOFF_MIN: Duration = Duration::from_secs(1);
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(30);
/// Messages for channels the bot has no group for yet wait here; the oldest are dropped beyond this.
const MAX_WAITING_MESSAGES: usize = 1000;

#[derive(Debug, Clone, Copy)]
pub struct WorkerSettings {
  pub batch: BatchLimits,
  /// `None` turns index publishing off.
  pub index_interval: Option<Duration>,
  /// Whether sources announced in the app are served; `None` turns them off.
  pub maybe_app_sources: Option<AppSourceSettings>,
}

#[derive(Debug, Clone, Copy)]
pub struct AppSourceSettings {
  /// How many pages of older history the bridge reads once per channel, for announcements older
  /// than its database (it was lost or is new).
  pub history_pages: u32,
}

#[derive(Debug)]
struct Waiting {
  conversation: ConversationId,
  text: String,
  mentions: Vec<UserId>,
}

pub struct Worker<A: ServerApi, S: LocalStore> {
  client: Client<A, S>,
  sources: SourceRegistry,
  /// Per channel: the source ledger revision the member roles were last read for.
  seen_revisions: BTreeMap<ConversationId, u64>,
  /// Per channel: older history pages read so far, until the look back is done.
  history_scans: BTreeMap<ConversationId, u32>,
  scanned: HashSet<ConversationId>,
  receiver: mpsc::Receiver<Submission>,
  batcher: Batcher,
  settings: WorkerSettings,
  gateway: Option<A::Gateway>,
  waiting: VecDeque<Waiting>,
  next_connect: Instant,
  backoff: Duration,
  last_heartbeat: Instant,
  last_index: Instant,
  last_flush: Instant,
}

impl<A: ServerApi, S: LocalStore> Worker<A, S> {
  pub fn new(
    client: Client<A, S>,
    sources: SourceRegistry,
    receiver: mpsc::Receiver<Submission>,
    settings: WorkerSettings,
  ) -> Self {
    let now = Instant::now();
    Self {
      client,
      sources,
      seen_revisions: BTreeMap::new(),
      history_scans: BTreeMap::new(),
      scanned: HashSet::new(),
      receiver,
      batcher: Batcher::new(settings.batch),
      settings,
      gateway: None,
      waiting: VecDeque::new(),
      next_connect: now,
      backoff: RECONNECT_BACKOFF_MIN,
      last_heartbeat: now,
      last_index: now,
      last_flush: now,
    }
  }

  /// Runs until every sender of submissions is gone, then sends what is left.
  pub async fn run(mut self) -> Client<A, S> {
    // The store already holds what the bridge learned before a restart.
    self.refresh_sources().await;
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
      tokio::select! {
        submission = self.receiver.recv() => match submission {
          Some(submission) => self.accept(submission).await,
          None => break,
        },
        _ = tick.tick() => self.on_tick().await,
      }
    }
    for batch in self.batcher.take_all() {
      self.send(batch.conversation, batch.text, batch.mentions).await;
    }
    self.retry_waiting().await;
    if let Err(error) = self.client.flush_outbox().await {
      warn!(%error, "flushing the outbox at shutdown failed");
    }
    self.client
  }

  async fn accept(&mut self, submission: Submission) {
    let now = Instant::now();
    match submission {
      Submission::Message {
        source,
        text,
        is_alert,
      } => {
        let (text, mentions) = alert_text(text, is_alert, &source.mentions);
        self.send(source.conversation, text, mentions).await;
      },
      Submission::Lines { source, lines } => {
        for line in lines {
          if let Some(batch) = self
            .batcher
            .push(source.conversation, line, &source.mentions, now)
          {
            self.send(batch.conversation, batch.text, batch.mentions).await;
          }
        }
      },
      Submission::Dropped { source, count } => {
        warn!(source = %source.name, count, "dropped lines over the rate limit");
        let notice = LogLine {
          text: format!("[bridge] dropped {count} lines from {} (rate limit)", source.name),
          is_alert: false,
        };
        if let Some(batch) = self
          .batcher
          .push(source.conversation, notice, &source.mentions, now)
        {
          self.send(batch.conversation, batch.text, batch.mentions).await;
        }
      },
    }
  }

  async fn on_tick(&mut self) {
    let now = Instant::now();
    for batch in self.batcher.take_due(now) {
      self.send(batch.conversation, batch.text, batch.mentions).await;
    }
    self.keep_gateway(now).await;
    if now.saturating_duration_since(self.last_flush) >= Duration::from_secs(1) {
      self.last_flush = now;
      self.refresh_sources().await;
      self.retry_waiting().await;
      if let Err(error) = self.client.flush_outbox().await {
        debug!(%error, "flushing the outbox failed");
      }
    }
    if let Some(interval) = self.settings.index_interval
      && now.saturating_duration_since(self.last_index) >= interval
    {
      self.last_index = now;
      match self.client.publish_index_segments().await {
        Ok(0) => {},
        Ok(count) => info!(count, "published search index segments"),
        Err(error) => warn!(%error, "publishing search index segments failed"),
      }
    }
  }

  /// Brings the sources announced in the app up to date with the bot's store, which the gateway
  /// keeps current: reads the channel roles again when a source change arrived, looks back once
  /// through each channel's older history, and swaps the endpoints' managed set when it changed.
  /// Runs once a second; it reads the local store only, apart from those one-off requests.
  async fn refresh_sources(&mut self) {
    let Some(app_sources) = self.settings.maybe_app_sources else {
      return;
    };
    let conversations = match self.client.group_conversations() {
      Ok(conversations) => conversations,
      Err(error) => {
        warn!(%error, "listing the bridge's channels failed");
        return;
      },
    };
    let mut active = Vec::new();
    for conversation in conversations {
      if self.gateway.is_some() {
        self.scan_history(conversation, app_sources.history_pages).await;
      }
      match self.channel_sources(conversation).await {
        Ok(sources) => active.extend(sources),
        Err(error) => warn!(%error, %conversation, "reading the channel's sources failed"),
      }
    }
    if self.sources.has_managed(&active) {
      return;
    }
    let change = self.sources.replace_managed(active);
    for name in change.added {
      info!(source = %name, "source added in the app");
    }
    for name in change.removed {
      info!(source = %name, "source revoked or no longer allowed");
    }
  }

  async fn channel_sources(
    &mut self,
    conversation: ConversationId,
  ) -> Result<Vec<Source>, teamcraft_core::CoreError> {
    let mut sources = self.client.integration_sources(conversation)?;
    let is_new = self
      .seen_revisions
      .get(&conversation)
      .is_none_or(|seen| *seen != sources.revision);
    if is_new && sources.revision > 0 && self.gateway.is_some() {
      // Somebody announced, renamed or revoked a source: judge it by the current channel roles.
      if let Err(error) = self.client.refresh_source_authority(conversation).await {
        debug!(%error, %conversation, "reading the channel roles failed; using the stored ones");
      } else {
        self.seen_revisions.insert(conversation, sources.revision);
      }
      sources = self.client.integration_sources(conversation)?;
    }
    Ok(sources.active.iter().filter_map(Source::from_managed).collect())
  }

  /// Reads one more page of the channel's older history until the look back is done.
  async fn scan_history(&mut self, conversation: ConversationId, max_pages: u32) {
    if self.scanned.contains(&conversation) {
      return;
    }
    let pages = self.history_scans.get(&conversation).copied().unwrap_or(0);
    match self
      .client
      .scan_history_for_sources(conversation, pages, max_pages)
      .await
    {
      Ok(HistoryScan::Done) => {
        self.scanned.insert(conversation);
        self.history_scans.remove(&conversation);
      },
      Ok(HistoryScan::More) => {
        self.history_scans.insert(conversation, pages + 1);
      },
      Err(error) => debug!(%error, %conversation, "reading older history failed; trying again"),
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

  /// Hands a message to the client; if the bot has no group for the channel yet it waits.
  async fn send(&mut self, conversation: ConversationId, text: String, mentions: Vec<UserId>) {
    if !self.can_send(conversation) {
      self.hold(Waiting {
        conversation,
        text,
        mentions,
      });
      return;
    }
    if let Err(error) = self
      .client
      .send_text_mentioning(conversation, None, &text, mentions)
      .await
    {
      // The message is in the outbox already when sending fails; it goes out with the next flush.
      debug!(%error, %conversation, "sending is delayed");
    }
  }

  fn can_send(&mut self, conversation: ConversationId) -> bool {
    self.client.has_group(conversation).unwrap_or(false)
  }

  fn hold(&mut self, message: Waiting) {
    if self.waiting.len() >= MAX_WAITING_MESSAGES {
      self.waiting.pop_front();
      warn!("dropped the oldest message: the bot is still no member of its channels");
    }
    self.waiting.push_back(message);
  }

  async fn retry_waiting(&mut self) {
    for _ in 0..self.waiting.len() {
      let Some(message) = self.waiting.pop_front() else {
        break;
      };
      if self.can_send(message.conversation) {
        self
          .send(message.conversation, message.text, message.mentions)
          .await;
      } else {
        self.waiting.push_back(message);
      }
    }
  }
}

/// An alert gets the mentioned people's names in front and their IDs as metadata.
fn alert_text(text: String, is_alert: bool, mentions: &Mentions) -> (String, Vec<UserId>) {
  if !is_alert || mentions.users.is_empty() {
    return (text, Vec::new());
  }
  let names: Vec<String> = mentions.names.iter().map(|name| format!("@{name}")).collect();
  (format!("{}\n{text}", names.join(" ")), mentions.users.clone())
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  #[test]
  fn alerts_name_and_mention_people_and_other_messages_do_not() {
    let mentions = Mentions {
      users: vec![UserId::from_snowflake(Snowflake::new(5).unwrap())],
      names: vec!["alice".into(), "bob".into()],
    };
    let (text, users) = alert_text("Disk full".into(), true, &mentions);
    assert_eq!((text.as_str(), users.len()), ("@alice @bob\nDisk full", 1));
    assert_eq!(
      alert_text("Disk full".into(), false, &mentions),
      ("Disk full".to_owned(), Vec::new())
    );
    assert_eq!(
      alert_text("x".into(), true, &Mentions::default()),
      ("x".to_owned(), Vec::new())
    );
  }
}
