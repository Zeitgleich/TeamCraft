//! Keeps the gateway connected: one supervisor for every shell (the mobile runner, the web core and
//! the desktop runtime), so connecting, heartbeats, dead-connection detection, backoff and the
//! connection state behave the same everywhere (robustness.md, "Network loss and change").
//!
//! The supervisor owns the connection but not the loop: each shell keeps its own event loop and calls
//! [`ConnectionSupervisor::connect_if_due`], [`ConnectionSupervisor::receive`],
//! [`ConnectionSupervisor::on_frame`] and [`ConnectionSupervisor::on_tick`] from it, and wakes up by
//! [`ConnectionSupervisor::next_deadline_ms`]. Time is the client's clock, so tests drive it.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_channel::mpsc;
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use teamcraft_proto::{GatewayClientMessage, GatewayServerMessage};

use crate::api::{ApiError, GatewayConnection, Interception, ServerApi};
use crate::client::{Client, GatewayOutcome, GatewayTiming, HeldFrames, HeldLimits, keep_alive};
use crate::error::CoreError;
use crate::retry::{Jitter, RetryPolicy, core_retry_after};
use crate::store::LocalStore;
use teamcraft_proto::SignInReason;

/// Where the gateway connection stands. The shells show it ("connecting", "offline, retrying at",
/// "sign in again") instead of guessing from the operating system's connectivity: a device can be
/// online while the server is down, a proxy blocks WebSockets or the token was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConnectionState {
  /// Opening the connection, running the handshake and, for a fresh session, the full sync.
  Connecting,
  Connected,
  /// The last attempt failed or the connection was lost; the next attempt starts at `retry_at_ms`
  /// (Unix milliseconds), or at once when the network changes. `maybe_interception` names what
  /// answered instead of the server, if the last attempt found out (a captive portal, a TLS proxy).
  Offline {
    retry_at_ms: i64,
    #[serde(rename = "interception", default, skip_serializing_if = "Option::is_none")]
    maybe_interception: Option<Interception>,
  },
  /// The server refused the access token, for `reason` if it said. Tried again when the app has
  /// another token, or when the network changes.
  SignInRequired {
    #[serde(default, skip_serializing_if = "SignInReason::is_unknown")]
    reason: SignInReason,
  },
}

/// A tick this much earlier than the previous one means the wall clock was set back.
const CLOCK_JUMP_BACK_MS: i64 = 1_000;

/// Timing of the supervisor. The defaults suit every shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionSettings {
  /// Longest heartbeat interval, also when the server's `hello` allows more: proxies drop
  /// WebSockets that stay idle for 30 to 60 s.
  pub max_heartbeat: Duration,
  /// How long each handshake step may take (socket and `hello`, the answer to `identify`/`resume`).
  pub handshake_timeout: Duration,
  pub reconnect_min: Duration,
  pub reconnect_max: Duration,
  /// A connection that stayed up this long counts as stable; only then does the backoff start
  /// over. A server that accepts and then drops every connection keeps backing off.
  pub stable_after: Duration,
  /// After a network change or a wake-up, an open connection must answer a heartbeat within this
  /// long or it is replaced.
  pub probe_timeout: Duration,
  /// A gap this long between two ticks means the device slept or the tab was frozen.
  pub wake_gap: Duration,
  /// What is kept while a long sync or command runs ([`ConnectionSupervisor::keep_alive`]); beyond
  /// it the connection isn't read until the work is done.
  pub held_limits: HeldLimits,
}

/// The current [`ConnectionState`], readable from other threads and while the shell's loop is busy
/// (connecting, syncing).
#[derive(Clone, Debug)]
pub struct ConnectionStatus(Arc<Mutex<ConnectionState>>);

/// Network changes and wake-ups the platform reported (Android `NetworkCallback`, iOS
/// `NWPathMonitor`, browser `online`/`visibilitychange`, OS resume). Every subscriber hears each
/// one: the gateway reconnects at once, and calls can use the same signal.
#[derive(Clone, Debug, Default)]
pub struct NetworkChanges {
  subscribers: Arc<Mutex<Vec<mpsc::UnboundedSender<()>>>>,
}

/// Keeps one gateway connection alive; see the module docs.
pub struct ConnectionSupervisor<G> {
  settings: ConnectionSettings,
  maybe_gateway: Option<G>,
  status: ConnectionStatus,
  maybe_observer: Option<Box<dyn Fn(ConnectionState)>>,
  /// Failed attempts since the last stable connection; the backoff grows with them.
  failures: u32,
  next_attempt_ms: i64,
  heartbeat: Duration,
  connected_at_ms: i64,
  last_heard_ms: i64,
  last_heartbeat_ms: i64,
  /// Set after a network change: the open connection must answer by then.
  maybe_probe_deadline_ms: Option<i64>,
  maybe_last_tick_ms: Option<i64>,
  /// A fingerprint of the token the server refused, so it isn't offered again and again (each
  /// refusal counts against the address on the server).
  maybe_refused_token: Option<u64>,
  /// What arrived while long work kept the connection alive; [`Self::receive`] hands it out first.
  held: HeldFrames,
  jitter: Jitter,
}

impl Default for ConnectionSettings {
  fn default() -> Self {
    Self {
      max_heartbeat: Duration::from_secs(25),
      handshake_timeout: crate::client::DEFAULT_HANDSHAKE_TIMEOUT,
      reconnect_min: Duration::from_secs(1),
      reconnect_max: Duration::from_secs(30),
      stable_after: Duration::from_secs(30),
      probe_timeout: Duration::from_secs(3),
      wake_gap: Duration::from_secs(30),
      held_limits: HeldLimits::default(),
    }
  }
}

impl ConnectionStatus {
  pub fn new() -> Self {
    Self(Arc::new(Mutex::new(ConnectionState::Connecting)))
  }

  pub fn get(&self) -> ConnectionState {
    self.0.lock().map_or(ConnectionState::Connecting, |state| *state)
  }

  fn set(&self, state: ConnectionState) {
    if let Ok(mut slot) = self.0.lock() {
      *slot = state;
    }
  }
}

impl Default for ConnectionStatus {
  fn default() -> Self {
    Self::new()
  }
}

impl NetworkChanges {
  pub fn new() -> Self {
    Self::default()
  }

  /// Reports a change to every subscriber.
  pub fn notify(&self) {
    if let Ok(mut subscribers) = self.subscribers.lock() {
      subscribers.retain(|subscriber| subscriber.unbounded_send(()).is_ok());
    }
  }

  /// One item per change from now on; dropping the receiver unsubscribes.
  pub fn subscribe(&self) -> mpsc::UnboundedReceiver<()> {
    let (sender, receiver) = mpsc::unbounded();
    if let Ok(mut subscribers) = self.subscribers.lock() {
      subscribers.push(sender);
    }
    receiver
  }
}

impl<G: GatewayConnection> ConnectionSupervisor<G> {
  /// `status` is shared with whoever shows the state; the first attempt is due at once.
  pub fn new(settings: ConnectionSettings, status: ConnectionStatus) -> Self {
    let heartbeat = settings.max_heartbeat;
    status.set(ConnectionState::Connecting);
    Self {
      settings,
      maybe_gateway: None,
      status,
      maybe_observer: None,
      failures: 0,
      next_attempt_ms: i64::MIN,
      heartbeat,
      connected_at_ms: i64::MIN,
      last_heard_ms: i64::MIN,
      last_heartbeat_ms: i64::MIN,
      maybe_probe_deadline_ms: None,
      maybe_last_tick_ms: None,
      maybe_refused_token: None,
      held: HeldFrames::default(),
      jitter: Jitter::new(OsRng.next_u64()),
    }
  }

  /// A fixed seed for the backoff jitter (tests).
  pub fn with_jitter_seed(mut self, seed: u64) -> Self {
    self.jitter = Jitter::new(seed);
    self
  }

  /// Reports to `status` from now on (an app that keeps one status across restarts of its loop).
  pub fn share_status(&mut self, status: ConnectionStatus) {
    status.set(self.status.get());
    self.status = status;
  }

  /// Runs whenever the state changes (the shell tells its UI to read it again).
  pub fn set_observer(&mut self, observer: Box<dyn Fn(ConnectionState)>) {
    self.maybe_observer = Some(observer);
  }

  pub fn state(&self) -> ConnectionState {
    self.status.get()
  }

  pub fn status(&self) -> ConnectionStatus {
    self.status.clone()
  }

  pub fn is_connected(&self) -> bool {
    self.maybe_gateway.is_some()
  }

  /// When the shell's loop should wake up for the supervisor: the next attempt while disconnected;
  /// the next heartbeat, probe deadline or silence limit while connected. `None`: only an event
  /// (a network change, a new token) can move things on.
  pub fn next_deadline_ms(&self) -> Option<i64> {
    if self.maybe_gateway.is_none() {
      return match self.state() {
        ConnectionState::SignInRequired { .. } => None,
        _ => Some(self.next_attempt_ms),
      };
    }
    let heartbeat_due = self.last_heartbeat_ms.saturating_add(millis(self.heartbeat));
    let silence_due = self.last_heard_ms.saturating_add(self.silence_limit_ms());
    let mut deadline = heartbeat_due.min(silence_due);
    if let Some(probe) = self.maybe_probe_deadline_ms {
      deadline = deadline.min(probe);
    }
    Some(deadline)
  }

  /// Connects when an attempt is due; returns how the connection started.
  pub async fn connect_if_due<A, S>(&mut self, client: &mut Client<A, S>) -> Option<GatewayOutcome>
  where
    A: ServerApi<Gateway = G>,
    S: LocalStore,
  {
    if self.maybe_gateway.is_some() || !self.is_attempt_due(client) {
      return None;
    }
    self.set_state(ConnectionState::Connecting);
    let timing = GatewayTiming {
      handshake_timeout: self.settings.handshake_timeout,
      max_heartbeat: self.settings.max_heartbeat,
      held_limits: self.settings.held_limits,
    };
    let result = client.start_gateway(timing).await;
    let now = client.now_ms();
    match result {
      Ok(start) => {
        tracing::info!(outcome = ?start.outcome, "gateway connected");
        self.heartbeat = start.heartbeat_interval.min(self.settings.max_heartbeat);
        self.maybe_gateway = Some(start.connection);
        self.held = start.held;
        // The heartbeats went on during the sync: the next one is due an interval from now at the
        // latest, and the loop was busy, not asleep.
        self.held.take_kept_alive();
        self.connected_at_ms = now;
        self.last_heard_ms = now;
        self.last_heartbeat_ms = now;
        self.maybe_last_tick_ms = Some(now);
        self.maybe_probe_deadline_ms = None;
        self.maybe_refused_token = None;
        self.set_state(ConnectionState::Connected);
        Some(start.outcome)
      },
      Err(error) if error.is_sign_in_required() => {
        tracing::warn!(%error, "the gateway refused the access token");
        self.maybe_refused_token = token_fingerprint(client);
        let reason = match &error {
          CoreError::Api(api) => api
            .error_code()
            .map(SignInReason::from_error_code)
            .unwrap_or_default(),
          _ => SignInReason::Unknown,
        };
        self.set_state(ConnectionState::SignInRequired { reason });
        None
      },
      Err(error) => {
        tracing::warn!(%error, "gateway connection failed");
        let maybe_interception = match &error {
          CoreError::Api(api) => api.interception(),
          _ => None,
        };
        // A server that answered 503 says when to come back (a restart, a dependency outage).
        self.schedule_attempt(now, maybe_interception, core_retry_after(&error));
        None
      },
    }
  }

  /// The next frame of the open connection, frames held during long work first; never completes
  /// while there is none.
  pub async fn receive(&mut self) -> Option<Result<GatewayServerMessage, ApiError>> {
    if let Some(held) = self.held.pop() {
      return held;
    }
    match self.maybe_gateway.as_mut() {
      Some(connection) => connection.receive().await,
      None => std::future::pending().await,
    }
  }

  /// Applies what [`Self::receive`] returned; `true` if it may have changed what the UI shows.
  /// `false` for a heartbeat answer (it only proves the connection is alive; reporting it as a
  /// change made the apps re-read everything every 25 s while idle) and when the connection is
  /// gone (it is then replaced after the backoff).
  pub async fn on_frame<A, S>(
    &mut self,
    client: &mut Client<A, S>,
    maybe_frame: Option<Result<GatewayServerMessage, ApiError>>,
  ) -> bool
  where
    A: ServerApi<Gateway = G>,
    S: LocalStore,
  {
    let now = client.now_ms();
    let outcome = match maybe_frame {
      Some(Ok(frame)) => {
        self.last_heard_ms = now;
        self.maybe_probe_deadline_ms = None;
        let is_heartbeat_ack = matches!(frame, GatewayServerMessage::HeartbeatAck);
        // An event can start a long catch-up (a welcome, a membership change).
        let handled = self.keep_alive(now, client.handle_frame(frame)).await;
        self.note_kept_alive(client.now_ms());
        handled.map(|()| !is_heartbeat_ack)
      },
      Some(Err(error)) => Err(CoreError::Api(error)),
      None => Err(CoreError::Gateway("connection closed".into())),
    };
    match outcome {
      Ok(is_changed) => is_changed,
      Err(error) => {
        self.lose(client.now_ms(), &error);
        false
      },
    }
  }

  /// Heartbeats, dead-connection detection and wake-up detection. Call it at least every few
  /// seconds and at [`Self::next_deadline_ms`].
  pub async fn on_tick(&mut self, now_ms: i64) {
    self.note_kept_alive(now_ms);
    if let Some(last) = self.maybe_last_tick_ms
      && last.saturating_sub(now_ms) >= CLOCK_JUMP_BACK_MS
    {
      tracing::info!(
        back_ms = last - now_ms,
        "the clock went back; keeping the gateway's pace"
      );
      self.shift_timers(now_ms.saturating_sub(last));
    }
    let has_slept = self
      .maybe_last_tick_ms
      .is_some_and(|last| now_ms.saturating_sub(last) >= millis(self.settings.wake_gap));
    self.maybe_last_tick_ms = Some(now_ms);
    if has_slept {
      tracing::info!("woke up after a pause, checking the gateway");
      self.network_changed(now_ms).await;
    }
    if self.maybe_gateway.is_none() {
      return;
    }
    if self.failures > 0 && now_ms.saturating_sub(self.connected_at_ms) >= millis(self.settings.stable_after)
    {
      self.failures = 0;
    }
    if self
      .maybe_probe_deadline_ms
      .is_some_and(|deadline| now_ms >= deadline)
    {
      self.lose(now_ms, &"no answer after a network change");
      // The network changed or the device woke up: the new connection is tried at once, not after
      // the backoff (the user is likely looking at the app right now).
      self.next_attempt_ms = now_ms;
      self.set_state(ConnectionState::Offline {
        retry_at_ms: now_ms,
        maybe_interception: None,
      });
      return;
    }
    let silent_ms = now_ms.saturating_sub(self.last_heard_ms);
    if silent_ms >= self.silence_limit_ms() {
      self.lose(
        now_ms,
        &format!("no frame from the gateway for {} s", silent_ms / 1000),
      );
      return;
    }
    if now_ms.saturating_sub(self.last_heartbeat_ms) >= millis(self.heartbeat) {
      self.heartbeat_now(now_ms).await;
    }
  }

  /// Runs `work` (a UI command, the upkeep, the outbox) while the connection keeps its heartbeats
  /// and reads what arrives (issue #325): a long command can't get the connection dropped. `now_ms`
  /// is the time the work starts; what arrived is handed out by [`Self::receive`] afterwards, and
  /// the next [`Self::on_tick`] or [`Self::on_frame`] counts the heartbeats sent meanwhile.
  pub async fn keep_alive<T>(&mut self, now_ms: i64, work: impl std::future::Future<Output = T>) -> T {
    let Some(connection) = self.maybe_gateway.as_mut() else {
      return work.await;
    };
    let heartbeat_due = self.last_heartbeat_ms.saturating_add(millis(self.heartbeat));
    let first_in = Duration::from_millis(heartbeat_due.saturating_sub(now_ms).max(0).unsigned_abs());
    keep_alive(
      connection,
      first_in,
      self.heartbeat,
      self.settings.held_limits,
      &mut self.held,
      work,
    )
    .await
  }

  /// Sends a frame on the open connection; `false` if there is none or the send failed (the
  /// connection is then replaced).
  pub async fn send(&mut self, now_ms: i64, message: &GatewayClientMessage) -> bool {
    let Some(connection) = self.maybe_gateway.as_mut() else {
      return false;
    };
    match connection.send(message).await {
      Ok(()) => true,
      Err(error) => {
        self.lose(now_ms, &error);
        false
      },
    }
  }

  /// The network changed or the device woke up: an open connection has to prove it still works
  /// within [`ConnectionSettings::probe_timeout`], a missing one is tried again at once (also after
  /// a refused token: the app may have signed in again meanwhile).
  pub async fn network_changed(&mut self, now_ms: i64) {
    self.failures = 0;
    if self.maybe_gateway.is_some() {
      self.maybe_probe_deadline_ms = Some(now_ms.saturating_add(millis(self.settings.probe_timeout)));
      self.heartbeat_now(now_ms).await;
      return;
    }
    self.maybe_refused_token = None;
    self.next_attempt_ms = now_ms;
    if matches!(self.state(), ConnectionState::SignInRequired { .. }) {
      self.set_state(ConnectionState::Offline {
        retry_at_ms: now_ms,
        maybe_interception: None,
      });
    }
  }

  /// Counts what [`Self::keep_alive`] did: heartbeats sent and answers heard move the timers, and a
  /// long work isn't mistaken for a device that slept.
  fn note_kept_alive(&mut self, now_ms: i64) {
    let kept = self.held.take_kept_alive();
    if kept.heartbeats_sent > 0 {
      self.last_heartbeat_ms = now_ms;
      self.maybe_last_tick_ms = Some(now_ms);
    }
    if kept.was_heard {
      self.last_heard_ms = now_ms;
      self.maybe_probe_deadline_ms = None;
    }
  }

  fn is_attempt_due<A: ServerApi, S: LocalStore>(&self, client: &Client<A, S>) -> bool {
    match self.state() {
      // Only another token can change the answer.
      ConnectionState::SignInRequired { .. } => token_fingerprint(client) != self.maybe_refused_token,
      _ => client.now_ms() >= self.next_attempt_ms,
    }
  }

  async fn heartbeat_now(&mut self, now_ms: i64) {
    self.last_heartbeat_ms = now_ms;
    if !self.send(now_ms, &GatewayClientMessage::Heartbeat).await {
      tracing::debug!("heartbeat not sent");
    }
  }

  /// The connection is gone; the next attempt follows after the backoff.
  fn lose(&mut self, now_ms: i64, reason: &dyn std::fmt::Display) {
    tracing::warn!(%reason, "gateway connection lost");
    self.maybe_gateway = None;
    // Frames of the old connection: a resume replays them, a fresh session syncs.
    self.held = HeldFrames::default();
    self.maybe_probe_deadline_ms = None;
    if now_ms.saturating_sub(self.connected_at_ms) >= millis(self.settings.stable_after) {
      self.failures = 0;
    }
    self.schedule_attempt(now_ms, None, None);
  }

  /// The shared retry policy (`crate::retry`): equal jitter, so clients that lost the server at
  /// the same moment (a restart) don't come back in lock-step, and at least the server's
  /// `Retry-After`.
  fn schedule_attempt(
    &mut self,
    now_ms: i64,
    maybe_interception: Option<Interception>,
    maybe_retry_after: Option<Duration>,
  ) {
    let policy = RetryPolicy::new(self.settings.reconnect_min, self.settings.reconnect_max);
    let delay = policy.delay(self.failures, &mut self.jitter, maybe_retry_after);
    self.failures = self.failures.saturating_add(1);
    self.next_attempt_ms = now_ms.saturating_add(millis(delay));
    self.set_state(ConnectionState::Offline {
      retry_at_ms: self.next_attempt_ms,
      maybe_interception,
    });
  }

  /// Moves every timer by `delta_ms` (negative after the wall clock went back), so a clock that
  /// jumps back an hour doesn't postpone heartbeats, dead-connection checks and reconnects by an
  /// hour (robustness.md, "Time").
  fn shift_timers(&mut self, delta_ms: i64) {
    let shift = |at: &mut i64| {
      if *at != i64::MIN {
        *at = at.saturating_add(delta_ms);
      }
    };
    shift(&mut self.next_attempt_ms);
    shift(&mut self.connected_at_ms);
    shift(&mut self.last_heard_ms);
    shift(&mut self.last_heartbeat_ms);
    if let Some(deadline) = self.maybe_probe_deadline_ms.as_mut() {
      shift(deadline);
    }
    if let ConnectionState::Offline {
      maybe_interception, ..
    } = self.state()
    {
      self.set_state(ConnectionState::Offline {
        retry_at_ms: self.next_attempt_ms,
        maybe_interception,
      });
    }
  }

  /// Every heartbeat is answered, so two missed answers and a slow round trip mean the connection
  /// is dead, even if the socket never reports an error (half-open TCP after a network change).
  fn silence_limit_ms(&self) -> i64 {
    millis(self.heartbeat)
      .saturating_mul(2)
      .saturating_add(millis(self.settings.probe_timeout))
  }

  fn set_state(&mut self, state: ConnectionState) {
    if self.status.get() == state {
      return;
    }
    self.status.set(state);
    if let Some(observer) = &self.maybe_observer {
      observer(state);
    }
  }
}

#[cfg(feature = "ui")]
impl From<ConnectionState> for teamcraft_client_api::remote::CoreConnection {
  fn from(state: ConnectionState) -> Self {
    match state {
      ConnectionState::Connecting => Self::Connecting,
      ConnectionState::Connected => Self::Connected,
      ConnectionState::Offline {
        retry_at_ms,
        maybe_interception,
      } => Self::Offline {
        retry_at_ms,
        maybe_interception: maybe_interception.map(|kind| match kind {
          Interception::CaptivePortal => teamcraft_client_api::remote::NetworkInterception::CaptivePortal,
          Interception::UntrustedCertificate => {
            teamcraft_client_api::remote::NetworkInterception::UntrustedCertificate
          },
        }),
      },
      ConnectionState::SignInRequired { reason } => Self::SignInRequired { reason },
    }
  }
}

/// Tells tokens apart without keeping one around. Not a security measure: it only notices that the
/// app has another token than the refused one.
fn token_fingerprint<A: ServerApi, S: LocalStore>(client: &Client<A, S>) -> Option<u64> {
  let token = client.gateway_token().ok()?;
  let mut hasher = DefaultHasher::new();
  token.hash(&mut hasher);
  Some(hasher.finish())
}

fn millis(duration: Duration) -> i64 {
  i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn network_changes_reach_every_subscriber() {
    let changes = NetworkChanges::new();
    let mut first = changes.subscribe();
    let mut second = changes.subscribe();
    changes.notify();
    assert_eq!(first.try_recv().ok(), Some(()));
    assert_eq!(second.try_recv().ok(), Some(()));
    drop(first);
    changes.notify();
    assert_eq!(second.try_recv().ok(), Some(()));
  }

  #[test]
  fn the_state_serializes_with_a_tag() {
    let offline = serde_json::to_value(ConnectionState::Offline {
      retry_at_ms: 5,
      maybe_interception: None,
    })
    .ok();
    assert_eq!(
      offline,
      Some(serde_json::json!({ "state": "offline", "retry_at_ms": 5 }))
    );
    let portal = serde_json::to_value(ConnectionState::Offline {
      retry_at_ms: 5,
      maybe_interception: Some(Interception::CaptivePortal),
    })
    .ok();
    assert_eq!(
      portal,
      Some(serde_json::json!({ "state": "offline", "retry_at_ms": 5, "interception": "captive_portal" }))
    );
    let sign_in = serde_json::to_value(ConnectionState::SignInRequired {
      reason: SignInReason::Unknown,
    })
    .ok();
    assert_eq!(sign_in, Some(serde_json::json!({ "state": "sign_in_required" })));
    let ended = serde_json::to_value(ConnectionState::SignInRequired {
      reason: SignInReason::SessionEnded,
    })
    .ok();
    assert_eq!(
      ended,
      Some(serde_json::json!({ "state": "sign_in_required", "reason": "session_ended" }))
    );
  }
}
