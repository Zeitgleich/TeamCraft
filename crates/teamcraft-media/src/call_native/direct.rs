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
//! Behind NATs (ARCHITECTURE.md §6.7, "Hard NATs" and "Endpoint discovery"): the socket asks its
//! router for a port mapping (`crate::port_mapping`, desktop builds), reflects on the SFU's two ports
//! to learn whether its NAT maps every destination to a new port, and when exactly one of the two
//! devices is behind such a hard NAT, the two find a way in by birthday probing: the hard side opens
//! [`BIRTHDAY_SOCKETS`] sockets that punch toward the other device, the other side probes random
//! ports of the hard side's address from its direct path socket, and the first probe that arrives
//! tells the hard side which socket to move its endpoint to and whom to dial.
//!
//! The audio (the full voice and the screen's sound) keeps going to the SFU as well: its speaker
//! selection and voice guard watch it, and the SFU forwards it again the moment either device
//! reports its direct path down. The video goes on one layer per track, the one the receiver takes
//! (its `wants`) and the direct connection carries (`DirectVideoBudget`), through a send queue so it
//! never holds the voice up; while both devices say their path carries the video, the SFU forwards
//! none of it between them and the publisher sends it the SFU no more. Addresses are never logged.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use rand_core::OsRng;
use rand_core::RngCore;
use teamcraft_crypto::direct_path::DirectPathKey;
use teamcraft_proto::{
  Base64Bytes, CallClientMessage, CallPeerStanding, CallServerMessage, DIRECT_PROBE_TOKEN_BYTES, DeviceId,
  DirectMediaPolicy, DirectNatKind, DirectOfferBody, DirectPathMedia, MediaStreamId, VideoTrackKind,
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
use crate::call_engine::{CallEngine, RelayedDirectOffer};
use crate::direct_path::{
  BIRTHDAY_LINGER, BIRTHDAY_PROBE_INTERVAL, BIRTHDAY_PUNCHES, BIRTHDAY_SOCKETS, BIRTHDAY_TIMEOUT,
  BirthdayBudget, BirthdayRole, CERTIFICATE_HASH_LEN, DIAL_TIMEOUT, DIRECT_KEYFRAME_INTERVAL, DirectControl,
  DirectDatagram, DirectPair, DirectPathInputs, DirectVideoBudget, DirectVideoTrack, DownReason, OfferInbox,
  OwnNat, PathChange, PathReading, PathSelector, PathState, Pinger, RelayArbiter, WANTS_INTERVAL,
  accepted_connection_wins, birthday_ports, birthday_probe, birthday_role, dial_delay, direct_copy,
  direct_video_layers, eligibility, is_birthday_probe, nat_kind, offer_epoch, open_offer, own_candidates,
  read_direct, seal_offer,
};
#[cfg(feature = "port-mapping")]
use crate::port_mapping::PortMapper;
use crate::quic_send::QuicSender;
use crate::send_queue::SendQueueConfig;
use crate::sfu_address::{PinnedResolver, with_port};

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
/// The headroom of descriptors birthday probing leaves when the system runs short of them: the
/// last sockets it opened are closed again.
const BIRTHDAY_SPARE_SOCKETS: usize = 32;
/// The largest datagram a birthday socket reads: probes are shorter, longer ones are no probe.
const BIRTHDAY_READ_BYTES: usize = 64;
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
  maybe_held_offer: Option<RelayedDirectOffer>,
  maybe_peer: Option<PeerOffer>,
  maybe_reflexive: Option<SocketAddr>,
  /// How this device's NAT maps the socket, once both of the SFU's ports reflected it.
  maybe_nat: Option<DirectNatKind>,
  /// The port the router mapped to the socket, while it holds.
  maybe_mapped: Option<SocketAddr>,
  /// The birthday round running now, and when the next may start.
  maybe_birthday: Option<JoinHandle<()>>,
  birthday: BirthdayBudget,
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
  /// Where the own audio's direct copies and video go: shared with the sinks that send the media.
  sender: DirectSender,
  /// The own video on the direct path (#127 part 4).
  video: DirectVideo,
}

/// Sends the own audio's copies and video on the direct connection while it carries them.
#[derive(Clone, Default)]
pub(super) struct DirectSender {
  maybe_link: Arc<Mutex<Option<DirectLink>>>,
}

/// The direct connection as the media sinks send on it.
#[derive(Clone)]
struct DirectLink {
  connection: quinn::Connection,
  video: Arc<QuicSender>,
}

/// The video's side of the direct path: what the other device takes of the own tracks, the layer
/// each goes on, and what this device takes of the other's.
#[derive(Default)]
struct DirectVideo {
  budget: DirectVideoBudget,
  /// The other device's tracks it takes, as its last `wants` said.
  peer_wants: Vec<(MediaStreamId, u8)>,
  /// The layer each own track goes on over the direct path.
  layers: HashMap<MediaStreamId, u8>,
  /// What this device last told the other one it takes, and when.
  maybe_wants_sent: Option<(Vec<(MediaStreamId, u8)>, Instant)>,
  /// When a keyframe of each own track was last made for the other device's ask.
  keyframes_made_at: HashMap<MediaStreamId, Instant>,
}

/// Where the SFU's `reflect` goes: the session connection's SFU.
#[derive(Clone)]
pub(super) struct ReflectTarget {
  pub(super) sfu_url: String,
  pub(super) address: SocketAddr,
  /// The SFU's second port that only reflects (`audio_socket`'s `reflect_port`).
  pub(super) maybe_second: Option<SocketAddr>,
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
  /// The SFU's reflections: its main port's, and its second port's where it has one.
  Reflected {
    first: SocketAddr,
    maybe_second: Option<SocketAddr>,
  },
  /// The router's mapping of the socket changed (`None`: none any more).
  #[cfg_attr(
    not(feature = "port-mapping"),
    expect(dead_code, reason = "only the port mapper sends it, a `port-mapping` build")
  )]
  Mapped(Option<SocketAddr>),
  /// A birthday probe reached one of the hard side's sockets: the endpoint moves there and dials
  /// `peer`, where the probe came from.
  BirthdayHit {
    socket: UdpSocket,
    peer: SocketAddr,
  },
  /// A birthday round ended without a way in.
  BirthdayEnded,
}

/// The direct path socket: one QUIC endpoint that dials and accepts, and its certificate.
struct DirectSocket {
  endpoint: quinn::Endpoint,
  /// The same socket, for the birthday probes, which aren't QUIC.
  raw: UdpSocket,
  identity: Identity,
  certificate_hash: [u8; 32],
  /// What the other device puts into its birthday probes to this socket, sealed in the offer.
  probe_token: [u8; DIRECT_PROBE_TOKEN_BYTES],
  /// The endpoint moved to a birthday socket: its offer's addresses are stale.
  is_rebound: bool,
  accepting: JoinHandle<()>,
  #[cfg(feature = "port-mapping")]
  maybe_mapper: Option<PortMapper>,
}

/// The other device's offer, as this device dials it.
struct PeerOffer {
  certificate_hash: [u8; 32],
  candidates: Vec<SocketAddr>,
  body: DirectOfferBody,
  /// Where the SFU saw the other device's call connection come from, with this offer.
  maybe_observed_ip: Option<IpAddr>,
}

struct DirectConnection {
  connection: quinn::Connection,
  is_accepted: bool,
  reading: JoinHandle<()>,
  /// The own video's queue on it.
  video: Arc<QuicSender>,
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
      maybe_nat: None,
      maybe_mapped: None,
      maybe_birthday: None,
      birthday: BirthdayBudget::default(),
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
      video: DirectVideo::default(),
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
    let birthday = self
      .birthday
      .next_start()
      .filter(|_| self.maybe_birthday.is_none() && self.maybe_connection.is_none());
    [selector, pinger, birthday].into_iter().flatten().min()
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
          self.start_mapping();
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
      self.take_offer(engine, held, now);
    }
    for offer in offers {
      if offer.from == pair.peer {
        self.take_offer(engine, offer, now);
      }
    }
    self.maybe_dial(now);
    self.forward_keyframe_asks(engine, now);
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
        self.on_datagram(bytes, read_at, engine, now)
      },
      DirectInput::Closed { connection_id } => {
        if self.is_current(connection_id) {
          self.drop_connection();
          let change = self.selector.on_closed(now, DownReason::Closed);
          self.apply_change(change, engine);
          if self.maybe_socket.as_ref().is_some_and(|socket| socket.is_rebound) {
            // The birthday socket's way in is gone with its connection, and the offer names the
            // socket before it: a fresh socket, reflections and offer.
            self.renew_socket(engine, now);
          }
        }
        None
      },
      DirectInput::Reflected { first, maybe_second } => {
        let maybe_nat = maybe_second.map(|second| nat_kind(first, second));
        if self.maybe_reflexive != Some(first) || self.maybe_nat != maybe_nat {
          debug!(nat = ?maybe_nat, "direct path socket reflected");
          self.maybe_reflexive = Some(first);
          self.maybe_nat = maybe_nat;
          self.send_offer(engine);
          self.maybe_birthday(now);
        }
        None
      },
      DirectInput::Mapped(maybe_mapped) => {
        if self.maybe_mapped != maybe_mapped {
          self.maybe_mapped = maybe_mapped;
          self.send_offer(engine);
        }
        None
      },
      DirectInput::BirthdayHit { socket, peer } => {
        self.on_birthday_hit(socket, peer, now);
        None
      },
      DirectInput::BirthdayEnded => {
        self.maybe_birthday = None;
        self.birthday.on_ended(now);
        debug!(
          failed = self.birthday.failed(),
          "a birthday round found no way in"
        );
        None
      },
    }
  }

  /// Pings, the selector's deadlines, dialing rounds; on the call's timer and the path's deadlines.
  pub(super) fn on_timer(&mut self, engine: &mut CallEngine, now: Instant) {
    if self.maybe_connection.is_some() {
      let due = self.pinger.poll(now);
      if let Some(ping) = due.maybe_send {
        self.send_control(&ping);
      }
      for _ in 0..due.lost {
        let change = self.selector.on_ping_lost(now);
        self.apply_change(change, engine);
      }
    }
    let change = self.selector.poll(now);
    self.apply_change(change, engine);
    // A round the selector gave up stops dialing, unless its connection may still win over the
    // one there is; a path that failed to prove itself is closed.
    if !self.selector.is_dialing() && !self.may_be_replaced() {
      self.stop_dialing();
    }
    if !self.selector.is_connected() {
      self.drop_connection();
    }
    self.maybe_dial(now);
    self.maybe_birthday(now);
    self.publish_sender(now);
    self.read_path(now);
    self.follow_video(engine, now);
  }

  /// The network changed: the direct path is gone with it; a fresh socket, a new reflection and a
  /// new offer follow, and the next round starts at once.
  pub(super) fn on_network_changed(&mut self, engine: &mut CallEngine, now: Instant) {
    if self.maybe_socket.is_none() {
      return;
    }
    let change = self.selector.on_closed(now, DownReason::Closed);
    self.apply_change(change, engine);
    self.selector.reset_backoff();
    // A new network may have another NAT: birthday probing may start again at once.
    self.birthday.on_network_changed();
    self.renew_socket(engine, now);
  }

  /// A fresh socket, with fresh reflections, mapping and offer.
  fn renew_socket(&mut self, engine: &mut CallEngine, now: Instant) {
    self.close_socket();
    self.maybe_reflexive = None;
    self.maybe_nat = None;
    self.maybe_mapped = None;
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

  fn take_offer(&mut self, engine: &mut CallEngine, offer: RelayedDirectOffer, now: Instant) {
    let (Some(key), Some(pair)) = (self.maybe_key.as_ref(), self.maybe_pair) else {
      return;
    };
    let sealed = offer.sealed.as_slice();
    match offer_epoch(sealed) {
      Ok(epoch) if epoch > key.epoch() => {
        // This device hasn't processed that epoch's commit yet.
        self.maybe_held_offer = Some(offer);
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
    let candidates: Vec<SocketAddr> = body
      .candidates
      .iter()
      .map(|candidate| candidate.address)
      .collect();
    let has_new_candidates = self
      .maybe_peer
      .as_ref()
      .is_none_or(|peer| peer.candidates != candidates);
    let has_new_certificate = self
      .maybe_peer
      .as_ref()
      .is_none_or(|peer| peer.certificate_hash != certificate_hash);
    self.maybe_peer = Some(PeerOffer {
      certificate_hash,
      candidates,
      body,
      maybe_observed_ip: offer.maybe_observed_ip,
    });
    if has_new_certificate {
      self.pin_server(certificate_hash);
    }
    // A new offer: the other device may have moved, so the next round starts at once; one that
    // runs with the old addresses starts over with the new ones (its reflexive address often
    // comes a moment after its first offer).
    self.selector.reset_backoff();
    if has_new_candidates && self.maybe_dialing.is_some() {
      self.stop_dialing();
      self.spawn_dial(now);
    }
    self.maybe_dial(now);
    // Birthday probing keeps its own backoff: the other device's offers can't make it run more.
    self.maybe_birthday(now);
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
    let candidates = own_candidates(hosts, local.port(), self.maybe_reflexive, self.maybe_mapped);
    if candidates.is_empty() {
      debug!("no address to offer: the call stays on the SFU");
      return;
    }
    self.counter += 1;
    let body = DirectOfferBody {
      counter: self.counter,
      certificate_hash: Base64Bytes::new(socket.certificate_hash.to_vec()),
      candidates,
      maybe_nat: self.maybe_nat.clone(),
      maybe_probe_token: Some(Base64Bytes::new(socket.probe_token.to_vec())),
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
    self.spawn_dial(now);
  }

  /// Dials every candidate of the other device's offer in a round of its own.
  fn spawn_dial(&mut self, now: Instant) {
    if self.maybe_connection.is_some() {
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
    let is_winning_kind = is_accepted == accepted_connection_wins(&pair);
    if is_winning_kind {
      self.stop_dialing();
    }
    // Else the own dialing goes on: when it connects as well, the other device takes that one (the
    // lower device ID's accepted one wins on both sides), so it must not be dropped here.
    self.stop_birthday();
    self.birthday.on_connected();
    info!(call_id = %pair.call_id, is_accepted, "direct connection up: proving it");
    let reading = tokio::spawn(read_direct_datagrams(connection.clone(), self.inputs_tx.clone()));
    // One layer per track: a keyframe makes what waits of the track obsolete.
    let video = Arc::new(QuicSender::spawn_direct(
      connection.clone(),
      SendQueueConfig::downlink(),
    ));
    // A new connection is measured afresh; the layers the engine has stay until chosen again.
    self.video.budget = DirectVideoBudget::default();
    self.video.maybe_wants_sent = None;
    self.maybe_connection = Some(DirectConnection {
      connection,
      is_accepted,
      reading,
      video,
    });
    self.selector.on_connected(now);
    self.pinger.start(now);
  }

  /// Whether the connection there is may still give way to the own dialing's: it is of the kind
  /// that loses when both directions connect, and the dialing runs.
  fn may_be_replaced(&self) -> bool {
    let (Some(pair), Some(current)) = (self.maybe_pair, self.maybe_connection.as_ref()) else {
      return false;
    };
    current.is_accepted != accepted_connection_wins(&pair)
      && self
        .maybe_dialing
        .as_ref()
        .is_some_and(|dialing| !dialing.is_finished())
  }

  fn on_datagram(
    &mut self,
    bytes: Bytes,
    read_at: Instant,
    engine: &mut CallEngine,
    now: Instant,
  ) -> Option<(Bytes, Instant)> {
    match read_direct(&bytes)? {
      DirectDatagram::Control(DirectControl::Ping(id)) => {
        self.selector.on_heard(now);
        self.send_control(&DirectControl::Pong(id));
        None
      },
      DirectDatagram::Control(DirectControl::Pong(id)) => {
        if self.pinger.on_pong(id, now) {
          self.selector.on_heard(now);
        }
        None
      },
      DirectDatagram::Control(DirectControl::Wants(wants)) => {
        self.selector.on_heard(now);
        if self.video.peer_wants != wants {
          self.video.peer_wants = wants;
          self.follow_video(engine, now);
        }
        None
      },
      DirectDatagram::Control(DirectControl::Keyframe(stream_id)) => {
        self.selector.on_heard(now);
        self.make_keyframe(stream_id, engine, now);
        None
      },
      DirectDatagram::Control(DirectControl::Nack {
        stream_id,
        layer,
        sequence_numbers,
      }) => {
        self.selector.on_heard(now);
        // Only the layer the direct path carries, and within the direct budget's share for repair.
        let budget_bps = self.video.budget.budget_bps();
        engine.resend_direct_video(stream_id, layer, &sequence_numbers, budget_bps, now);
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
        info!(?call_id, rtt_ms = ?self.pinger.rtt().map(|rtt| rtt.as_millis()), "direct path carries the media");
        engine.report_direct_path(Some(DirectPathMedia::AudioAndVideo));
      },
      PathChange::Down(reason) => {
        info!(?call_id, ?reason, "the media goes through the SFU again");
        engine.report_direct_path(None);
        self.stop_video(engine);
        self.arbiter.clear();
      },
    }
  }

  /// The sinks send direct copies exactly while the selector says so.
  fn publish_sender(&self, now: Instant) {
    let maybe_link = self
      .maybe_connection
      .as_ref()
      .filter(|_| self.selector.sends_on_direct(now))
      .map(|direct| DirectLink {
        connection: direct.connection.clone(),
        video: Arc::clone(&direct.video),
      });
    *lock(&self.sender.maybe_link) = maybe_link;
  }

  fn send_control(&self, control: &DirectControl) {
    if let Some(direct) = &self.maybe_connection {
      // A control datagram that doesn't fit or finds no room is lost like any other: a ping is
      // counted, `wants` go again, a keyframe is asked for again.
      let _ = direct.connection.send_datagram(Bytes::from(control.to_bytes()));
    }
  }

  /// Feeds the direct video budget what the connection did since the last look.
  fn read_path(&mut self, now: Instant) {
    let Some(direct) = self.maybe_connection.as_ref() else {
      return;
    };
    let stats = direct.connection.stats();
    self.video.budget.on_reading(PathReading {
      at: now,
      sent_packets: stats.path.sent_packets,
      lost_packets: stats.path.lost_packets,
      sent_bytes: stats.udp_tx.bytes,
      rtt: stats.path.rtt,
      queue_drops: direct.video.dropped_packets(),
    });
  }

  /// The keyframes and missing packets this device asked for of tracks the direct path brings go
  /// to the other device too: the SFU never had them. After every step of the call task (nothing to
  /// do but take empty lists, mostly).
  fn forward_keyframe_asks(&mut self, engine: &mut CallEngine, now: Instant) {
    for stream_id in engine.take_direct_keyframe_asks() {
      if self.maybe_connection.is_some() && self.arbiter.is_direct(stream_id, now) {
        self.send_control(&DirectControl::Keyframe(stream_id));
      }
    }
    for nack in engine.take_direct_nacks() {
      if self.maybe_connection.is_some() && self.arbiter.is_direct(nack.stream_id, now) {
        self.send_control(&DirectControl::Nack {
          stream_id: nack.stream_id,
          layer: nack.layer,
          sequence_numbers: nack.sequence_numbers,
        });
      }
    }
  }

  /// The video while the direct path carries the media: which own track goes on which layer, and
  /// what this device takes of the other's (told again now and then). On the path's timer, every
  /// [`crate::direct_path::PING_INTERVAL`] while connected, and when the other device's `wants`
  /// change.
  fn follow_video(&mut self, engine: &mut CallEngine, now: Instant) {
    let Some(pair) = self.maybe_pair else {
      return;
    };
    if self.maybe_connection.is_none() || self.selector.state() != PathState::Direct {
      self.stop_video(engine);
      return;
    }
    let wants = engine.video_wants_of(pair.peer);
    let is_due = self
      .video
      .maybe_wants_sent
      .as_ref()
      .is_none_or(|(sent, at)| *sent != wants || now.saturating_duration_since(*at) >= WANTS_INTERVAL);
    if is_due {
      self.send_control(&DirectControl::Wants(wants.clone()));
      self.video.maybe_wants_sent = Some((wants, now));
    }
    self.choose_layers(engine);
  }

  /// Puts each own track the other device takes on the best layer the budget fits.
  fn choose_layers(&mut self, engine: &mut CallEngine) {
    let tracks: Vec<DirectVideoTrack> = engine
      .own_video_tracks()
      .into_iter()
      .map(|track| DirectVideoTrack {
        stream_id: track.stream_id,
        is_screen: track.kind == VideoTrackKind::Screen,
        layers: track.layers.clone(),
        maybe_want: self
          .video
          .peer_wants
          .iter()
          .find(|(stream_id, _)| *stream_id == track.stream_id)
          .map(|(_, max_layer)| *max_layer),
        maybe_current: self.video.layers.get(&track.stream_id).copied(),
      })
      .collect();
    let chosen = direct_video_layers(self.video.budget.budget_bps(), &tracks);
    // Tracks that are gone go off the direct path too.
    self
      .video
      .layers
      .retain(|stream_id, _| chosen.iter().any(|(chosen_id, _)| chosen_id == stream_id));
    for (stream_id, maybe_layer) in chosen {
      if self.video.layers.get(&stream_id).copied() == maybe_layer {
        continue;
      }
      debug!(stream_id = stream_id.0, layer = ?maybe_layer, budget_bps = self.video.budget.budget_bps(), "direct video layer");
      match maybe_layer {
        Some(layer) => self.video.layers.insert(stream_id, layer),
        None => self.video.layers.remove(&stream_id),
      };
      engine.set_direct_video_layer(stream_id, maybe_layer);
    }
  }

  /// No own video on the direct path any more: the SFU's layers alone are encoded again.
  fn stop_video(&mut self, engine: &mut CallEngine) {
    for (stream_id, _) in self.video.layers.drain() {
      engine.set_direct_video_layer(stream_id, None);
    }
    self.video.maybe_wants_sent = None;
  }

  /// The other device asked for a keyframe of an own track: made on the direct layer, at most every
  /// [`DIRECT_KEYFRAME_INTERVAL`] per track.
  fn make_keyframe(&mut self, stream_id: MediaStreamId, engine: &mut CallEngine, now: Instant) {
    if !self.video.layers.contains_key(&stream_id) {
      return;
    }
    let is_due = self
      .video
      .keyframes_made_at
      .get(&stream_id)
      .is_none_or(|at| now.saturating_duration_since(*at) >= DIRECT_KEYFRAME_INTERVAL);
    if is_due {
      self.video.keyframes_made_at.insert(stream_id, now);
      engine.request_direct_keyframe(stream_id);
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
      let maybe_second = target.maybe_second;
      let first = timed_reflect(endpoint.clone(), target.clone(), target.address, secret.clone());
      let second = async {
        match maybe_second {
          Some(address) => timed_reflect(endpoint, target, address, secret).await.ok(),
          None => None,
        }
      };
      let (first, maybe_second) = tokio::join!(first, second);
      match first {
        Ok(first) => {
          // The call task may be gone.
          let _ = inputs.send(DirectInput::Reflected { first, maybe_second });
        },
        Err(reason) => debug!(reason, "no reflexive address: host candidates only"),
      }
    }));
  }

  /// Asks the router for a mapping of the socket's port (desktop builds).
  fn start_mapping(&mut self) {
    #[cfg(feature = "port-mapping")]
    {
      let Some(socket) = &mut self.maybe_socket else {
        return;
      };
      let Ok(local) = socket.endpoint.local_addr() else {
        return;
      };
      let inputs = self.inputs_tx.clone();
      socket.maybe_mapper = Some(PortMapper::start(local.port(), move |maybe_mapped| {
        // The call task may be gone.
        let _ = inputs.send(DirectInput::Mapped(maybe_mapped.map(SocketAddr::V4)));
      }));
    }
  }

  /// Starts a birthday round when this device has a part in one (one of the two behind a hard
  /// NAT), none runs, no connection is there, and the backoff after failed rounds has passed.
  fn maybe_birthday(&mut self, now: Instant) {
    if self.maybe_birthday.is_some() || self.maybe_connection.is_some() || !self.birthday.may_start(now) {
      return;
    }
    let (Some(socket), Some(peer)) = (self.maybe_socket.as_ref(), self.maybe_peer.as_ref()) else {
      return;
    };
    if socket.is_rebound {
      return;
    }
    let own = OwnNat {
      maybe_kind: self.maybe_nat.clone(),
      has_mapped: self.maybe_mapped.is_some(),
      token: socket.probe_token,
    };
    let Some(role) = birthday_role(&own, &peer.body, peer.maybe_observed_ip) else {
      return;
    };
    self.birthday.on_started();
    let inputs = self.inputs_tx.clone();
    match role {
      BirthdayRole::Open { peer, own_token } => {
        info!(
          role = "open",
          "birthday round: the other device probes this device's sockets"
        );
        self.maybe_birthday = Some(tokio::spawn(open_birthday(peer, own_token, inputs)));
      },
      BirthdayRole::Probe { peer_ip, peer_token } => {
        let raw = match socket.raw.try_clone() {
          Ok(raw) => raw,
          Err(error) => {
            debug!(%error, "no handle on the direct path socket for birthday probes");
            return;
          },
        };
        let is_dual_stack = socket.endpoint.local_addr().is_ok_and(|local| local.is_ipv6());
        info!(role = "probe", "birthday round: probing the other device's NAT");
        let ports = birthday_ports(&mut OsRng);
        self.maybe_birthday = Some(tokio::spawn(probe_birthday(
          raw,
          is_dual_stack,
          peer_ip,
          peer_token,
          ports,
          inputs,
        )));
      },
    }
  }

  /// A probe found one of this device's birthday sockets: the endpoint moves to it, and dials the
  /// other device from there (its NAT opened exactly that socket's port toward the other device).
  fn on_birthday_hit(&mut self, socket: UdpSocket, peer: SocketAddr, now: Instant) {
    self.stop_birthday();
    if self.maybe_connection.is_some() {
      return;
    }
    let (Some(direct), Some(certificate_hash)) = (
      self.maybe_socket.as_mut(),
      self.maybe_peer.as_ref().map(|peer| peer.certificate_hash),
    ) else {
      return;
    };
    if let Err(error) = direct.endpoint.rebind(socket) {
      debug!(%error, "the direct path endpoint couldn't move to the birthday socket");
      return;
    }
    direct.is_rebound = true;
    let client = match direct.client_config(certificate_hash) {
      Ok(client) => client,
      Err(error) => {
        debug!(%error, "the direct path's client configuration failed");
        return;
      },
    };
    let endpoint = direct.endpoint.clone();
    info!("birthday probe arrived: dialing from the socket it found");
    self.stop_dialing();
    self.selector.on_dialing(now);
    self.maybe_dialing = Some(tokio::spawn(dial(
      endpoint,
      client,
      vec![peer],
      self.inputs_tx.clone(),
    )));
  }

  fn stop_birthday(&mut self) {
    if let Some(birthday) = self.maybe_birthday.take() {
      birthday.abort();
    }
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
    *lock(&self.sender.maybe_link) = None;
    self.video.peer_wants.clear();
  }

  fn close_socket(&mut self) {
    self.stop_dialing();
    self.stop_birthday();
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
    engine.report_direct_path(None);
    self.stop_video(engine);
    self.close_socket();
    self.maybe_pair = None;
    self.maybe_key = None;
    self.maybe_peer = None;
    self.maybe_held_offer = None;
    self.maybe_reflexive = None;
    self.maybe_nat = None;
    self.maybe_mapped = None;
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
    let Some(copy) = direct_copy(datagram) else {
      return;
    };
    let maybe_connection = lock(&self.maybe_link)
      .as_ref()
      .map(|link| link.connection.clone());
    if let Some(connection) = maybe_connection {
      // Unreliable by design, as on the SFU path; the SFU's copy went out as well. Past the video's
      // queue: it waits behind at most the little of the video QUIC holds.
      let _ = connection.send_datagram(Bytes::from(copy));
    }
  }

  /// Sends a datagram of the own video's direct layer (`EngineOutput::DirectDatagram`), through
  /// the video's queue, while the direct path carries the media.
  pub(super) fn send_video(&self, datagram: &[u8]) {
    let maybe_video = lock(&self.maybe_link)
      .as_ref()
      .map(|link| Arc::clone(&link.video));
    if let Some(video) = maybe_video {
      video.send_datagram(datagram);
    }
  }
}

impl DirectSocket {
  /// A dual-stack socket on a free port with an endpoint on it, and a fresh certificate. It accepts
  /// nothing until an offer names the other device's certificate.
  fn open() -> Result<Self, DirectSetupError> {
    let socket = bind_wildcard(true)?;
    let raw = socket.try_clone()?;
    let endpoint = quinn::Endpoint::new(EndpointConfig::default(), None, socket, Arc::new(TokioRuntime))?;
    let mut probe_token = [0u8; DIRECT_PROBE_TOKEN_BYTES];
    OsRng.fill_bytes(&mut probe_token);
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
      raw,
      identity,
      certificate_hash,
      probe_token,
      is_rebound: false,
      accepting: tokio::spawn(async {}),
      #[cfg(feature = "port-mapping")]
      maybe_mapper: None,
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

/// The hard side's birthday round: up to [`BIRTHDAY_SOCKETS`] IPv4 sockets, each punching toward
/// the other device (at the times and TTLs of [`BIRTHDAY_PUNCHES`]) so the NAT maps a port for each,
/// and waiting for the other device's probe. The first socket a probe with `own_token` from the
/// other device's address reaches is handed back, with the system's TTL again; the others close.
async fn open_birthday(
  peer: SocketAddr,
  own_token: [u8; DIRECT_PROBE_TOKEN_BYTES],
  inputs: mpsc::UnboundedSender<DirectInput>,
) {
  let sockets = open_birthday_sockets().await;
  if sockets.is_empty() {
    let _ = inputs.send(DirectInput::BirthdayEnded);
    return;
  }
  debug!(sockets = sockets.len(), "birthday sockets open");
  let maybe_default_ttl = sockets.first().and_then(|socket| socket.ttl().ok());
  let sockets: Vec<Arc<tokio::net::UdpSocket>> = sockets.into_iter().map(Arc::new).collect();
  let (hit_tx, mut hit_rx) = mpsc::unbounded_channel::<(usize, SocketAddr)>();
  let mut readers = Vec::with_capacity(sockets.len());
  for (index, socket) in sockets.iter().enumerate() {
    let (socket, hit_tx) = (Arc::clone(socket), hit_tx.clone());
    readers.push(tokio::spawn(async move {
      let mut buffer = [0u8; BIRTHDAY_READ_BYTES];
      loop {
        // An error is an ICMP answer to a punch (Windows reports them on any UDP socket): the
        // socket is still good.
        let Ok((length, from)) = socket.recv_from(&mut buffer).await else {
          continue;
        };
        let is_peer = from.ip().to_canonical() == peer.ip().to_canonical();
        if is_peer && is_birthday_probe(&buffer[..length], &own_token) {
          let _ = hit_tx.send((index, from));
          return;
        }
      }
    }));
  }
  drop(hit_tx);
  let punch = birthday_probe(&own_token);
  let punching = {
    let sockets = sockets.clone();
    tokio::spawn(async move {
      let started = tokio::time::Instant::now();
      for (at, maybe_ttl) in BIRTHDAY_PUNCHES {
        tokio::time::sleep_until(started + at).await;
        for socket in &sockets {
          // A socket whose TTL can't be set punches with the one it has: that may cost its port
          // at the other NAT, not the round.
          if let Some(ttl) = maybe_ttl.or(maybe_default_ttl) {
            let _ = socket.set_ttl(ttl);
          }
          // A punch that can't go out maps nothing; the others still may.
          let _ = socket.send_to(&punch, peer).await;
        }
      }
    })
  };
  let maybe_hit = tokio::time::timeout(BIRTHDAY_TIMEOUT, hit_rx.recv())
    .await
    .ok()
    .flatten();
  punching.abort();
  // Aborted: it gives back its handles on the sockets.
  let _ = punching.await;
  for reader in &readers {
    reader.abort();
  }
  for reader in readers {
    // Aborted: each gives back its handle on the socket.
    let _ = reader.await;
  }
  let input = maybe_hit
    .and_then(|(index, from)| {
      let socket = Arc::into_inner(sockets.into_iter().nth(index)?)?;
      if let Some(ttl) = maybe_default_ttl {
        // The direct connection's packets go all the way.
        socket.set_ttl(ttl).ok()?;
      }
      let socket = socket.into_std().ok()?;
      Some(DirectInput::BirthdayHit { socket, peer: from })
    })
    .unwrap_or(DirectInput::BirthdayEnded);
  let _ = inputs.send(input);
}

/// Opens the birthday sockets: as many as the system allows, up to [`BIRTHDAY_SOCKETS`]; when it
/// runs short, the last [`BIRTHDAY_SPARE_SOCKETS`] close again so the app keeps descriptors.
async fn open_birthday_sockets() -> Vec<tokio::net::UdpSocket> {
  let mut sockets = Vec::with_capacity(BIRTHDAY_SOCKETS);
  for _ in 0..BIRTHDAY_SOCKETS {
    match tokio::net::UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))).await {
      Ok(socket) => sockets.push(socket),
      Err(error) => {
        debug!(%error, opened = sockets.len(), "no more birthday sockets");
        sockets.truncate(sockets.len().saturating_sub(BIRTHDAY_SPARE_SOCKETS));
        break;
      },
    }
  }
  sockets
}

/// The easy side's birthday round: probes with the other device's token from the direct path
/// socket to `ports` of `peer_ip`, [`BIRTHDAY_PROBE_INTERVAL`] apart. It ends by itself after the
/// last port; a connection that comes meanwhile stops it.
async fn probe_birthday(
  raw: UdpSocket,
  is_dual_stack: bool,
  peer_ip: IpAddr,
  peer_token: [u8; DIRECT_PROBE_TOKEN_BYTES],
  ports: Vec<u16>,
  inputs: mpsc::UnboundedSender<DirectInput>,
) {
  let socket = match raw
    .set_nonblocking(true)
    .and_then(|()| tokio::net::UdpSocket::from_std(raw))
  {
    Ok(socket) => socket,
    Err(error) => {
      debug!(%error, "birthday probes can't be sent");
      let _ = inputs.send(DirectInput::BirthdayEnded);
      return;
    },
  };
  // A dual-stack socket sends to IPv4 in the IPv4-mapped form.
  let peer_ip = match peer_ip {
    IpAddr::V4(ip) if is_dual_stack => IpAddr::V6(ip.to_ipv6_mapped()),
    ip => ip,
  };
  let probe = birthday_probe(&peer_token);
  let mut ticks = tokio::time::interval(BIRTHDAY_PROBE_INTERVAL);
  ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
  for port in ports {
    ticks.tick().await;
    // A probe that can't go out is one fewer; the round goes on.
    let _ = socket.send_to(&probe, SocketAddr::new(peer_ip, port)).await;
  }
  // The last probes' answers may still be on their way.
  tokio::time::sleep(BIRTHDAY_LINGER).await;
  let _ = inputs.send(DirectInput::BirthdayEnded);
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

/// [`reflect`] within [`REFLECT_TIMEOUT`].
async fn timed_reflect(
  endpoint: quinn::Endpoint,
  target: ReflectTarget,
  address: SocketAddr,
  secret: Base64Bytes,
) -> Result<SocketAddr, String> {
  tokio::time::timeout(REFLECT_TIMEOUT, reflect(endpoint, target, address, secret))
    .await
    .map_err(|_| "the SFU's reflection timed out".to_string())?
}

/// Asks the SFU from the direct path socket where its packets come from (ARCHITECTURE.md §6.7,
/// "Endpoint discovery"): a WebTransport connection on the same QUIC endpoint, `reflect` with the
/// session's secret, the SFU's `reflected`, and the close.
async fn reflect(
  endpoint: quinn::Endpoint,
  target: ReflectTarget,
  address: SocketAddr,
  secret: Base64Bytes,
) -> Result<SocketAddr, String> {
  let builder = ClientConfig::builder().with_bind_default();
  let builder = if target.certificate_hashes.is_empty() {
    with_trusted_roots(builder)
  } else {
    builder.with_server_certificate_hashes(target.certificate_hashes.iter().copied().map(Sha256Digest::new))
  };
  let config = builder.dns_resolver(PinnedResolver(address)).build();
  let client: Endpoint<Client> = Endpoint::client_on_endpoint(config, endpoint);
  let url = if address == target.address {
    target.sfu_url.clone()
  } else {
    with_port(&target.sfu_url, address.port()).ok_or("the SFU's address is no URL")?
  };
  let connection = client.connect(&url).await.map_err(|error| error.to_string())?;
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
