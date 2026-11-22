//! The direct path of a 1:1 call (#127, ARCHITECTURE.md §6.7, e2ee.md §8.4): its own UDP socket, a
//! QUIC endpoint on it that dials and accepts at once with mutually pinned ephemeral certificates,
//! the SFU's `reflect` for the reflexive address, the sealed offers, the dialing rounds, the pings,
//! and the switch of the own audio between the SFU and the direct connection.
//!
//! The decisions are `crate::direct_path`'s ([`PathSelector`], eligibility, offers); this module
//! does the I/O. It runs only while this device may go direct: the policy it verified allows it for
//! these two devices, its user's setting is on, and the call has exactly two devices whose DAVE
//! group this device authenticated. Anything else tears it down, and the media goes through the SFU.
//!
//! Only the audio takes the direct path in this step (the full voice and the screen's sound); the
//! video keeps going through the SFU, which picks its layers per receiver. The audio keeps going to
//! the SFU as well: its speaker selection and voice guard watch it, and the SFU forwards it again
//! the moment either device reports its direct path down. Addresses are never logged.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use rand_core::OsRng;
use teamcraft_crypto::direct_path::DirectPathKey;
use teamcraft_proto::{
  Base64Bytes, CallClientMessage, CallPeerStanding, CallServerMessage, DeviceId, DirectMediaPolicy,
  DirectOfferBody,
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Bytes;
use tracing::{debug, info};
use wtransport::ClientConfig;
use wtransport::endpoint::endpoint_side::Client;
use wtransport::quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use wtransport::quinn::{self, EndpointConfig, IdleTimeout, TokioRuntime, TransportConfig};
use wtransport::tls::rustls::client::danger::{
  HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
};
use wtransport::tls::rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use wtransport::tls::rustls::pki_types::{
  CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime,
};
use wtransport::tls::rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use wtransport::tls::rustls::{self, DigitallySignedStruct, DistinguishedName, SignatureScheme};
use wtransport::tls::{Certificate, Sha256Digest};
use wtransport::{Endpoint, Identity};

use super::{bind_wildcard, local_route_to, read_framed, with_trusted_roots, write_framed};
use crate::call_engine::CallEngine;
use crate::direct_path::{
  CERTIFICATE_HASH_LEN, DIAL_TIMEOUT, DirectControl, DirectDatagram, DirectPair, DirectPathInputs,
  DownReason, OfferInbox, PathChange, PathSelector, Pinger, RelayArbiter, accepted_connection_wins,
  dial_delay, direct_copy, eligibility, offer_epoch, open_offer, own_candidates, read_direct, seal_offer,
};
use crate::sfu_address::PinnedResolver;

/// The direct connection's ALPN: only another TeamCraft direct path endpoint speaks it.
const DIRECT_ALPN: &[u8] = b"teamcraft-direct/1";
/// The name both sides put in the handshake; the pinned certificate hash decides, not the name.
const DIRECT_SERVER_NAME: &str = "teamcraft-direct";
/// A direct connection that hears nothing this long is gone (the pings say so much sooner).
const DIRECT_IDLE_TIMEOUT: Duration = Duration::from_secs(3);
/// The close of a direct connection: the other one of a pair won, the path is no longer allowed, or
/// the call ended.
const DIRECT_CLOSE: u32 = 0;
/// How long the SFU's reflection may take.
const REFLECT_TIMEOUT: Duration = Duration::from_secs(5);
/// After the direct path socket couldn't be opened (no UDP, no certificate), the next try waits.
const SOCKET_RETRY_AFTER: Duration = Duration::from_secs(10);
/// Addresses whose route shows the interface the system sends from in each family: documentation
/// ranges (RFC 5737, RFC 3849), never contacted; the route lookup sends nothing.
const IPV4_ROUTE_PROBE: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 9);
const IPV6_ROUTE_PROBE: SocketAddr =
  SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)), 9);

/// What the call's direct path needs to know besides the call itself: the workspace policy this
/// device verified, its user's setting, and how it places itself and the devices of the call
/// (ARCHITECTURE.md §6.7, "Who may"). A device it can't place is an outsider.
#[derive(Debug, Clone)]
pub struct DirectPathConfig {
  pub policy: DirectMediaPolicy,
  /// `calls.direct_connections`: the user allows direct connections.
  pub does_user_allow: bool,
  pub own: CallPeerStanding,
  /// The standing of each device that may be in the call, from the verified roster.
  pub standings: Vec<(DeviceId, CallPeerStanding)>,
}

/// The call's direct path, owned by the call task.
pub(super) struct DirectPath {
  config: DirectPathConfig,
  maybe_socket: Option<DirectSocket>,
  /// When opening the socket last failed: the next try waits [`SOCKET_RETRY_AFTER`].
  maybe_socket_failed_at: Option<Instant>,
  /// The pair and the epoch's key while the call is eligible.
  maybe_pair: Option<DirectPair>,
  maybe_key: Option<DirectPathKey>,
  inbox: OfferInbox,
  /// This device's offers of the current epoch, numbered.
  counter: u64,
  /// An offer of a later epoch than this device holds: opened once its key is there.
  maybe_held_offer: Option<Base64Bytes>,
  maybe_peer: Option<PeerOffer>,
  maybe_reflexive: Option<SocketAddr>,
  maybe_session_secret: Option<Base64Bytes>,
  maybe_reflect_target: Option<ReflectTarget>,
  selector: PathSelector,
  pinger: Pinger,
  arbiter: RelayArbiter,
  maybe_connection: Option<DirectConnection>,
  maybe_dialing: Option<JoinHandle<()>>,
  maybe_reflecting: Option<JoinHandle<()>>,
  inputs_tx: mpsc::UnboundedSender<DirectInput>,
  inputs: mpsc::UnboundedReceiver<DirectInput>,
  /// Where the own audio's direct copies go: shared with the sinks that send the media.
  sender: DirectSender,
}

/// Sends the own audio's copies on the direct connection while it carries them.
#[derive(Clone, Default)]
pub(super) struct DirectSender {
  maybe_connection: Arc<Mutex<Option<quinn::Connection>>>,
}

/// Where the SFU's `reflect` goes: the session connection's SFU.
#[derive(Clone)]
pub(super) struct ReflectTarget {
  pub(super) sfu_url: String,
  pub(super) address: SocketAddr,
  pub(super) certificate_hashes: Vec<[u8; 32]>,
}

/// What happened on the direct path's socket.
pub(super) enum DirectInput {
  Connected {
    connection: quinn::Connection,
    is_accepted: bool,
  },
  Datagram {
    connection_id: usize,
    bytes: Bytes,
    read_at: Instant,
  },
  Closed {
    connection_id: usize,
  },
  Reflected(SocketAddr),
}

/// The direct path socket: one QUIC endpoint that dials and accepts, and its certificate.
struct DirectSocket {
  endpoint: quinn::Endpoint,
  identity: Identity,
  certificate_hash: [u8; 32],
  accepting: JoinHandle<()>,
}

/// The other device's offer, as this device dials it.
struct PeerOffer {
  certificate_hash: [u8; 32],
  candidates: Vec<SocketAddr>,
}

struct DirectConnection {
  connection: quinn::Connection,
  is_accepted: bool,
  reading: JoinHandle<()>,
}

/// Accepts exactly one certificate, by its SHA-256 hash: the one the other device's offer named
/// (e2ee.md §8.4, "The direct connection"). Signatures are checked as usual.
#[derive(Debug)]
struct PinnedPeer {
  certificate_hash: [u8; 32],
  algorithms: WebPkiSupportedAlgorithms,
}

/// Why the direct path socket or its TLS couldn't be set up.
#[derive(Debug, thiserror::Error)]
enum DirectSetupError {
  #[error(transparent)]
  Io(#[from] std::io::Error),
  #[error(transparent)]
  Tls(#[from] rustls::Error),
  #[error("the direct path's certificate: {0}")]
  Certificate(String),
  #[error("the QUIC crypto configuration: {0}")]
  Quic(String),
}

impl DirectPath {
  pub(super) fn new(config: DirectPathConfig) -> Self {
    let (inputs_tx, inputs) = mpsc::unbounded_channel();
    Self {
      config,
      maybe_socket: None,
      maybe_socket_failed_at: None,
      maybe_pair: None,
      maybe_key: None,
      inbox: OfferInbox::default(),
      counter: 0,
      maybe_held_offer: None,
      maybe_peer: None,
      maybe_reflexive: None,
      maybe_session_secret: None,
      maybe_reflect_target: None,
      selector: PathSelector::new(),
      pinger: Pinger::default(),
      arbiter: RelayArbiter::default(),
      maybe_connection: None,
      maybe_dialing: None,
      maybe_reflecting: None,
      inputs_tx,
      inputs,
      sender: DirectSender::default(),
    }
  }

  /// The handle the media sinks send the own audio's direct copies through.
  pub(super) fn sender(&self) -> DirectSender {
    self.sender.clone()
  }

  /// Where the media goes now, for the connection details.
  #[cfg(feature = "call-view")]
  pub(super) fn state(&self) -> crate::direct_path::PathState {
    self.selector.state()
  }

  /// The direct connection's round trip, while it carries the media.
  #[cfg(feature = "call-view")]
  pub(super) fn rtt(&self) -> Option<Duration> {
    (self.selector.state() == crate::direct_path::PathState::Direct)
      .then(|| self.pinger.rtt())
      .flatten()
  }

  /// The user turned "Connect directly in calls" on or off during the call.
  pub(super) fn set_user_allows(&mut self, does_allow: bool, engine: &mut CallEngine, now: Instant) {
    self.config.does_user_allow = does_allow;
    self.sync(engine, now);
  }

  /// The session's secret for the SFU's `reflect`, after every join and switch.
  pub(super) fn on_session_secret(&mut self, secret: Base64Bytes, target: Option<ReflectTarget>) {
    self.maybe_session_secret = Some(secret);
    self.maybe_reflect_target = target;
    self.start_reflect();
  }

  /// The next thing that happened on the direct path; never while there is no socket.
  pub(super) async fn next_input(&mut self) -> DirectInput {
    match self.inputs.recv().await {
      Some(input) => input,
      // Invariant: this owns a sender, so the channel never closes.
      None => std::future::pending().await,
    }
  }

  /// When [`DirectPath::on_timer`] has something to do next.
  pub(super) fn next_deadline(&self) -> Option<Instant> {
    let selector = self.selector.next_deadline();
    let pinger = self
      .maybe_connection
      .as_ref()
      .and_then(|_| self.pinger.next_deadline());
    match (selector, pinger) {
      (Some(a), Some(b)) => Some(a.min(b)),
      (deadline, None) | (None, deadline) => deadline,
    }
  }

  /// Follows the call: eligibility, the epoch's key, the other device's offers. After every step
  /// of the call task.
  pub(super) fn sync(&mut self, engine: &mut CallEngine, now: Instant) {
    let offers = engine.take_direct_offers();
    let Some(pair) = self.eligible_pair(engine) else {
      self.tear_down(engine, now);
      return;
    };
    if self.maybe_pair != Some(pair) {
      self.tear_down(engine, now);
      self.maybe_pair = Some(pair);
    }
    if self.maybe_socket.is_none() {
      let may_try = self
        .maybe_socket_failed_at
        .is_none_or(|failed_at| now.saturating_duration_since(failed_at) >= SOCKET_RETRY_AFTER);
      if !may_try {
        return;
      }
      match DirectSocket::open() {
        Ok(socket) => {
          self.maybe_socket_failed_at = None;
          self.maybe_socket = Some(socket);
          self.spawn_accepting();
          self.start_reflect();
        },
        Err(error) => {
          debug!(%error, "the direct path socket could not be opened: the call stays on the SFU");
          self.maybe_socket_failed_at = Some(now);
          return;
        },
      }
    }
    self.follow_key(engine);
    if let Some(held) = self.maybe_held_offer.take() {
      self.take_offer(engine, held.as_slice(), now);
    }
    for (from, sealed) in offers {
      if from == pair.peer {
        self.take_offer(engine, sealed.as_slice(), now);
      }
    }
    self.maybe_dial(now);
  }

  /// Handles what happened on the direct path; returns a datagram of the other device's media for
  /// the engine.
  pub(super) fn on_input(
    &mut self,
    input: DirectInput,
    engine: &mut CallEngine,
    now: Instant,
  ) -> Option<(Bytes, Instant)> {
    match input {
      DirectInput::Connected {
        connection,
        is_accepted,
      } => {
        self.on_connected(connection, is_accepted, now);
        None
      },
      DirectInput::Datagram {
        connection_id,
        bytes,
        read_at,
      } => {
        if !self.is_current(connection_id) {
          return None;
        }
        self.on_datagram(bytes, read_at, now)
      },
      DirectInput::Closed { connection_id } => {
        if self.is_current(connection_id) {
          self.drop_connection();
          let change = self.selector.on_closed(now, DownReason::Closed);
          self.apply_change(change, engine);
        }
        None
      },
      DirectInput::Reflected(address) => {
        if self.maybe_reflexive != Some(address) {
          self.maybe_reflexive = Some(address);
          self.send_offer(engine);
        }
        None
      },
    }
  }

  /// Pings, the selector's deadlines, dialing rounds; on the call's timer and the path's deadlines.
  pub(super) fn on_timer(&mut self, engine: &mut CallEngine, now: Instant) {
    if self.maybe_connection.is_some() {
      let due = self.pinger.poll(now);
      if let Some(ping) = due.maybe_send {
        self.send_control(ping);
      }
      for _ in 0..due.lost {
        let change = self.selector.on_ping_lost(now);
        self.apply_change(change, engine);
      }
    }
    let change = self.selector.poll(now);
    self.apply_change(change, engine);
    // A round the selector gave up stops dialing; a path that failed to prove itself is closed.
    if !self.selector.is_dialing() {
      self.stop_dialing();
    }
    if !self.selector.is_connected() {
      self.drop_connection();
    }
    self.maybe_dial(now);
    self.publish_sender(now);
  }

  /// The network changed: the direct path is gone with it; a fresh socket, a new reflection and a
  /// new offer follow, and the next round starts at once.
  pub(super) fn on_network_changed(&mut self, engine: &mut CallEngine, now: Instant) {
    if self.maybe_socket.is_none() {
      return;
    }
    let change = self.selector.on_closed(now, DownReason::Closed);
    self.apply_change(change, engine);
    self.close_socket();
    self.maybe_reflexive = None;
    self.selector.reset_backoff();
    self.sync(engine, now);
  }

  /// Whether the SFU's copy of a datagram is taken: not the other device's audio while the direct
  /// path brings it.
  pub(super) fn admits_relay(&self, datagram: &[u8], now: Instant) -> bool {
    self.arbiter.admits_relay(datagram, now)
  }

  /// The call ends.
  pub(super) fn close(&mut self) {
    self.stop_dialing();
    self.drop_connection();
    self.close_socket();
  }

  /// The pair this device may connect now (ARCHITECTURE.md §6.7, "Who may" and "When").
  fn eligible_pair(&self, engine: &CallEngine) -> Option<DirectPair> {
    let (call_id, peer) = engine.direct_pair()?;
    // Only a group whose members this device authenticated: the key exists only then.
    engine.direct_path_epoch()?;
    let maybe_peer = self
      .config
      .standings
      .iter()
      .find(|(device, _)| *device == peer)
      .map(|(_, standing)| *standing);
    let inputs = DirectPathInputs {
      policy: &self.config.policy,
      does_user_allow: self.config.does_user_allow,
      own: self.config.own,
      maybe_peer,
      devices_in_call: engine.devices_in_call(),
    };
    eligibility(&inputs).ok()?;
    Some(DirectPair {
      call_id,
      own: engine.device_id(),
      peer,
    })
  }

  /// A new DAVE epoch brings a new key: offers start over, and this device offers again.
  fn follow_key(&mut self, engine: &mut CallEngine) {
    let Some(epoch) = engine.direct_path_epoch() else {
      return;
    };
    if self.maybe_key.as_ref().is_some_and(|held| held.epoch() == epoch) {
      return;
    }
    let Some(key) = engine.direct_path_key() else {
      return;
    };
    self.maybe_key = Some(key);
    self.counter = 0;
    self.send_offer(engine);
  }

  fn take_offer(&mut self, engine: &mut CallEngine, sealed: &[u8], now: Instant) {
    let (Some(key), Some(pair)) = (self.maybe_key.as_ref(), self.maybe_pair) else {
      return;
    };
    match offer_epoch(sealed) {
      Ok(epoch) if epoch > key.epoch() => {
        // This device hasn't processed that epoch's commit yet.
        self.maybe_held_offer = Some(Base64Bytes::new(sealed.to_vec()));
        return;
      },
      Ok(epoch) if epoch < key.epoch() => return,
      Ok(_) => {},
      Err(error) => {
        debug!(%error, "a direct path offer that isn't one");
        return;
      },
    }
    let is_first_of_epoch = !self.inbox.holds_epoch(key.epoch());
    let body = match open_offer(key, &pair, &mut self.inbox, sealed) {
      Ok(body) => body,
      Err(error) => {
        debug!(%error, "direct path offer refused");
        return;
      },
    };
    let Ok(certificate_hash) = <[u8; CERTIFICATE_HASH_LEN]>::try_from(body.certificate_hash.as_slice())
    else {
      return;
    };
    debug!(candidates = body.candidates.len(), "direct path offer taken");
    let candidates = body
      .candidates
      .iter()
      .map(|candidate| candidate.address)
      .collect();
    let has_new_certificate = self
      .maybe_peer
      .as_ref()
      .is_none_or(|peer| peer.certificate_hash != certificate_hash);
    self.maybe_peer = Some(PeerOffer {
      certificate_hash,
      candidates,
    });
    if has_new_certificate {
      self.pin_server(certificate_hash);
    }
    // A new offer: the other device may have moved, so the next round starts at once.
    self.selector.reset_backoff();
    self.maybe_dial(now);
    // The first offer of an epoch is answered, so each side has the other's even when one of the
    // offers went out before the other device could open it.
    if is_first_of_epoch {
      self.send_offer(engine);
    }
  }

  fn send_offer(&mut self, engine: &mut CallEngine) {
    let (Some(key), Some(pair), Some(socket)) = (
      self.maybe_key.as_ref(),
      self.maybe_pair,
      self.maybe_socket.as_ref(),
    ) else {
      return;
    };
    let Ok(local) = socket.endpoint.local_addr() else {
      return;
    };
    let hosts = [IPV6_ROUTE_PROBE, IPV4_ROUTE_PROBE]
      .into_iter()
      .filter_map(local_route_to);
    let candidates = own_candidates(hosts, local.port(), self.maybe_reflexive, None);
    if candidates.is_empty() {
      debug!("no address to offer: the call stays on the SFU");
      return;
    }
    self.counter += 1;
    let body = DirectOfferBody {
      counter: self.counter,
      certificate_hash: Base64Bytes::new(socket.certificate_hash.to_vec()),
      candidates,
    };
    match seal_offer(key, &pair, &body, &mut OsRng) {
      Ok(sealed) => {
        debug!(
          candidates = body.candidates.len(),
          counter = body.counter,
          "direct path offer sent"
        );
        engine.send_direct_offer(pair.peer, sealed);
      },
      Err(error) => debug!(%error, "the direct path offer could not be sealed"),
    }
  }

  /// Starts a dialing round when one may start: every candidate of the other device, one after
  /// the other ([`dial_delay`]), while this socket accepts the other side's dialing.
  fn maybe_dial(&mut self, now: Instant) {
    if self.maybe_connection.is_some() || self.maybe_dialing.is_some() || !self.selector.may_dial(now) {
      return;
    }
    let (Some(socket), Some(peer)) = (self.maybe_socket.as_ref(), self.maybe_peer.as_ref()) else {
      return;
    };
    let client = match socket.client_config(peer.certificate_hash) {
      Ok(client) => client,
      Err(error) => {
        debug!(%error, "the direct path's client configuration failed");
        return;
      },
    };
    self.selector.on_dialing(now);
    let endpoint = socket.endpoint.clone();
    let candidates = peer.candidates.clone();
    let inputs = self.inputs_tx.clone();
    self.maybe_dialing = Some(tokio::spawn(dial(endpoint, client, candidates, inputs)));
  }

  fn on_connected(&mut self, connection: quinn::Connection, is_accepted: bool, now: Instant) {
    let Some(pair) = self.maybe_pair else {
      connection.close(DIRECT_CLOSE.into(), b"not allowed");
      return;
    };
    if let Some(current) = &self.maybe_connection {
      // Both directions connected: the one the lower device ID accepted wins on both sides.
      let winner_is_accepted = accepted_connection_wins(&pair);
      if current.is_accepted == winner_is_accepted || is_accepted != winner_is_accepted {
        connection.close(DIRECT_CLOSE.into(), b"the other one");
        return;
      }
      self.drop_connection();
    }
    self.stop_dialing();
    info!(call_id = %pair.call_id, is_accepted, "direct connection up: proving it");
    let reading = tokio::spawn(read_direct_datagrams(connection.clone(), self.inputs_tx.clone()));
    self.maybe_connection = Some(DirectConnection {
      connection,
      is_accepted,
      reading,
    });
    self.selector.on_connected(now);
    self.pinger.start(now);
  }

  fn on_datagram(&mut self, bytes: Bytes, read_at: Instant, now: Instant) -> Option<(Bytes, Instant)> {
    match read_direct(&bytes)? {
      DirectDatagram::Control(DirectControl::Ping(id)) => {
        self.selector.on_heard(now);
        self.send_control(DirectControl::Pong(id));
        None
      },
      DirectDatagram::Control(DirectControl::Pong(id)) => {
        if self.pinger.on_pong(id, now) {
          self.selector.on_heard(now);
        }
        None
      },
      DirectDatagram::Media(media) => {
        self.selector.on_heard(now);
        self.arbiter.on_direct(media, now);
        Some((bytes.clone(), read_at))
      },
    }
  }

  fn apply_change(&mut self, maybe_change: Option<PathChange>, engine: &mut CallEngine) {
    let Some(change) = maybe_change else {
      return;
    };
    let call_id = self.maybe_pair.map(|pair| pair.call_id);
    match change {
      PathChange::Up => {
        info!(?call_id, rtt_ms = ?self.pinger.rtt().map(|rtt| rtt.as_millis()), "direct path carries the audio");
        engine.report_direct_path(true);
      },
      PathChange::Down(reason) => {
        info!(?call_id, ?reason, "the audio goes through the SFU again");
        engine.report_direct_path(false);
        self.arbiter.clear();
      },
    }
  }

  /// The sinks send direct copies exactly while the selector says so.
  fn publish_sender(&self, now: Instant) {
    let maybe_connection = self
      .maybe_connection
      .as_ref()
      .filter(|_| self.selector.sends_on_direct(now))
      .map(|direct| direct.connection.clone());
    *lock(&self.sender.maybe_connection) = maybe_connection;
  }

  fn send_control(&self, control: DirectControl) {
    if let Some(direct) = &self.maybe_connection {
      // A control datagram that doesn't fit or finds no room is a lost ping, which is counted.
      let _ = direct
        .connection
        .send_datagram(Bytes::copy_from_slice(&control.to_bytes()));
    }
  }

  fn is_current(&self, connection_id: usize) -> bool {
    self
      .maybe_connection
      .as_ref()
      .is_some_and(|direct| direct.connection.stable_id() == connection_id)
  }

  /// Every connection is refused but one pinned to the other device's current certificate.
  fn pin_server(&self, certificate_hash: [u8; 32]) {
    let Some(socket) = &self.maybe_socket else {
      return;
    };
    match socket.server_config(certificate_hash) {
      Ok(server) => {
        socket.endpoint.set_server_config(Some(server));
      },
      Err(error) => debug!(%error, "the direct path's server configuration failed"),
    }
  }

  fn spawn_accepting(&mut self) {
    let Some(socket) = &mut self.maybe_socket else {
      return;
    };
    socket.accepting.abort();
    socket.accepting = tokio::spawn(accept(socket.endpoint.clone(), self.inputs_tx.clone()));
  }

  fn start_reflect(&mut self) {
    let (Some(socket), Some(secret), Some(target)) = (
      self.maybe_socket.as_ref(),
      self.maybe_session_secret.clone(),
      self.maybe_reflect_target.clone(),
    ) else {
      return;
    };
    if let Some(running) = self.maybe_reflecting.take() {
      running.abort();
    }
    let endpoint = socket.endpoint.clone();
    let inputs = self.inputs_tx.clone();
    self.maybe_reflecting = Some(tokio::spawn(async move {
      match tokio::time::timeout(REFLECT_TIMEOUT, reflect(endpoint, target, secret)).await {
        Ok(Ok(address)) => {
          // The call task may be gone.
          let _ = inputs.send(DirectInput::Reflected(address));
        },
        Ok(Err(reason)) => debug!(reason, "no reflexive address: host candidates only"),
        Err(_) => debug!("the SFU's reflection timed out: host candidates only"),
      }
    }));
  }

  fn stop_dialing(&mut self) {
    if let Some(dialing) = self.maybe_dialing.take() {
      dialing.abort();
    }
  }

  fn drop_connection(&mut self) {
    if let Some(direct) = self.maybe_connection.take() {
      direct.reading.abort();
      direct.connection.close(DIRECT_CLOSE.into(), b"closed");
    }
    self.pinger.stop();
    *lock(&self.sender.maybe_connection) = None;
  }

  fn close_socket(&mut self) {
    self.stop_dialing();
    self.drop_connection();
    if let Some(reflecting) = self.maybe_reflecting.take() {
      reflecting.abort();
    }
    if let Some(socket) = self.maybe_socket.take() {
      socket.accepting.abort();
      socket.endpoint.close(DIRECT_CLOSE.into(), b"closed");
    }
  }

  /// No direct path now: back to the SFU, and everything of the old pair goes.
  fn tear_down(&mut self, engine: &mut CallEngine, now: Instant) {
    if self.maybe_pair.is_none() && self.maybe_socket.is_none() {
      return;
    }
    let change = self.selector.on_closed(now, DownReason::NotAllowed);
    self.apply_change(change, engine);
    engine.report_direct_path(false);
    self.close_socket();
    self.maybe_pair = None;
    self.maybe_key = None;
    self.maybe_peer = None;
    self.maybe_held_offer = None;
    self.maybe_reflexive = None;
    self.inbox = OfferInbox::default();
    self.selector = PathSelector::new();
    self.arbiter.clear();
  }
}

impl Drop for DirectPath {
  fn drop(&mut self) {
    self.close();
  }
}

impl DirectSender {
  /// Sends the direct copy of an own datagram, if it is audio and the direct path carries it now.
  pub(super) fn send(&self, datagram: &[u8]) {
    let maybe_connection = lock(&self.maybe_connection).clone();
    let Some(connection) = maybe_connection else {
      return;
    };
    if let Some(copy) = direct_copy(datagram) {
      // Unreliable by design, as on the SFU path; the SFU's copy went out as well.
      let _ = connection.send_datagram(Bytes::from(copy));
    }
  }
}

impl DirectSocket {
  /// A dual-stack socket on a free port with an endpoint on it, and a fresh certificate. It accepts
  /// nothing until an offer names the other device's certificate.
  fn open() -> Result<Self, DirectSetupError> {
    let socket = bind_wildcard(true)?;
    let endpoint = quinn::Endpoint::new(EndpointConfig::default(), None, socket, Arc::new(TokioRuntime))?;
    let identity = Identity::self_signed([DIRECT_SERVER_NAME])
      .map_err(|error| DirectSetupError::Certificate(error.to_string()))?;
    let certificate_hash = identity
      .certificate_chain()
      .as_slice()
      .first()
      .map(|certificate| *certificate.hash().as_ref())
      .ok_or_else(|| DirectSetupError::Certificate("no certificate".into()))?;
    Ok(Self {
      endpoint,
      identity,
      certificate_hash,
      accepting: tokio::spawn(async {}),
    })
  }

  fn server_config(&self, peer_hash: [u8; 32]) -> Result<quinn::ServerConfig, DirectSetupError> {
    let (chain, key) = self.tls_identity();
    let mut tls = rustls::ServerConfig::builder_with_provider(provider())
      .with_protocol_versions(&[&rustls::version::TLS13])?
      .with_client_cert_verifier(Arc::new(PinnedPeer::new(peer_hash)))
      .with_single_cert(chain, key)?;
    tls.alpn_protocols = vec![DIRECT_ALPN.to_vec()];
    let crypto =
      QuicServerConfig::try_from(tls).map_err(|error| DirectSetupError::Quic(error.to_string()))?;
    let mut server = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    server.transport_config(Arc::new(direct_transport()?));
    Ok(server)
  }

  fn client_config(&self, peer_hash: [u8; 32]) -> Result<quinn::ClientConfig, DirectSetupError> {
    let (chain, key) = self.tls_identity();
    let mut tls = rustls::ClientConfig::builder_with_provider(provider())
      .with_protocol_versions(&[&rustls::version::TLS13])?
      .dangerous()
      .with_custom_certificate_verifier(Arc::new(PinnedPeer::new(peer_hash)))
      .with_client_auth_cert(chain, key)?;
    tls.alpn_protocols = vec![DIRECT_ALPN.to_vec()];
    let crypto =
      QuicClientConfig::try_from(tls).map_err(|error| DirectSetupError::Quic(error.to_string()))?;
    let mut client = quinn::ClientConfig::new(Arc::new(crypto));
    client.transport_config(Arc::new(direct_transport()?));
    Ok(client)
  }

  fn tls_identity(&self) -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
    let chain = self
      .identity
      .certificate_chain()
      .as_slice()
      .iter()
      .map(|certificate| CertificateDer::from(certificate.der().to_vec()))
      .collect();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
      self.identity.private_key().secret_der().to_vec(),
    ));
    (chain, key)
  }
}

impl PinnedPeer {
  fn new(certificate_hash: [u8; 32]) -> Self {
    Self {
      certificate_hash,
      algorithms: provider().signature_verification_algorithms,
    }
  }

  fn check(&self, end_entity: &CertificateDer<'_>) -> Result<(), rustls::Error> {
    let certificate = Certificate::from_der(end_entity.to_vec())
      .map_err(|_| rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding))?;
    if certificate.hash() == Sha256Digest::new(self.certificate_hash) {
      Ok(())
    } else {
      Err(rustls::Error::InvalidCertificate(
        rustls::CertificateError::UnknownIssuer,
      ))
    }
  }
}

impl ServerCertVerifier for PinnedPeer {
  fn verify_server_cert(
    &self,
    end_entity: &CertificateDer<'_>,
    _intermediates: &[CertificateDer<'_>],
    _server_name: &ServerName<'_>,
    _ocsp_response: &[u8],
    _now: UnixTime,
  ) -> Result<ServerCertVerified, rustls::Error> {
    self.check(end_entity).map(|()| ServerCertVerified::assertion())
  }

  fn verify_tls12_signature(
    &self,
    _message: &[u8],
    _cert: &CertificateDer<'_>,
    _dss: &DigitallySignedStruct,
  ) -> Result<HandshakeSignatureValid, rustls::Error> {
    // TLS 1.3 only (the configurations name no other version).
    Err(rustls::Error::PeerIncompatible(
      rustls::PeerIncompatible::Tls12NotOffered,
    ))
  }

  fn verify_tls13_signature(
    &self,
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &DigitallySignedStruct,
  ) -> Result<HandshakeSignatureValid, rustls::Error> {
    rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
  }

  fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
    self.algorithms.supported_schemes()
  }
}

impl ClientCertVerifier for PinnedPeer {
  fn root_hint_subjects(&self) -> &[DistinguishedName] {
    &[]
  }

  fn verify_client_cert(
    &self,
    end_entity: &CertificateDer<'_>,
    _intermediates: &[CertificateDer<'_>],
    _now: UnixTime,
  ) -> Result<ClientCertVerified, rustls::Error> {
    self.check(end_entity).map(|()| ClientCertVerified::assertion())
  }

  fn verify_tls12_signature(
    &self,
    _message: &[u8],
    _cert: &CertificateDer<'_>,
    _dss: &DigitallySignedStruct,
  ) -> Result<HandshakeSignatureValid, rustls::Error> {
    Err(rustls::Error::PeerIncompatible(
      rustls::PeerIncompatible::Tls12NotOffered,
    ))
  }

  fn verify_tls13_signature(
    &self,
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &DigitallySignedStruct,
  ) -> Result<HandshakeSignatureValid, rustls::Error> {
    rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
  }

  fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
    self.algorithms.supported_schemes()
  }
}

/// The TLS provider of the direct connection: the one wtransport's QUIC uses (ring).
fn provider() -> Arc<CryptoProvider> {
  Arc::new(rustls::crypto::ring::default_provider())
}

/// QUIC's settings of a direct connection: datagrams, no keep-alives (the pings are its sign of
/// life), and an idle timeout the pings never let run out on a working path.
fn direct_transport() -> Result<TransportConfig, DirectSetupError> {
  let idle =
    IdleTimeout::try_from(DIRECT_IDLE_TIMEOUT).map_err(|error| DirectSetupError::Quic(error.to_string()))?;
  let mut transport = TransportConfig::default();
  transport.max_idle_timeout(Some(idle)).keep_alive_interval(None);
  Ok(transport)
}

/// Dials every candidate, [`dial_delay`] apart, until one connects or the round ends.
async fn dial(
  endpoint: quinn::Endpoint,
  client: quinn::ClientConfig,
  candidates: Vec<SocketAddr>,
  inputs: mpsc::UnboundedSender<DirectInput>,
) {
  let (connected_tx, mut connected_rx) = mpsc::unbounded_channel::<quinn::Connection>();
  let mut attempts = Vec::with_capacity(candidates.len());
  for (index, address) in candidates.into_iter().enumerate() {
    let (endpoint, client, connected) = (endpoint.clone(), client.clone(), connected_tx.clone());
    attempts.push(tokio::spawn(async move {
      tokio::time::sleep(dial_delay(index)).await;
      let Ok(connecting) = endpoint.connect_with(client, address, DIRECT_SERVER_NAME) else {
        return;
      };
      if let Ok(connection) = connecting.await {
        let _ = connected.send(connection);
      }
    }));
  }
  drop(connected_tx);
  if let Ok(Some(connection)) = tokio::time::timeout(DIAL_TIMEOUT, connected_rx.recv()).await {
    // The call task may be gone.
    let _ = inputs.send(DirectInput::Connected {
      connection,
      is_accepted: false,
    });
  }
  for attempt in attempts {
    attempt.abort();
  }
}

/// Accepts the other device's dialing for as long as the socket lives. The server configuration
/// refuses every certificate but the pinned one.
async fn accept(endpoint: quinn::Endpoint, inputs: mpsc::UnboundedSender<DirectInput>) {
  while let Some(incoming) = endpoint.accept().await {
    let inputs = inputs.clone();
    tokio::spawn(async move {
      match incoming.await {
        Ok(connection) => {
          let _ = inputs.send(DirectInput::Connected {
            connection,
            is_accepted: true,
          });
        },
        Err(error) => debug!(%error, "an incoming direct connection failed"),
      }
    });
  }
}

async fn read_direct_datagrams(connection: quinn::Connection, inputs: mpsc::UnboundedSender<DirectInput>) {
  let connection_id = connection.stable_id();
  loop {
    match connection.read_datagram().await {
      Ok(bytes) => {
        let input = DirectInput::Datagram {
          connection_id,
          bytes,
          read_at: Instant::now(),
        };
        if inputs.send(input).is_err() {
          return;
        }
      },
      Err(error) => {
        debug!(%error, "the direct connection closed");
        let _ = inputs.send(DirectInput::Closed { connection_id });
        return;
      },
    }
  }
}

/// Asks the SFU from the direct path socket where its packets come from (ARCHITECTURE.md §6.7,
/// "Endpoint discovery"): a WebTransport connection on the same QUIC endpoint, `reflect` with the
/// session's secret, the SFU's `reflected`, and the close.
async fn reflect(
  endpoint: quinn::Endpoint,
  target: ReflectTarget,
  secret: Base64Bytes,
) -> Result<SocketAddr, String> {
  let builder = ClientConfig::builder().with_bind_default();
  let builder = if target.certificate_hashes.is_empty() {
    with_trusted_roots(builder)
  } else {
    builder.with_server_certificate_hashes(target.certificate_hashes.iter().copied().map(Sha256Digest::new))
  };
  let config = builder.dns_resolver(PinnedResolver(target.address)).build();
  let client: Endpoint<Client> = Endpoint::client_on_endpoint(config, endpoint);
  let connection = client
    .connect(&target.sfu_url)
    .await
    .map_err(|error| error.to_string())?;
  let (mut send, mut recv) = connection
    .open_bi()
    .await
    .map_err(|error| error.to_string())?
    .await
    .map_err(|error| error.to_string())?;
  write_framed(&mut send, &CallClientMessage::Reflect { secret })
    .await
    .ok_or("the reflect request could not be written")?;
  let answer = read_framed(&mut recv).await;
  connection.close(DIRECT_CLOSE.into(), b"reflected");
  match answer {
    Some(CallServerMessage::Reflected { address }) => Ok(address),
    Some(_) => Err("the SFU refused the reflection".into()),
    None => Err("no answer to the reflection".into()),
  }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
  // Nothing spans a panic point while it is held.
  mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
