//! Keeps a bot's gateway connection: connects with exponential backoff, processes what the server
//! sent, and sends heartbeats. Calls and requests reach the bot through it.

use std::time::{Duration, Instant};

use teamcraft_core::Client;
use teamcraft_core::api::{GatewayConnection, ServerApi};
use teamcraft_core::store::LocalStore;
use teamcraft_proto::GatewayClientMessage;
use tracing::{info, warn};

/// Keep-alive for the gateway (the server expects one every 30 seconds).
const HEARTBEAT: Duration = Duration::from_secs(20);
const RECONNECT_BACKOFF_MIN: Duration = Duration::from_secs(1);
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(30);

/// The gateway connection and its timers.
pub struct GatewayKeeper<G> {
  maybe_connection: Option<G>,
  next_connect: Instant,
  backoff: Duration,
  last_heartbeat: Instant,
}

/// What [`GatewayKeeper::keep`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayStep {
  /// It connected just now: calls may have ended or begun while it was away.
  Connected,
  /// The connection was up and is up.
  Up,
  /// No connection (it is retried later).
  Down,
}

impl<G: GatewayConnection> GatewayKeeper<G> {
  pub fn new(now: Instant) -> Self {
    Self {
      maybe_connection: None,
      next_connect: now,
      backoff: RECONNECT_BACKOFF_MIN,
      last_heartbeat: now,
    }
  }

  pub fn is_connected(&self) -> bool {
    self.maybe_connection.is_some()
  }

  /// Connects when due, processes what the server sent, and sends a heartbeat when due.
  pub async fn keep<A, S>(&mut self, client: &mut Client<A, S>, now: Instant) -> GatewayStep
  where
    A: ServerApi<Gateway = G>,
    S: LocalStore,
  {
    let mut step = GatewayStep::Up;
    if self.maybe_connection.is_none() {
      if now < self.next_connect {
        return GatewayStep::Down;
      }
      match client.connect_gateway().await {
        Ok((connection, outcome)) => {
          info!(?outcome, "gateway connected");
          self.maybe_connection = Some(connection);
          self.backoff = RECONNECT_BACKOFF_MIN;
          self.last_heartbeat = now;
          step = GatewayStep::Connected;
        },
        Err(error) => {
          warn!(%error, retry_in = ?self.backoff, "gateway connection failed");
          self.next_connect = now + self.backoff;
          self.backoff = (self.backoff * 2).min(RECONNECT_BACKOFF_MAX);
          return GatewayStep::Down;
        },
      }
    }
    let Some(connection) = self.maybe_connection.as_mut() else {
      return GatewayStep::Down;
    };
    if let Err(error) = client.pump(connection).await {
      warn!(%error, "gateway connection lost");
      self.maybe_connection = None;
      self.next_connect = now + RECONNECT_BACKOFF_MIN;
      return GatewayStep::Down;
    }
    if now.saturating_duration_since(self.last_heartbeat) >= HEARTBEAT {
      self.last_heartbeat = now;
      if let Err(error) = connection.send(&GatewayClientMessage::Heartbeat).await {
        warn!(%error, "heartbeat failed");
        self.maybe_connection = None;
        return GatewayStep::Down;
      }
    }
    step
  }
}
