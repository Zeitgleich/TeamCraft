//! WebTransport endpoint and per-session handling.
//!
//! Session flow: the client connects to `/call`, opens one bidirectional stream for signaling and
//! sends [`CallClientMessage::Join`] with its token. Afterwards media flows as QUIC datagrams.
//!
//! Mobile networks: QUIC connection migration is allowed, so a client whose address changes
//! (Wi-Fi to LTE, NAT rebinding) continues on the same connection after path validation. A
//! connection that is lost anyway (idle timeout) keeps its participant's slot for the reconnect
//! grace period; a deliberate close (the client leaving, a host control) ends it at once.
//!
//! Restarts and rolling deploys: on shutdown the SFU closes every connection with
//! [`SFU_GOING_AWAY_CLOSE_CODE`] ([`Sfu::run_until`]). Clients rejoin with a fresh join token; the
//! restarted SFU (or another instance at the same address) creates the room again and DAVE founds a
//! new group. Leaves are not reported to the server while going away, so the call stays up there.
//!
//! Besides joins and leaves the SFU reports its whole roster: at start, every
//! [`SfuConfig::roster_interval`] and right after a call moved between a user's devices. The server
//! drops what the SFU no longer has, so a lost leave (or one never sent while going away, by a
//! client that didn't come back) can't keep a device in a call for good.

use std::{
  future::Future,
  net::SocketAddr,
  sync::Arc,
  time::{Duration, Instant},
};

use teamcraft_proto::{
  CallClientMessage, CallEvent, CallServerMessage, ErrorBody, ErrorCode, SFU_GOING_AWAY_CLOSE_CODE,
  SfuControlRequest, SfuRosterReport,
};
use thiserror::Error;
use tokio::sync::{Notify, mpsc};
use tokio::time::MissedTickBehavior;
use tracing::{Instrument, debug, info, info_span, warn};
use wtransport::{
  Connection, Endpoint, Identity, RecvStream, SendStream, ServerConfig,
  config::Ipv6DualStackConfig,
  endpoint::{IncomingSession, endpoint_side::Server},
  tls::Sha256Digest,
};

use crate::{
  auth::{JoinGrant, JoinTokenVerifier},
  config::SfuConfig,
  prejoin::PreJoinLimiter,
  reporting::{NoReporter, ParticipantReporter},
  room::{JoinOutcome, ParticipantHandle, Room, RoomError, Rooms, SessionId},
  signaling::{EventSequence, Outgoing, SignalingError, read_message, write_message},
};

/// How often senders get feedback on their datagrams (uplink bandwidth estimation) and layer
/// allocations are re-run.
const PERIODIC_INTERVAL: Duration = Duration::from_millis(100);

/// HTTP path of the WebTransport session.
const PRE_JOIN_LIMIT: u32 = 0x4803;
/// Rosters never go out more often than this, whatever the configuration says.
const MIN_ROSTER_INTERVAL: Duration = Duration::from_millis(100);

pub const CALL_PATH: &str = "/call";

/// A bound SFU, ready to [`Sfu::run`].
pub struct Sfu {
  endpoint: Endpoint<Server>,
  certificate_hash: Sha256Digest,
  shared: Arc<Shared>,
}

struct Shared {
  config: SfuConfig,
  rooms: Rooms,
  verifier: Arc<dyn JoinTokenVerifier>,
  reporter: Arc<dyn ParticipantReporter>,
  prejoin: Arc<PreJoinLimiter>,
  /// When this SFU started: the roster tells the server how fresh its rooms are.
  started_at: Instant,
  /// Wakes the roster reporter before its interval (a call moved between devices).
  roster_now: Arc<Notify>,
}

/// Carries out meeting host controls on this SFU's calls (see [`crate::control`]).
#[derive(Clone)]
pub struct SfuControl {
  shared: Arc<Shared>,
}

/// One joined participant's session.
struct ParticipantSession {
  connection: Connection,
  room: Arc<Room>,
  grant: JoinGrant,
  signal_tx: mpsc::Sender<Outgoing>,
  max_bytes: usize,
  roster_now: Arc<Notify>,
}

/// How a participant's session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionEnd {
  /// The client left, or the SFU closed the connection (host control, protocol violation).
  Closed,
  /// The connection broke (idle timeout, reset): the client is expected back.
  Lost,
}

#[derive(Debug, Error)]
pub enum SfuError {
  #[error("binding the endpoint failed: {0}")]
  Bind(#[source] std::io::Error),
  #[error("the TLS identity has no certificate")]
  NoCertificate,
  #[error("the idle timeout is out of range")]
  IdleTimeout,
}

#[derive(Debug, Error)]
enum SessionError {
  #[error("connection failed: {0}")]
  Connection(#[from] wtransport::error::ConnectionError),
  #[error("signaling failed: {0}")]
  Signaling(#[from] SignalingError),
  #[error("client did not join in time")]
  JoinTimeout,
  #[error("first message was not a join")]
  ExpectedJoin,
  #[error(transparent)]
  Room(#[from] RoomError),
}

impl Sfu {
  pub fn bind(
    config: SfuConfig,
    identity: Identity,
    verifier: Arc<dyn JoinTokenVerifier>,
  ) -> Result<Self, SfuError> {
    let certificate_hash = identity
      .certificate_chain()
      .as_slice()
      .first()
      .ok_or(SfuError::NoCertificate)?
      .hash();
    let builder = ServerConfig::builder();
    let builder = match config.bind_addr {
      SocketAddr::V6(addr) => builder.with_bind_address_v6(addr, Ipv6DualStackConfig::Allow),
      SocketAddr::V4(_) => builder.with_bind_address(config.bind_addr),
    };
    let server_config = builder
      .with_identity(identity)
      .keep_alive_interval(Some(config.keep_alive_interval))
      .max_idle_timeout(Some(config.idle_timeout))
      .map_err(|_| SfuError::IdleTimeout)?
      // Clients moving between networks keep their connection (validated by QUIC path checks).
      .allow_migration(true)
      .build();
    let endpoint = Endpoint::server(server_config).map_err(SfuError::Bind)?;
    let rooms = Rooms::new(config.clone());
    Ok(Self {
      endpoint,
      certificate_hash,
      shared: Arc::new(Shared {
        config,
        rooms,
        verifier,
        reporter: Arc::new(NoReporter),
        prejoin: Arc::new(PreJoinLimiter::default()),
        started_at: Instant::now(),
        roster_now: Arc::new(Notify::new()),
      }),
    })
  }

  /// Reports joins and leaves to `teamcraft-server` (call start, ringing, roster). Call before
  /// [`Self::run`].
  pub fn with_reporter(mut self, reporter: Arc<dyn ParticipantReporter>) -> Self {
    if let Some(shared) = Arc::get_mut(&mut self.shared) {
      shared.reporter = reporter;
    }
    self
  }

  pub fn local_addr(&self) -> Result<SocketAddr, SfuError> {
    self.endpoint.local_addr().map_err(SfuError::Bind)
  }

  /// A handle for host controls; take it after [`Self::with_reporter`] and before [`Self::run`].
  pub fn control(&self) -> SfuControl {
    SfuControl {
      shared: Arc::clone(&self.shared),
    }
  }

  /// SHA-256 of the leaf certificate, for `serverCertificateHashes` in browsers.
  pub fn certificate_hash(&self) -> Sha256Digest {
    self.certificate_hash.clone()
  }

  /// Accepts sessions until the task is dropped.
  pub async fn run(self) {
    self.run_until(std::future::pending()).await;
  }

  /// Accepts sessions until `shutdown` resolves (SIGTERM), then goes away: every connection is
  /// closed with [`SFU_GOING_AWAY_CLOSE_CODE`], so its client rejoins instead of ending the call,
  /// and the closes get up to [`SfuConfig::shutdown_drain`] to reach the clients.
  pub async fn run_until(self, shutdown: impl Future<Output = ()>) {
    let rosters = tokio::spawn(report_rosters(Arc::clone(&self.shared)));
    tokio::select! {
      () = self.accept_sessions() => {},
      () = shutdown => {},
    }
    // Going away empties the rooms; that is no roster to tell the server.
    rosters.abort();
    self.go_away().await;
  }

  async fn accept_sessions(&self) {
    let mut session_number: u64 = 0;
    loop {
      session_number = session_number.wrapping_add(1);
      let incoming = self.endpoint.accept().await;
      let shared = Arc::clone(&self.shared);
      tokio::spawn(
        async move {
          if let Err(error) = handle_session(incoming, &shared).await {
            debug!(%error, "session ended");
          }
        }
        .instrument(info_span!("session", session_number)),
      );
    }
  }

  async fn go_away(&self) {
    // Rooms first: from here on, the sessions' ends are not reported as leaves.
    self.shared.rooms.go_away_gracefully().await;
    // Also the sessions that haven't joined yet.
    self
      .endpoint
      .close(SFU_GOING_AWAY_CLOSE_CODE.into(), b"going away");
    if tokio::time::timeout(self.shared.config.shutdown_drain, self.endpoint.wait_idle())
      .await
      .is_err()
    {
      warn!("not every client confirmed the close in time");
    }
    info!("went away");
  }
}

/// Reports the roster at start, every [`SfuConfig::roster_interval`] and when woken early, until
/// the SFU goes away. A tick missed under load is not caught up: one current roster is enough.
async fn report_rosters(shared: Arc<Shared>) {
  let mut interval = tokio::time::interval(shared.config.roster_interval.max(MIN_ROSTER_INTERVAL));
  interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
  loop {
    tokio::select! {
      _ = interval.tick() => {},
      () = shared.roster_now.notified() => {},
    }
    if shared.rooms.is_going_away() {
      return;
    }
    shared.reporter.roster(shared.roster());
  }
}

impl Shared {
  fn roster(&self) -> SfuRosterReport {
    SfuRosterReport {
      uptime_secs: self.started_at.elapsed().as_secs(),
      calls: self.rooms.roster(),
    }
  }
}

async fn handle_session(incoming: IncomingSession, shared: &Shared) -> Result<(), SessionError> {
  let request = incoming.await?;
  if request.path() != CALL_PATH {
    request.not_found().await;
    return Ok(());
  }
  let connection = request.accept().await?;
  if shared.rooms.is_going_away() {
    connection.close(SFU_GOING_AWAY_CLOSE_CODE.into(), b"going away");
    return Ok(());
  }
  let remote_ip = connection.remote_address().ip().to_canonical();
  let Some(prejoin_slot) = shared
    .prejoin
    .try_acquire(remote_ip, shared.config.max_prejoin_sessions_per_ip)
  else {
    connection.close(PRE_JOIN_LIMIT.into(), b"too many sessions");
    return Ok(());
  };
  // Dual-stack sockets report IPv4 peers as IPv4-mapped IPv6; log them in their canonical form.
  debug!(%remote_ip, "session accepted");
  let (mut send, mut recv) = tokio::time::timeout(shared.config.join_timeout, connection.accept_bi())
    .await
    .map_err(|_| SessionError::JoinTimeout)??;

  let max_bytes = shared.config.max_signaling_message_bytes;
  let first = tokio::time::timeout(
    shared.config.join_timeout,
    read_message::<CallClientMessage>(&mut recv, max_bytes),
  )
  .await
  .map_err(|_| SessionError::JoinTimeout)??;
  let CallClientMessage::Join { token, capabilities } = first else {
    reject(&mut send, ErrorCode::ProtocolViolation, "expected join").await;
    return Err(SessionError::ExpectedJoin);
  };
  let grant = match shared.verifier.verify(&token) {
    Ok(grant) => grant,
    Err(error) => {
      reject(&mut send, ErrorCode::InvalidJoinToken, &error.to_string()).await;
      return Ok(());
    },
  };

  let (signal_tx, signal_rx) = mpsc::channel(shared.config.signaling_queue_len.max(1));
  let handle = ParticipantHandle {
    link: Arc::new(connection.clone()),
    signal_tx: signal_tx.clone(),
  };
  let (room, session_id, outcome) = match shared.rooms.join(grant, capabilities, handle) {
    Ok(joined) => joined,
    Err(error) => {
      let code = match error {
        RoomError::Locked => ErrorCode::CallLocked,
        RoomError::Removed => ErrorCode::RemovedFromCall,
        _ => ErrorCode::AlreadyJoined,
      };
      reject(&mut send, code, &error.to_string()).await;
      return Err(error.into());
    },
  };

  drop(prejoin_slot);
  // The call the device is in: the running call of its conversation when the token named another.
  let grant = JoinGrant {
    call_id: room.call_id(),
    ..grant
  };
  // A device taking its slot back is still in the server's roster.
  if outcome == JoinOutcome::Joined {
    shared.reporter.joined(grant);
  }
  let session = ParticipantSession {
    connection,
    room: Arc::clone(&room),
    grant,
    signal_tx,
    max_bytes,
    roster_now: Arc::clone(&shared.roster_now),
  };
  let (end, result) = session.run(send, recv, signal_rx).await;
  drop(session);
  release_slot(shared, &room, grant, session_id, end).await;
  result
}

/// Ends a session's hold on its slot: at once after a close, after the reconnect grace period
/// after a loss (unless the device took the slot back on a new connection meanwhile).
async fn release_slot(
  shared: &Shared,
  room: &Room,
  grant: JoinGrant,
  session_id: SessionId,
  end: SessionEnd,
) {
  if end == SessionEnd::Lost
    && shared
      .rooms
      .disconnect(room, grant.device_id, session_id, Instant::now())
  {
    tokio::time::sleep(shared.config.reconnect_grace).await;
  }
  // Going away: the participant rejoins on the restarted SFU, and the server keeps it in the call.
  if shared.rooms.leave(room, grant.device_id, session_id) && !shared.rooms.is_going_away() {
    shared.reporter.left(grant);
  }
}

impl SfuControl {
  /// Every call on this SFU and who is in it, for a server that starts and takes them over.
  pub fn roster(&self) -> SfuRosterReport {
    self.shared.roster()
  }

  pub fn apply(&self, request: &SfuControlRequest) -> Result<(), RoomError> {
    self
      .shared
      .rooms
      .control(request.call_id, request.by_user_id, &request.action)
  }
}

impl ParticipantSession {
  /// Runs until the client disconnects or violates the protocol.
  async fn run(
    &self,
    send: SendStream,
    recv: RecvStream,
    signal_rx: mpsc::Receiver<Outgoing>,
  ) -> (SessionEnd, Result<(), SessionError>) {
    let result = self.serve(send, recv, signal_rx).await;
    // A failed stream read says little; the connection's close reason tells a loss from a close.
    // Still open means the session failed on its own (a protocol violation): closed by us.
    let end = match &result {
      Err(SessionError::Connection(error)) => session_end(error),
      _ => match tokio::time::timeout(Duration::ZERO, self.connection.closed()).await {
        Ok(error) => session_end(&error),
        Err(_) => SessionEnd::Closed,
      },
    };
    info!(
      call_id = %self.grant.call_id,
      participant_id = %self.grant.device_id,
      ?end,
      "session closed"
    );
    (end, result)
  }

  async fn serve(
    &self,
    send: SendStream,
    mut recv: RecvStream,
    signal_rx: mpsc::Receiver<Outgoing>,
  ) -> Result<(), SessionError> {
    let writer = tokio::spawn(write_signaling(send, signal_rx));
    let signaling = async {
      loop {
        let message = read_message::<CallClientMessage>(&mut recv, self.max_bytes).await?;
        self.handle_client_message(message);
      }
    };
    let media = async {
      loop {
        let datagram = self.connection.receive_datagram().await?;
        if let Err(error) =
          self
            .room
            .forward_datagram(self.grant.device_id, &datagram.payload(), Instant::now())
        {
          debug!(participant_id = %self.grant.device_id, %error, "dropping invalid datagram");
        }
      }
    };
    let periodic = async {
      let mut interval = tokio::time::interval(PERIODIC_INTERVAL);
      loop {
        interval.tick().await;
        self.room.on_timer(self.grant.device_id, Instant::now());
        if let Some(feedback) = self.room.take_uplink_feedback(self.grant.device_id) {
          // Fails only when the writer task ended, i.e. the session is closing anyway.
          let _ = self
            .signal_tx
            .try_send(Outgoing::Event(CallEvent::TransportFeedback { feedback }));
        }
      }
    };
    let result: Result<(), SessionError> = tokio::select! {
      result = signaling => result,
      result = media => result,
      () = periodic => Ok(()),
    };
    writer.abort();
    result
  }

  fn handle_client_message(&self, message: CallClientMessage) {
    // Proves the session's path to a browser, which can't see QUIC keep-alives. One small answer
    // per ping; when the queue is full it is dropped, and the client's next ping asks again.
    if let CallClientMessage::Ping { id } = message {
      let _ = self.signal_tx.try_send(Outgoing::Pong(id));
      return;
    }
    // The account's other devices leave for this one; one of them may be in the server's roster
    // only (its leave got lost), which a roster right after repairs at once instead of within the
    // interval.
    let is_move_here = matches!(message, CallClientMessage::MoveHere);
    if let Err(error) = self
      .room
      .on_client_message(self.grant.device_id, message, Instant::now())
    {
      // Fails only when the writer task ended, i.e. the session is closing anyway.
      let _ = self.signal_tx.try_send(Outgoing::Error(error));
    }
    if is_move_here {
      self.roster_now.notify_one();
    }
  }
}

/// Deliberate closes (by the client or by us) end the session; everything else is a loss the client
/// may recover from by rejoining.
fn session_end(error: &wtransport::error::ConnectionError) -> SessionEnd {
  use wtransport::error::ConnectionError;
  match error {
    ConnectionError::ApplicationClosed(_)
    | ConnectionError::LocallyClosed
    | ConnectionError::LocalH3Error(_) => SessionEnd::Closed,
    ConnectionError::ConnectionClosed(_)
    | ConnectionError::TimedOut
    | ConnectionError::QuicProto(_)
    | ConnectionError::CidsExhausted => SessionEnd::Lost,
  }
}

async fn write_signaling(mut send: SendStream, mut signal_rx: mpsc::Receiver<Outgoing>) {
  let mut sequence = EventSequence::default();
  while let Some(outgoing) = signal_rx.recv().await {
    if let Err(error) = write_message(&mut send, &sequence.frame(outgoing)).await {
      debug!(%error, "signaling write failed");
      return;
    }
  }
}

async fn reject(send: &mut SendStream, error_code: ErrorCode, message: &str) {
  let error = CallServerMessage::Error {
    error: ErrorBody::new(error_code, message),
  };
  // Best effort: the session is being closed anyway.
  if write_message(send, &error).await.is_ok() {
    let _ = send.finish().await;
  }
}
