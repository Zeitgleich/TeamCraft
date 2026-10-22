//! The task that owns the encrypted client: it keeps the gateway connection (so the bot receives
//! welcomes, commits and content keys), batches log lines, sends the messages and publishes search
//! index segments.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use teamcraft_core::Client;
use teamcraft_core::api::{GatewayConnection, ServerApi};
use teamcraft_core::store::LocalStore;
use teamcraft_proto::{ConversationId, GatewayClientMessage, UserId};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::batch::{BatchLimits, Batcher, LogLine, Mentions};
use crate::http::Submission;

/// How often the worker looks at timers, the gateway and the outbox.
const TICK: Duration = Duration::from_millis(200);
/// Keep-alive for the gateway (the server expects one every 30 seconds).
const HEARTBEAT: Duration = Duration::from_secs(20);
const RECONNECT_BACKOFF_MIN: Duration = Duration::from_secs(1);
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(30);
/// Messages for channels the bot has no group for yet wait here; the oldest are dropped beyond this.
const MAX_WAITING_MESSAGES: usize = 1000;

/// What the worker needs to know about a source.
#[derive(Debug, Clone)]
pub struct WorkerSource {
  pub name: String,
  pub conversation: ConversationId,
  pub mentions: Mentions,
}

#[derive(Debug, Clone, Copy)]
pub struct WorkerSettings {
  pub batch: BatchLimits,
  /// `None` turns index publishing off.
  pub index_interval: Option<Duration>,
}

#[derive(Debug)]
struct Waiting {
  conversation: ConversationId,
  text: String,
  mentions: Vec<UserId>,
}

pub struct Worker<A: ServerApi, S: LocalStore> {
  client: Client<A, S>,
  sources: Vec<WorkerSource>,
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
    sources: Vec<WorkerSource>,
    receiver: mpsc::Receiver<Submission>,
    settings: WorkerSettings,
  ) -> Self {
    let now = Instant::now();
    Self {
      client,
      sources,
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
        let Some(source) = self.sources.get(source).cloned() else {
          return;
        };
        let (text, mentions) = alert_text(text, is_alert, &source.mentions);
        self.send(source.conversation, text, mentions).await;
      },
      Submission::Lines { source, lines } => {
        let Some(source) = self.sources.get(source).cloned() else {
          return;
        };
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
        let Some(source) = self.sources.get(source).cloned() else {
          return;
        };
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
