//! Network changes with the real native client over loopback. A UDP relay in front of the SFU
//! plays the device's network: a new upstream socket is a new address (Wi-Fi to LTE, a NAT
//! rebinding), so QUIC has to migrate; a black hole is a network that is gone, so the client has
//! to rejoin on a fresh ticket through its "new network" (straight to the SFU).

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

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

  relay.rebind().await;
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
  wait_for(&mut bob, |event| matches!(event, CallEngineEvent::Secured { .. })).await;

  relay.black_hole();
  alice.network_changed();
  wait_for(&mut alice, |event| *event == CallEngineEvent::Reconnecting).await;
  wait_for(&mut alice, |event| *event == CallEngineEvent::Reconnected).await;
  wait_for(&mut alice, |event| {
    matches!(event, CallEngineEvent::Secured { .. })
  })
  .await;
  assert_eq!(tickets.issued.load(Ordering::Relaxed), 1);
  // Bob stays in the call with Alice; the group re-keyed.
  let bob_events = collect(&mut bob, Duration::from_secs(1)).await;
  assert!(
    !bob_events.contains(&CallEngineEvent::ParticipantLeft(id_of::<DeviceId>(ALICE))),
    "{bob_events:?}"
  );
  let alice_stats = alice.leave().await.unwrap();
  let bob_stats = bob.leave().await.unwrap();
  assert_eq!(alice_stats.maybe_privacy_code, bob_stats.maybe_privacy_code);
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

/// A UDP relay between one client and the SFU, with a replaceable upstream socket.
struct Relay {
  front: Arc<UdpSocket>,
  state: Arc<RelayState>,
}

struct RelayState {
  sfu: SocketAddr,
  upstream: Mutex<Arc<UdpSocket>>,
  maybe_client: Mutex<Option<SocketAddr>>,
  is_black_hole: AtomicBool,
  generation: AtomicU64,
  downstream_on_new_path: AtomicU64,
}

impl Relay {
  async fn start(sfu_port: u16) -> Self {
    let front = Arc::new(UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).await.unwrap());
    let state = Arc::new(RelayState {
      sfu: SocketAddr::new(Ipv6Addr::LOCALHOST.into(), sfu_port),
      upstream: Mutex::new(Arc::new(UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).await.unwrap())),
      maybe_client: Mutex::new(None),
      is_black_hole: AtomicBool::new(false),
      generation: AtomicU64::new(0),
      downstream_on_new_path: AtomicU64::new(0),
    });
    let relay = Self { front, state };
    relay.spawn_downstream(0);
    let (front, state) = (Arc::clone(&relay.front), Arc::clone(&relay.state));
    tokio::spawn(async move {
      let mut buffer = vec![0u8; 65_536];
      while let Ok((length, from)) = front.recv_from(&mut buffer).await {
        *state.maybe_client.lock().unwrap() = Some(from);
        if state.is_black_hole.load(Ordering::Relaxed) {
          continue;
        }
        let upstream = Arc::clone(&state.upstream.lock().unwrap());
        // Best effort, like any network.
        let _ = upstream.send_to(&buffer[..length], state.sfu).await;
      }
    });
    relay
  }

  fn port(&self) -> u16 {
    self.front.local_addr().unwrap().port()
  }

  /// The client's traffic now leaves from another address; the old one is gone.
  async fn rebind(&self) {
    let socket = Arc::new(UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).await.unwrap());
    *self.state.upstream.lock().unwrap() = socket;
    let generation = self.state.generation.fetch_add(1, Ordering::Relaxed) + 1;
    self.spawn_downstream(generation);
  }

  fn black_hole(&self) {
    self.state.is_black_hole.store(true, Ordering::Relaxed);
  }

  fn downstream_on_new_path(&self) -> u64 {
    self.state.downstream_on_new_path.load(Ordering::Relaxed)
  }

  /// Forwards the SFU's packets on the upstream socket of `generation` to the client, while that
  /// socket is the current one.
  fn spawn_downstream(&self, generation: u64) {
    let socket = Arc::clone(&self.state.upstream.lock().unwrap());
    let (front, state) = (Arc::clone(&self.front), Arc::clone(&self.state));
    tokio::spawn(async move {
      let mut buffer = vec![0u8; 65_536];
      while let Ok((length, _)) = socket.recv_from(&mut buffer).await {
        if state.generation.load(Ordering::Relaxed) != generation {
          return;
        }
        if state.is_black_hole.load(Ordering::Relaxed) {
          continue;
        }
        let maybe_client = *state.maybe_client.lock().unwrap();
        if let Some(client) = maybe_client {
          if generation > 0 {
            state.downstream_on_new_path.fetch_add(1, Ordering::Relaxed);
          }
          let _ = front.send_to(&buffer[..length], client).await;
        }
      }
    });
  }
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
