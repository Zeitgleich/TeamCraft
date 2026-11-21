//! Network changes with the real native client over loopback. A UDP relay in front of the SFU
//! plays the device's network: a new upstream socket is a new address (Wi-Fi to LTE, a NAT
//! rebinding), so QUIC has to migrate; a cut-off client socket is a socket the new network can't
//! use, so the client has to move to a new one; a black hole is a network that is gone, so the
//! client has to rejoin on a fresh ticket through its "new network" (straight to the SFU).

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{HashMap, HashSet};
use std::net::{Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use teamcraft_media::audio_io::{AudioSink, AudioSource};
use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_engine::{CallEngineConfig, CallEngineEvent};
use teamcraft_media::call_native::{JoinTicketSource, NativeCall, NativeCallConfig, TicketFuture};
use teamcraft_media::call_resilience::{JoinTicket, ResilienceConfig};
use teamcraft_proto::{DeviceId, MediaCapabilities};
use teamcraft_sfu::auth::DevTokenVerifier;
use teamcraft_sfu::config::SfuConfig;
use teamcraft_sfu::server::{CALL_PATH, Sfu};
use tokio::net::UdpSocket;
use wtransport::Identity;

use crate::common::secured::wait_for_same_epoch;

const CALL_ID: u64 = 41;
const ALICE: u64 = 1;
const BOB: u64 = 2;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn new_address_migrates_the_connection_without_rejoining() {
  let (sfu_port, hash) = start_sfu().await;
  let relay = Relay::start(sfu_port).await;
  let mut alice = join(relay.port(), hash, ALICE, None).await;
  let mut bob = join(sfu_port, hash, BOB, None).await;
  wait_for(&mut alice, |event| {
    matches!(event, CallEngineEvent::Secured { .. })
  })
  .await;
  wait_for(&mut bob, |event| matches!(event, CallEngineEvent::Secured { .. })).await;

  relay.rebind();
  alice.network_changed();
  let (alice_events, bob_events) = tokio::join!(
    collect(&mut alice, Duration::from_secs(2)),
    collect(&mut bob, Duration::from_secs(2)),
  );
  for event in alice_events.iter().chain(&bob_events) {
    assert!(
      !matches!(
        event,
        CallEngineEvent::Reconnecting
          | CallEngineEvent::ParticipantReconnecting(_)
          | CallEngineEvent::ParticipantLeft(_)
      ),
      "{event:?}"
    );
  }
  // Bob's tone reached Alice over the new path: the SFU validated it and sends there.
  assert!(
    relay.downstream_on_new_path() > 25,
    "{}",
    relay.downstream_on_new_path()
  );
  alice.leave().await.unwrap();
  bob.leave().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn old_socket_unusable_after_a_change_moves_to_a_new_socket_without_rejoining() {
  let (sfu_port, hash) = start_sfu().await;
  let relay = Relay::start(sfu_port).await;
  let mut alice = join(relay.port(), hash, ALICE, None).await;
  let mut bob = join(sfu_port, hash, BOB, None).await;
  wait_for_same_epoch(&mut alice, &mut bob).await;

  // The new network can't carry the old socket's packets (its interface is gone).
  relay.cut_off_client();
  alice.network_changed();
  let (alice_events, bob_events) = tokio::join!(
    collect(&mut alice, Duration::from_secs(2)),
    collect(&mut bob, Duration::from_secs(2)),
  );
  for event in alice_events.iter().chain(&bob_events) {
    assert!(
      !matches!(
        event,
        CallEngineEvent::Reconnecting
          | CallEngineEvent::ParticipantReconnecting(_)
          | CallEngineEvent::ParticipantLeft(_)
          | CallEngineEvent::Secured { .. }
      ),
      "{event:?}"
    );
  }
  // Bob's tone reaches Alice's new socket: same connection, no rejoin, no re-key.
  assert!(
    relay.downstream_after_cut_off() > 25,
    "{}",
    relay.downstream_after_cut_off()
  );
  alice.leave().await.unwrap();
  bob.leave().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn network_gone_after_a_change_rejoins_on_a_fresh_ticket() {
  let (sfu_port, hash) = start_sfu().await;
  let relay = Relay::start(sfu_port).await;
  let tickets = Arc::new(DirectTickets {
    sfu_port,
    issued: AtomicU64::new(0),
  });
  let mut alice = join(relay.port(), hash, ALICE, Some(tickets.clone())).await;
  let mut bob = join(sfu_port, hash, BOB, None).await;
  wait_for(&mut alice, |event| {
    matches!(event, CallEngineEvent::Secured { .. })
  })
  .await;
  let bob_secured = wait_for(&mut bob, |event| matches!(event, CallEngineEvent::Secured { .. })).await;

  relay.black_hole();
  alice.network_changed();
  wait_for(&mut alice, |event| *event == CallEngineEvent::Reconnecting).await;
  wait_for(&mut alice, |event| *event == CallEngineEvent::Reconnected).await;
  assert_eq!(tickets.issued.load(Ordering::Relaxed), 1);
  // Alice carries on in the group she held: secured again on Bob's epoch, with his code, and
  // nobody re-keyed (#1762). Bob stays in the call with her and hears of no new epoch.
  let alice_secured = wait_for(&mut alice, |event| {
    matches!(event, CallEngineEvent::Secured { .. })
  })
  .await;
  assert_eq!(alice_secured, bob_secured);
  let bob_events = collect(&mut bob, Duration::from_secs(1)).await;
  assert!(
    !bob_events.contains(&CallEngineEvent::ParticipantLeft(id_of::<DeviceId>(ALICE)))
      && !bob_events
        .iter()
        .any(|event| matches!(event, CallEngineEvent::Secured { .. })),
    "{bob_events:?}"
  );
  alice.leave().await.unwrap();
  bob.leave().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn silent_path_is_noticed_without_a_network_change_report() {
  let (sfu_port, hash) = start_sfu().await;
  let relay = Relay::start(sfu_port).await;
  let tickets = Arc::new(DirectTickets {
    sfu_port,
    issued: AtomicU64::new(0),
  });
  let mut alice = join(relay.port(), hash, ALICE, Some(tickets)).await;
  let mut bob = join(sfu_port, hash, BOB, None).await;
  wait_for(&mut alice, |event| {
    matches!(event, CallEngineEvent::Secured { .. })
  })
  .await;
  wait_for(&mut bob, |event| matches!(event, CallEngineEvent::Secured { .. })).await;

  relay.black_hole();
  wait_for(&mut alice, |event| *event == CallEngineEvent::Reconnecting).await;
  wait_for(&mut alice, |event| *event == CallEngineEvent::Reconnected).await;
  alice.leave().await.unwrap();
  bob.leave().await.unwrap();
}

/// A UDP relay between one client and the SFU, the device's network: each client socket goes out
/// through an upstream socket of its own (a NAT's mappings: the own voice takes a connection of its
/// own, #2141), and a rebinding gives every one of them a new one.
struct Relay {
  front: Arc<UdpSocket>,
  state: Arc<RelayState>,
}

struct RelayState {
  sfu: SocketAddr,
  /// Bumped by a rebinding: mappings of an earlier generation are gone.
  generation: AtomicU64,
  /// Each client socket's upstream socket, and its generation.
  upstreams: Mutex<HashMap<SocketAddr, (u64, Arc<UdpSocket>)>>,
  /// The client's sockets that can't reach the SFU any more.
  cut_off_clients: Mutex<HashSet<SocketAddr>>,
  is_black_hole: AtomicBool,
  downstream_on_new_path: AtomicU64,
  downstream_after_cut_off: AtomicU64,
}

impl Relay {
  async fn start(sfu_port: u16) -> Self {
    let front = Arc::new(UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).await.unwrap());
    let state = Arc::new(RelayState {
      sfu: SocketAddr::new(Ipv6Addr::LOCALHOST.into(), sfu_port),
      generation: AtomicU64::new(0),
      upstreams: Mutex::default(),
      cut_off_clients: Mutex::default(),
      is_black_hole: AtomicBool::new(false),
      downstream_on_new_path: AtomicU64::new(0),
      downstream_after_cut_off: AtomicU64::new(0),
    });
    let relay = Self { front, state };
    let (front, state) = (Arc::clone(&relay.front), Arc::clone(&relay.state));
    tokio::spawn(async move {
      let mut buffer = vec![0u8; 65_536];
      while let Ok((length, from)) = front.recv_from(&mut buffer).await {
        if state.cut_off_clients.lock().unwrap().contains(&from)
          || state.is_black_hole.load(Ordering::Relaxed)
        {
          continue;
        }
        let upstream = upstream_of(&state, &front, from).await;
        // Best effort, like any network.
        let _ = upstream.send_to(&buffer[..length], state.sfu).await;
      }
    });
    relay
  }

  fn port(&self) -> u16 {
    self.front.local_addr().unwrap().port()
  }

  /// The client's traffic now leaves from other addresses; the old ones are gone.
  fn rebind(&self) {
    self.state.generation.fetch_add(1, Ordering::Relaxed);
  }

  /// The client's current sockets can't reach the SFU any more, neither way; other sockets of the
  /// client can.
  fn cut_off_client(&self) {
    let clients: Vec<SocketAddr> = self.state.upstreams.lock().unwrap().keys().copied().collect();
    self.state.cut_off_clients.lock().unwrap().extend(clients);
  }

  fn downstream_after_cut_off(&self) -> u64 {
    self.state.downstream_after_cut_off.load(Ordering::Relaxed)
  }

  fn black_hole(&self) {
    self.state.is_black_hole.store(true, Ordering::Relaxed);
  }

  fn downstream_on_new_path(&self) -> u64 {
    self.state.downstream_on_new_path.load(Ordering::Relaxed)
  }
}

/// The upstream socket of `client` in the current generation; a new one forwards the SFU's packets
/// to `client` while its generation lasts.
async fn upstream_of(state: &Arc<RelayState>, front: &Arc<UdpSocket>, client: SocketAddr) -> Arc<UdpSocket> {
  let generation = state.generation.load(Ordering::Relaxed);
  if let Some((mapped, socket)) = state.upstreams.lock().unwrap().get(&client)
    && *mapped == generation
  {
    return Arc::clone(socket);
  }
  let socket = Arc::new(UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).await.unwrap());
  state
    .upstreams
    .lock()
    .unwrap()
    .insert(client, (generation, Arc::clone(&socket)));
  let (reading, front, state) = (Arc::clone(&socket), Arc::clone(front), Arc::clone(state));
  tokio::spawn(async move {
    let mut buffer = vec![0u8; 65_536];
    while let Ok((length, _)) = reading.recv_from(&mut buffer).await {
      if state.generation.load(Ordering::Relaxed) != generation {
        return;
      }
      if state.is_black_hole.load(Ordering::Relaxed) {
        continue;
      }
      let (is_cut_off, is_after_cut_off) = {
        let cut_off = state.cut_off_clients.lock().unwrap();
        (cut_off.contains(&client), !cut_off.is_empty())
      };
      if is_cut_off {
        continue;
      }
      if generation > 0 {
        state.downstream_on_new_path.fetch_add(1, Ordering::Relaxed);
      }
      if is_after_cut_off {
        state.downstream_after_cut_off.fetch_add(1, Ordering::Relaxed);
      }
      let _ = front.send_to(&buffer[..length], client).await;
    }
  });
  socket
}

/// Tickets for the SFU straight, as if issued on the device's new network.
struct DirectTickets {
  sfu_port: u16,
  issued: AtomicU64,
}

impl JoinTicketSource for DirectTickets {
  fn fetch(&self) -> TicketFuture {
    self.issued.fetch_add(1, Ordering::Relaxed);
    let ticket = JoinTicket {
      sfu_url: sfu_url(self.sfu_port),
      token: format!("dev:{CALL_ID}:{ALICE}:{ALICE}"),
      // The pinned certificate (`maybe_certificate_hash`) applies to every connection.
      certificate_hashes: Vec::new(),
      maybe_fallback_url: None,
    };
    Box::pin(async move { Ok(ticket) })
  }
}

async fn join(
  port: u16,
  certificate_hash: [u8; 32],
  device: u64,
  maybe_tickets: Option<Arc<DirectTickets>>,
) -> NativeCall {
  let engine = CallEngineConfig {
    user_id: id_of(device),
    device_id: id_of(device),
    capabilities: MediaCapabilities::default(),
    bwe: BweConfig::default(),
  };
  let mut config = NativeCallConfig::new(sfu_url(port), format!("dev:{CALL_ID}:{device}:{device}"), engine);
  config.maybe_certificate_hash = Some(certificate_hash);
  config.source = AudioSource::Tone {
    frequency_hz: 300.0 + 100.0 * device as f32,
    amplitude: 0.3,
  };
  config.sink = AudioSink::Discard;
  config.resilience = ResilienceConfig {
    keep_alive_interval: Duration::from_millis(200),
    idle_timeout: Duration::from_secs(3),
    stall_timeout: Duration::from_millis(800),
    handover_timeout: Duration::from_millis(400),
    ..ResilienceConfig::default()
  };
  config.maybe_ticket_source = maybe_tickets.map(|tickets| tickets as Arc<dyn JoinTicketSource>);
  NativeCall::join(config).await.unwrap()
}

/// The next event matching `predicate` within five seconds.
async fn wait_for(call: &mut NativeCall, predicate: impl Fn(&CallEngineEvent) -> bool) -> CallEngineEvent {
  tokio::time::timeout(Duration::from_secs(5), async {
    loop {
      let event = call.next_event().await.expect("call ended before the event");
      if predicate(&event) {
        return event;
      }
    }
  })
  .await
  .expect("event did not arrive")
}

/// The events of the next `duration`, without frames.
async fn collect(call: &mut NativeCall, duration: Duration) -> Vec<CallEngineEvent> {
  let mut events = Vec::new();
  let deadline = tokio::time::Instant::now() + duration;
  while let Ok(Some(event)) = tokio::time::timeout_at(deadline, call.next_event()).await {
    if !matches!(event, CallEngineEvent::UplinkEstimate { .. }) {
      events.push(event);
    }
  }
  events
}

async fn start_sfu() -> (u16, [u8; 32]) {
  let config = SfuConfig {
    bind_addr: SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
    idle_timeout: Duration::from_secs(2),
    reconnect_grace: Duration::from_secs(10),
    ..SfuConfig::default()
  };
  let identity = Identity::self_signed(["localhost", "127.0.0.1", "::1"]).unwrap();
  let sfu = Sfu::bind(config, identity, Arc::new(DevTokenVerifier)).unwrap();
  let port = sfu.local_addr().unwrap().port();
  let certificate_hash = *sfu.certificate_hash().as_ref();
  tokio::spawn(sfu.run());
  (port, certificate_hash)
}

fn sfu_url(port: u16) -> String {
  format!(
    "https://{}{CALL_PATH}",
    SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port)
  )
}

fn id_of<T: std::str::FromStr>(raw: u64) -> T
where
  T::Err: std::fmt::Debug,
{
  raw.to_string().parse().unwrap()
}
