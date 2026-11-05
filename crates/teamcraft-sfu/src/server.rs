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
//!
//! Networks that block UDP reach the same rooms through the WebSocket fallback ([`crate::fallback`]),
//! admitted by the same [`admit`]. A session there moves to a QUIC connection once one gets through
//! ([`crate::switch`], #1309): that connection starts with `switch` instead of `join`.

use std::{
  future::Future,
  net::{SocketAddr, UdpSocket},
  sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
  },
  time::{Duration, Instant},
};

use socket2::{Domain, Protocol, Socket, Type};
use teamcraft_proto::{
  CallClientMessage, CallEvent, CallId, CallServerMessage, DeviceId, ErrorBody, ErrorCode,
  SFU_GOING_AWAY_CLOSE_CODE, SFU_SWITCHED_CLOSE_CODE, SfuControlRequest, SfuRosterReport,
};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tracing::{Instrument, debug, error, info, info_span, warn};
use wtransport::{
  Connection, Endpoint, Identity, RecvStream, SendStream, ServerConfig,
  endpoint::{IncomingSession, endpoint_side::Server},
  tls::Sha256Digest,
};

use crate::{
  auth::{JoinGrant, JoinTokenVerifier},
  config::SfuConfig,
  fallback,
  net::bind_dual_stack,
  prejoin::{PreJoinLimiter, PreJoinSlot},
  rejoin::{RejoinLimiter, RejoinTurn},
  reporting::{NoReporter, ParticipantReporter},
  room::{JoinOutcome, ParticipantHandle, ParticipantLink, Room, RoomError, RoomStats, Rooms, SessionId},
  signaling::{Dequeued, Handover, Outgoing, SignalQueue, SignalingError, read_message, write_message},
  switch::{SwitchRegistration, SwitchSecret, SwitchSecrets, SwitchTarget},
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
  maybe_fallback_listener: Option<TcpListener>,
  shared: Arc<Shared>,
}

pub(crate) struct Shared {
  pub(crate) config: SfuConfig,
  pub(crate) rooms: Rooms,
  pub(crate) verifier: Arc<dyn JoinTokenVerifier>,
  reporter: Arc<dyn ParticipantReporter>,
  prejoin: Arc<PreJoinLimiter>,
  /// The fallback's own: what fills it, through a proxy or not, costs a QUIC session nothing.
  pub(crate) fallback_prejoin: Arc<PreJoinLimiter>,
  /// Unjoined fallback connections per device, of those that showed a valid join token in the
  /// upgrade (#1297): they count against their device instead of their address.
  pub(crate) fallback_device_prejoin: Arc<PreJoinLimiter<(CallId, DeviceId)>>,
  /// The pace of each device's joins of each call, on both transports.
  rejoins: RejoinLimiter,
  /// When this SFU started: the roster tells the server how fresh its rooms are.
  started_at: Instant,
  /// Wakes the roster reporter before its interval (a call moved between devices).
  roster_now: Arc<Notify>,
  /// Open connections of the WebSocket fallback, for the metrics.
  pub(crate) fallback_connections: AtomicUsize,
  /// Told the operator of a forwarded address from somebody who isn't a trusted proxy.
  pub(crate) has_warned_of_forwarding: AtomicBool,
  /// The secrets that move running sessions to another connection (#1309).
  pub(crate) switch_secrets: SwitchSecrets,
}

/// Carries out meeting host controls on this SFU's calls (see [`crate::control`]).
#[derive(Clone)]
pub struct SfuControl {
  shared: Arc<Shared>,
}

/// One joined participant's session, whatever carries it.
pub(crate) struct ParticipantSession {
  room: Arc<Room>,
  grant: JoinGrant,
  signal_tx: mpsc::Sender<Outgoing>,
  max_bytes: usize,
  roster_now: Arc<Notify>,
}

/// How a participant's session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionEnd {
  /// The client left, or the SFU closed the connection (host control, protocol violation).
  Closed,
  /// The connection broke (idle timeout, reset): the client is expected back.
  Lost,
}

#[derive(Debug, Error)]
pub enum SfuError {
  #[error("binding the endpoint failed: {0}")]
  Bind(#[source] std::io::Error),
  #[error("binding the fallback's listener failed: {0}")]
  BindFallback(#[source] std::io::Error),
  #[error("the TLS identity has no certificate")]
  NoCertificate,
  #[error("the idle timeout is out of range")]
  IdleTimeout,
}

/// What the first frame of a session led to, on either transport.
pub(crate) enum Admission {
  Admitted(Admitted),
  Refused(Refusal),
}

/// A session that joined its call.
pub(crate) struct Admitted {
  pub(crate) room: Arc<Room>,
  pub(crate) grant: JoinGrant,
  pub(crate) session_id: SessionId,
  /// What its `joined` carried, to keep while the session runs ([`register_switch`]).
  pub(crate) maybe_switch_secret: Option<SwitchSecret>,
}

/// The writer task of a QUIC session's signaling stream.
struct SessionWriter {
  task: JoinHandle<WriterEnd>,
  /// It has the session's queue: from the start, or once the connection it took the session over
  /// from handed it on.
  has_queue: Arc<AtomicBool>,
}

/// Why a QUIC session's signaling writer stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriterEnd {
  /// The stream failed, or the queue ended with the session.
  Stopped,
  /// The session moved to another connection, and the queue went there (#1309).
  HandedOver,
  /// The connection this one took the session over from went before handing on its queue.
  Orphaned,
}

/// How serving a QUIC session ended, short of an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Served {
  Ended,
  HandedOver,
  Orphaned,
}

/// A first frame the session is refused for: what to tell the client, and how the session ended.
pub(crate) struct Refusal {
  code: ErrorCode,
  text: String,
  /// When the client may try again (a join that came too soon).
  maybe_retry_after: Option<Duration>,
  pub(crate) result: Result<(), SessionError>,
}

#[derive(Debug, Error)]
pub(crate) enum SessionError {
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
    let server_config = ServerConfig::builder()
      .with_bind_socket(bind_socket(&config).map_err(SfuError::Bind)?)
      .with_identity(identity)
      .keep_alive_interval(Some(config.keep_alive_interval))
      .max_idle_timeout(Some(config.idle_timeout))
      .map_err(|_| SfuError::IdleTimeout)?
      // Clients moving between networks keep their connection (validated by QUIC path checks).
      .allow_migration(true)
      .build();
    let endpoint = Endpoint::server(server_config).map_err(SfuError::Bind)?;
    let rooms = Rooms::new(config.clone());
    let rejoins = RejoinLimiter::new(config.rejoin);
    // Calls on QUIC don't depend on it: a port that is taken costs the fallback, not the SFU.
    let maybe_fallback_listener = config.fallback.maybe_bind_addr.and_then(|addr| {
      bind_dual_stack(addr)
        .inspect_err(|error| error!(%addr, %error, "no call fallback: its port can't be bound"))
        .inspect(|_| fallback::warn_if_listening_wider_than_its_proxies(&config.fallback, addr))
        .ok()
    });
    if maybe_fallback_listener.is_some() {
      fallback::warn_if_descriptors_are_short(&config.fallback);
    }
    Ok(Self {
      endpoint,
      certificate_hash,
      maybe_fallback_listener,
      shared: Arc::new(Shared {
        config,
        rooms,
        verifier,
        reporter: Arc::new(NoReporter),
        prejoin: Arc::new(PreJoinLimiter::default()),
        fallback_prejoin: Arc::new(PreJoinLimiter::default()),
        fallback_device_prejoin: Arc::new(PreJoinLimiter::default()),
        rejoins,
        started_at: Instant::now(),
        roster_now: Arc::new(Notify::new()),
        fallback_connections: AtomicUsize::new(0),
        has_warned_of_forwarding: AtomicBool::new(false),
        switch_secrets: SwitchSecrets::default(),
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

  /// Where the WebSocket fallback listens, if it does.
  pub fn fallback_addr(&self) -> Result<Option<SocketAddr>, SfuError> {
    self
      .maybe_fallback_listener
      .as_ref()
      .map(TcpListener::local_addr)
      .transpose()
      .map_err(SfuError::BindFallback)
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
  pub async fn run_until(mut self, shutdown: impl Future<Output = ()>) {
    let rosters = tokio::spawn(report_rosters(Arc::clone(&self.shared)));
    let maybe_fallback = self.maybe_fallback_listener.take();
    let shared = Arc::clone(&self.shared);
    let fallback = async move {
      match maybe_fallback {
        Some(listener) => fallback::serve(listener, shared).await,
        None => std::future::pending().await,
      }
    };
    tokio::select! {
      () = self.accept_sessions() => {},
      () = fallback => {},
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

/// The SFU's socket, dual stack where bound to IPv6, with [`SfuConfig::udp_receive_buffer_bytes`]
/// (as far as the system allows: it caps the size at `net.core.rmem_max` without an error).
fn bind_socket(config: &SfuConfig) -> std::io::Result<UdpSocket> {
  let socket = Socket::new(
    Domain::for_address(config.bind_addr),
    Type::DGRAM,
    Some(Protocol::UDP),
  )?;
  if config.bind_addr.is_ipv6() {
    socket.set_only_v6(false)?;
  }
  socket.set_recv_buffer_size(config.udp_receive_buffer_bytes)?;
  let got_bytes = socket.recv_buffer_size()?;
  if got_bytes < config.udp_receive_buffer_bytes {
    warn!(
      wanted_bytes = config.udp_receive_buffer_bytes,
      got_bytes,
      "the system caps the UDP receive buffer, big keyframes may get lost while the SFU is busy: \
       raise net.core.rmem_max"
    );
  }
  socket.bind(&config.bind_addr.into())?;
  Ok(socket.into())
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

  let first = tokio::time::timeout(
    shared.config.join_timeout,
    read_message::<CallClientMessage>(&mut recv, shared.config.max_signaling_message_bytes),
  )
  .await
  .map_err(|_| SessionError::JoinTimeout)??;

  if let CallClientMessage::Switch { secret } = &first {
    return take_over(shared, connection, (send, recv), secret.as_slice(), prejoin_slot).await;
  }

  let (signal_tx, signal_rx) = mpsc::channel(shared.config.signaling_queue_len.max(1));
  let handle = ParticipantHandle {
    link: Arc::new(connection.clone()),
    signal_tx: signal_tx.clone(),
  };
  let Admitted {
    room,
    grant,
    session_id,
    maybe_switch_secret,
  } = match admit(shared, first, handle).await {
    Admission::Admitted(admitted) => admitted,
    Admission::Refused(refusal) => {
      reject(&mut send, refusal.message()).await;
      return refusal.result;
    },
  };

  drop(prejoin_slot);
  let handover = Arc::new(Handover::default());
  let registration = register_switch(shared, maybe_switch_secret, &room, grant, session_id, &handover);
  let session = ParticipantSession::new(Arc::clone(&room), grant, signal_tx, shared);
  let writer = SessionWriter::spawn(send, SignalQueue::new(signal_rx, handover));
  let (end, result) = session.run(connection, recv, writer).await;
  drop(session);
  // The connection is gone: so is the way to move it.
  drop(registration);
  release_slot(shared, &room, grant, session_id, end).await;
  result
}

/// A connection that takes over a session of its client instead of joining (#1309): `secret` is
/// what that session's `joined` carried. It went through the same limits before this as a join,
/// but it needs no token and takes no turn of the join pace: nobody joins, nothing re-keys. The
/// answer is `switched` (or an error, then the close), and the first thing on the stream; the
/// session's events follow once the old connection sent what was queued for it.
async fn take_over(
  shared: &Shared,
  connection: Connection,
  (mut send, recv): (SendStream, RecvStream),
  secret: &[u8],
  prejoin_slot: PreJoinSlot,
) -> Result<(), SessionError> {
  let Some(SwitchTarget {
    room,
    grant,
    session_id: from,
    handover,
  }) = shared.switch_secrets.take(secret)
  else {
    debug!("switch refused: no running session has this secret");
    let error = ErrorBody::new(ErrorCode::InvalidJoinToken, "no session to take over");
    reject(&mut send, CallServerMessage::Error { error }).await;
    return Ok(());
  };
  let arrives = handover.prepare();
  let link: Arc<dyn ParticipantLink> = Arc::new(connection.clone());
  let (session_id, signal_tx) = match shared.rooms.switch(&room, grant.device_id, from, link) {
    Ok(switched) => switched,
    Err(error) => {
      handover.cancel();
      debug!(call_id = %grant.call_id, participant_id = %grant.device_id, %error, "switch refused");
      let error = ErrorBody::new(ErrorCode::Unavailable, error.to_string());
      reject(&mut send, CallServerMessage::Error { error }).await;
      return Ok(());
    },
  };
  // Only the old connection's queue may hold on to it: when that goes unsent, so does the queue.
  drop(handover);
  drop(prejoin_slot);
  if let Err(error) = write_message(&mut send, &CallServerMessage::Switched).await {
    // Reading tells how the connection went.
    debug!(%error, "could not confirm a switch");
  }
  let session = ParticipantSession::new(Arc::clone(&room), grant, signal_tx, shared);
  let writer = SessionWriter::spawn_taken_over(send, arrives);
  let (end, result) = session.run(connection, recv, writer).await;
  drop(session);
  release_slot(shared, &room, grant, session_id, end).await;
  result
}

/// Keeps the secret `joined` carried while the session runs, so its client can move it to another
/// connection (#1309). `None` without a secret.
pub(crate) fn register_switch<'a>(
  shared: &'a Shared,
  maybe_secret: Option<SwitchSecret>,
  room: &Arc<Room>,
  grant: JoinGrant,
  session_id: SessionId,
  handover: &Arc<Handover>,
) -> Option<SwitchRegistration<'a>> {
  let target = SwitchTarget {
    room: Arc::clone(room),
    grant,
    session_id,
    handover: Arc::clone(handover),
  };
  shared.switch_secrets.register(maybe_secret?, target)
}

/// The join every session starts with, whichever transport brought it: the first message must be
/// `join`, its token must verify, the device must not join this call more often than
/// [`SfuConfig::rejoin`] allows, and the room must take the device. The one place a join token is
/// checked (ARCHITECTURE.md §6.6: the fallback's first frame gets what QUIC's gets), and the one
/// place joins are paced (#1227): a join whose turn is near waits for it here, holding nothing but
/// its unjoined slot; one whose turn is further off is refused with the time to wait.
pub(crate) async fn admit(shared: &Shared, first: CallClientMessage, handle: ParticipantHandle) -> Admission {
  let CallClientMessage::Join { token, capabilities } = first else {
    return Admission::Refused(Refusal {
      code: ErrorCode::ProtocolViolation,
      text: "expected join".to_owned(),
      maybe_retry_after: None,
      result: Err(SessionError::ExpectedJoin),
    });
  };
  let grant = match shared.verifier.verify(&token) {
    Ok(grant) => grant,
    Err(error) => {
      return Admission::Refused(Refusal {
        code: ErrorCode::InvalidJoinToken,
        text: error.to_string(),
        maybe_retry_after: None,
        result: Ok(()),
      });
    },
  };
  match shared
    .rejoins
    .take_turn(grant.call_id, grant.device_id, Instant::now())
  {
    RejoinTurn::Now => {},
    RejoinTurn::After(wait) => {
      debug!(call_id = %grant.call_id, participant_id = %grant.device_id, ?wait, "join waits for its turn");
      tokio::time::sleep(wait).await;
    },
    RejoinTurn::Refused { retry_after } => {
      // Not above debug: whoever loops would choose the size of the log.
      debug!(
        call_id = %grant.call_id,
        participant_id = %grant.device_id,
        ?retry_after,
        "join refused: the device joins this call too often"
      );
      return Admission::Refused(Refusal {
        code: ErrorCode::RateLimited,
        text: "joined this call too often, try again later".to_owned(),
        maybe_retry_after: Some(retry_after),
        result: Ok(()),
      });
    },
  }
  // In `joined`, for this session only: a client on the fallback moves it to QUIC with it (#1309).
  let maybe_switch_secret = SwitchSecret::random();
  let maybe_wire_secret = maybe_switch_secret.as_ref().map(SwitchSecret::to_wire);
  let (room, session_id, outcome) = match shared.rooms.join(grant, capabilities, handle, maybe_wire_secret) {
    Ok(joined) => joined,
    Err(error) => {
      let code = match error {
        RoomError::Locked => ErrorCode::CallLocked,
        RoomError::Removed => ErrorCode::RemovedFromCall,
        RoomError::TooManyDevices(_) => ErrorCode::CallDeviceLimit,
        _ => ErrorCode::AlreadyJoined,
      };
      return Admission::Refused(Refusal {
        code,
        text: error.to_string(),
        maybe_retry_after: None,
        result: Err(error.into()),
      });
    },
  };
  // The call the device is in: the running call of its conversation when the token named another.
  let grant = JoinGrant {
    call_id: room.call_id(),
    ..grant
  };
  // A device taking its slot back is still in the server's roster.
  if outcome == JoinOutcome::Joined {
    shared.reporter.joined(grant);
  }
  Admission::Admitted(Admitted {
    room,
    grant,
    session_id,
    maybe_switch_secret,
  })
}

impl Refusal {
  /// What the client gets to read.
  pub(crate) fn message(&self) -> CallServerMessage {
    let mut error = ErrorBody::new(self.code.clone(), &self.text);
    if let Some(retry_after) = self.maybe_retry_after {
      // Rounded up: a client that waits as told finds its turn there.
      let secs = retry_after.as_secs() + u64::from(retry_after.subsec_nanos() > 0);
      error = error.with_retry_after_secs(secs);
    }
    CallServerMessage::Error { error }
  }
}

/// Ends a session's hold on its slot: at once after a close, after the reconnect grace period
/// after a loss, or as soon as the device took the slot back on a new connection: a lost session
/// waits only while it may still matter, so a device that keeps reconnecting leaves at most one
/// waiting behind.
pub(crate) async fn release_slot(
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
    room
      .wait_for_return(grant.device_id, session_id, shared.config.reconnect_grace)
      .await;
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

  /// The rooms in numbers, for a scrape of the metrics.
  pub fn room_stats(&self) -> RoomStats {
    self.shared.rooms.stats()
  }

  /// Open connections on the WebSocket fallback, for a scrape of the metrics.
  pub fn fallback_connections(&self) -> usize {
    self.shared.fallback_connections.load(Ordering::Relaxed)
  }

  pub fn apply(&self, request: &SfuControlRequest) -> Result<(), RoomError> {
    self
      .shared
      .rooms
      .control(request.call_id, request.by_user_id, &request.action)
  }
}

impl ParticipantSession {
  pub(crate) fn new(
    room: Arc<Room>,
    grant: JoinGrant,
    signal_tx: mpsc::Sender<Outgoing>,
    shared: &Shared,
  ) -> Self {
    Self {
      room,
      grant,
      signal_tx,
      max_bytes: shared.config.max_signaling_message_bytes,
      roster_now: Arc::clone(&shared.roster_now),
    }
  }

  /// Runs a QUIC session until the client disconnects or violates the protocol, or the session
  /// moves to another connection.
  async fn run(
    &self,
    connection: Connection,
    recv: RecvStream,
    writer: SessionWriter,
  ) -> (SessionEnd, Result<(), SessionError>) {
    let SessionWriter { mut task, has_queue } = writer;
    let result = self.serve(&connection, recv, &mut task).await;
    task.abort();
    // A failed stream read says little; the connection's close reason tells a loss from a close.
    // Still open means the session failed on its own (a protocol violation): closed by us.
    let end = match &result {
      Ok(Served::HandedOver) => {
        // Everything queued for it went out: the client reads up to this close, then the new one.
        ParticipantLink::close_gracefully(&connection, SFU_SWITCHED_CLOSE_CODE, b"switched").await;
        SessionEnd::Closed
      },
      Ok(Served::Orphaned) => SessionEnd::Lost,
      // It took the session over, but its events never came: the client rejoins into the slot.
      _ if !has_queue.load(Ordering::Acquire) => SessionEnd::Lost,
      Err(SessionError::Connection(error)) => session_end(error),
      _ => match tokio::time::timeout(Duration::ZERO, connection.closed()).await {
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
    (end, result.map(|_| ()))
  }

  async fn serve(
    &self,
    connection: &Connection,
    mut recv: RecvStream,
    writer: &mut JoinHandle<WriterEnd>,
  ) -> Result<Served, SessionError> {
    let signaling = async {
      loop {
        let message = read_message::<CallClientMessage>(&mut recv, self.max_bytes).await?;
        self.handle_client_message(message);
      }
    };
    let media = async {
      loop {
        let datagram = connection.receive_datagram().await?;
        self.on_datagram(&datagram.payload());
      }
    };
    let written = async {
      match writer.await {
        Ok(WriterEnd::HandedOver) => Served::HandedOver,
        Ok(WriterEnd::Orphaned) => Served::Orphaned,
        // A broken stream shows on the reading side too, with the connection's close reason.
        Ok(WriterEnd::Stopped) | Err(_) => std::future::pending().await,
      }
    };
    tokio::select! {
      result = signaling => result,
      result = media => result,
      () = self.periodic() => Ok(Served::Ended),
      served = written => Ok(served),
    }
  }

  /// One media datagram from the client.
  pub(crate) fn on_datagram(&self, datagram: &[u8]) {
    if let Err(error) = self
      .room
      .forward_datagram(self.grant.device_id, datagram, Instant::now())
    {
      debug!(participant_id = %self.grant.device_id, %error, "dropping invalid datagram");
    }
  }

  /// Timers and uplink feedback; runs for as long as the session does.
  pub(crate) async fn periodic(&self) {
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
  }

  pub(crate) fn handle_client_message(&self, message: CallClientMessage) {
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

impl SessionWriter {
  /// The writer of a session that starts on this connection.
  fn spawn(send: SendStream, queue: SignalQueue) -> Self {
    Self {
      task: tokio::spawn(write_signaling(send, queue)),
      has_queue: Arc::new(AtomicBool::new(true)),
    }
  }

  /// The writer of a session this connection took over: it writes once the old connection sent
  /// what was queued for it and handed the queue on.
  fn spawn_taken_over(send: SendStream, arrives: oneshot::Receiver<SignalQueue>) -> Self {
    let has_queue = Arc::new(AtomicBool::new(false));
    let marking = Arc::clone(&has_queue);
    let task = tokio::spawn(async move {
      match arrives.await {
        Ok(queue) => {
          marking.store(true, Ordering::Release);
          write_signaling(send, queue).await
        },
        Err(_) => WriterEnd::Orphaned,
      }
    });
    Self { task, has_queue }
  }
}

/// Writes the session's queue to the stream until it fails or the session moves on.
async fn write_signaling(mut send: SendStream, mut queue: SignalQueue) -> WriterEnd {
  while let Some(next) = queue.next().await {
    match next {
      Dequeued::Message(message) => {
        if let Err(error) = write_message(&mut send, &message).await {
          debug!(%error, "signaling write failed");
          return WriterEnd::Stopped;
        }
      },
      Dequeued::HandOver => {
        if !queue.hand_over() {
          debug!("the connection the session moved to went first");
        }
        return WriterEnd::HandedOver;
      },
    }
  }
  WriterEnd::Stopped
}

async fn reject(send: &mut SendStream, refusal: CallServerMessage) {
  // Best effort: the session is being closed anyway.
  if write_message(send, &refusal).await.is_ok() {
    let _ = send.finish().await;
  }
}
