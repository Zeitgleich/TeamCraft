//! SFU restarts with real native clients over loopback. A UDP front plays the SFU's public
//! address: it forwards to the running SFU process and switches to the restarted one, as a
//! container restart or rolling deploy behind the same address would. A graceful shutdown closes
//! every connection as "going away" and the clients rejoin at once; after a crash they notice the
//! dead path. Either way the call never ends on their side and the server hears no leave. A
//! restarted SFU with a new self-signed certificate is reached with the hash its fresh tokens name.

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::net::{Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use teamcraft_media::audio_io::{AudioSink, AudioSource};
use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_engine::{CallEngineConfig, CallEngineEvent};
use teamcraft_media::call_native::{JoinTicketSource, NativeCall, NativeCallConfig, TicketFuture};
use teamcraft_media::call_resilience::{JoinTicket, ResilienceConfig};
use teamcraft_proto::{MediaCapabilities, SfuRosterReport};
use teamcraft_sfu::auth::{DevTokenVerifier, JoinGrant};
use teamcraft_sfu::config::SfuConfig;
use teamcraft_sfu::reporting::ParticipantReporter;
use teamcraft_sfu::server::{CALL_PATH, Sfu};
use tokio::net::UdpSocket;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use wtransport::Identity;

const CALL_ID: u64 = 43;
const CONVERSATION_ID: u64 = 44;
const ALICE: u64 = 1;
const BOB: u64 = 2;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn graceful_restart_brings_everyone_back_into_the_call() {
  let identity = identity();
  let announced = announce(&identity);
  let reports = Reports::default();
  let first = SfuProcess::start(&identity, &reports);
  let front = SfuFront::start(first.port).await;
  let mut alice = join(&front, &announced, ALICE).await;
  let mut bob = join(&front, &announced, BOB).await;
  wait_for_secured(&mut alice).await;
  wait_for_secured(&mut bob).await;

  // SIGTERM: the SFU goes away; its successor comes up at the same address.
  let second = SfuProcess::start(&identity, &reports);
  first.shut_down().await;
  front.switch_to(second.port);

  for call in [&mut alice, &mut bob] {
    wait_for(call, |event| *event == CallEngineEvent::Reconnecting).await;
    wait_for(call, |event| *event == CallEngineEvent::Reconnected).await;
  }
  wait_for_secured(&mut alice).await;
  wait_for_secured(&mut bob).await;
  assert_back_together(alice, bob, &reports).await;
}

/// Issue #452: the restarted SFU has a new self-signed certificate. The server names its hash with
/// the tokens from then on; the rejoins must use it, not the first join's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_with_a_new_certificate_is_rejoined_with_the_fresh_tokens_hash() {
  let original = identity();
  let announced = announce(&original);
  let reports = Reports::default();
  let first = SfuProcess::start(&original, &reports);
  let front = SfuFront::start(first.port).await;
  let mut alice = join(&front, &announced, ALICE).await;
  let mut bob = join(&front, &announced, BOB).await;
  wait_for_secured(&mut alice).await;
  wait_for_secured(&mut bob).await;

  let renewed = identity();
  let second = SfuProcess::start(&renewed, &reports);
  *announced.lock().unwrap() = certificate_hash(&renewed);
  first.shut_down().await;
  front.switch_to(second.port);

  for call in [&mut alice, &mut bob] {
    wait_for(call, |event| *event == CallEngineEvent::Reconnected).await;
  }
  wait_for_secured(&mut alice).await;
  wait_for_secured(&mut bob).await;
  assert_back_together(alice, bob, &reports).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crash_is_noticed_and_everyone_rejoins_the_restarted_sfu() {
  let identity = identity();
  let announced = announce(&identity);
  let reports = Reports::default();
  // A crashed process reports nothing; this one lives on unreachable, so its reports don't count.
  let first = SfuProcess::start(&identity, &Reports::default());
  let front = SfuFront::start(first.port).await;
  let mut alice = join(&front, &announced, ALICE).await;
  let mut bob = join(&front, &announced, BOB).await;
  wait_for_secured(&mut alice).await;
  wait_for_secured(&mut bob).await;

  // The SFU dies without a word; nothing answers at its address until the restart.
  front.go_dark();
  for call in [&mut alice, &mut bob] {
    wait_for(call, |event| *event == CallEngineEvent::Reconnecting).await;
  }
  let second = SfuProcess::start(&identity, &reports);
  front.switch_to(second.port);
  for call in [&mut alice, &mut bob] {
    wait_for(call, |event| *event == CallEngineEvent::Reconnected).await;
  }
  wait_for_secured(&mut alice).await;
  wait_for_secured(&mut bob).await;
  assert_back_together(alice, bob, &reports).await;
  first.kill();
}

/// Both hear each other in one DAVE group, and the server only ever heard of joins.
async fn assert_back_together(mut alice: NativeCall, mut bob: NativeCall, reports: &Reports) {
  let (alice_events, bob_events) = tokio::join!(
    collect(&mut alice, Duration::from_secs(1)),
    collect(&mut bob, Duration::from_secs(1)),
  );
  for event in alice_events.iter().chain(&bob_events) {
    assert!(
      !matches!(
        event,
        CallEngineEvent::Reconnecting | CallEngineEvent::EndedByHost { .. }
      ),
      "{event:?}"
    );
  }
  let has_left = reports
    .lock()
    .unwrap()
    .iter()
    .any(|report| matches!(report, Report::Left));
  assert!(!has_left, "{:?}", reports.lock().unwrap());
  // The restarted SFU's roster has both of them back in the call the server kept (it drops no one
  // within its restart grace), named with the conversation for a server that restarted too.
  let latest_roster = reports
    .lock()
    .unwrap()
    .iter()
    .rev()
    .find_map(|report| match report {
      Report::Roster(roster) => Some(roster.clone()),
      _ => None,
    })
    .expect("the SFU reports its roster");
  let [call] = &latest_roster.calls[..] else {
    panic!("{latest_roster:?}");
  };
  assert_eq!(call.call_id, id_of(CALL_ID));
  assert_eq!(call.maybe_conversation_id, Some(id_of(CONVERSATION_ID)));
  let mut devices: Vec<u64> = call
    .participants
    .iter()
    .map(|entry| entry.device_id.to_string().parse().unwrap())
    .collect();
  devices.sort_unstable();
  assert_eq!(devices, vec![ALICE, BOB], "{latest_roster:?}");
  let alice_stats = alice.leave().await.unwrap();
  let bob_stats = bob.leave().await.unwrap();
  assert!(alice_stats.maybe_privacy_code.is_some());
  assert_eq!(alice_stats.maybe_privacy_code, bob_stats.maybe_privacy_code);
  assert!(alice_stats.frames_received > 25, "{alice_stats:?}");
  assert!(bob_stats.frames_received > 25, "{bob_stats:?}");
}

type Reports = Arc<Mutex<Vec<Report>>>;

/// The SFU certificate's hash that `teamcraft-server` names with its tokens
/// (`[calls] sfu_certificate_sha256`).
type AnnouncedHash = Arc<Mutex<[u8; 32]>>;

#[derive(Debug, Clone)]
enum Report {
  Joined,
  Left,
  Roster(SfuRosterReport),
}

/// What the SFU would tell `teamcraft-server`.
struct RecordingReporter(Reports);

impl ParticipantReporter for RecordingReporter {
  fn joined(&self, _grant: JoinGrant) {
    self.0.lock().unwrap().push(Report::Joined);
  }

  fn left(&self, _grant: JoinGrant) {
    self.0.lock().unwrap().push(Report::Left);
  }

  fn roster(&self, report: SfuRosterReport) {
    self.0.lock().unwrap().push(Report::Roster(report));
  }
}

/// One running SFU, like one process of it.
struct SfuProcess {
  port: u16,
  maybe_shutdown: Option<oneshot::Sender<()>>,
  task: JoinHandle<()>,
}

impl SfuProcess {
  fn start(identity: &Identity, reports: &Reports) -> Self {
    let config = SfuConfig {
      bind_addr: SocketAddr::from((Ipv6Addr::LOCALHOST, 0)),
      idle_timeout: Duration::from_secs(2),
      reconnect_grace: Duration::from_secs(10),
      // Several rosters within the second the test listens after the rejoin.
      roster_interval: Duration::from_millis(200),
      ..SfuConfig::default()
    };
    let sfu = Sfu::bind(config, identity.clone_identity(), Arc::new(DevTokenVerifier))
      .unwrap()
      .with_reporter(Arc::new(RecordingReporter(Arc::clone(reports))));
    let port = sfu.local_addr().unwrap().port();
    let (shutdown, shutdown_rx) = oneshot::channel::<()>();
    let task = tokio::spawn(sfu.run_until(async {
      // A dropped sender is no shutdown request.
      if shutdown_rx.await.is_err() {
        std::future::pending::<()>().await;
      }
    }));
    Self {
      port,
      maybe_shutdown: Some(shutdown),
      task,
    }
  }

  /// SIGTERM, and wait until the SFU went away.
  async fn shut_down(mut self) {
    if let Some(shutdown) = self.maybe_shutdown.take() {
      shutdown.send(()).unwrap();
    }
    tokio::time::timeout(Duration::from_secs(5), &mut self.task)
      .await
      .expect("the SFU went away in time")
      .unwrap();
  }

  /// Stops accepting; sessions already running live on in the test runtime, unreachable.
  fn kill(self) {
    self.task.abort();
  }
}

/// The SFU's public address: forwards each client's packets from a socket of its own (a NAT
/// mapping) to the current SFU process, and the answers back.
struct SfuFront {
  front: Arc<UdpSocket>,
  state: Arc<FrontState>,
}

struct FrontState {
  maybe_backend: Mutex<Option<SocketAddr>>,
  /// Bumped on every switch: mappings of an older one are dropped, like a restarted host's.
  generation: AtomicU64,
  mappings: Mutex<HashMap<SocketAddr, (u64, Arc<UdpSocket>)>>,
}

impl SfuFront {
  async fn start(backend_port: u16) -> Self {
    let front = Arc::new(UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).await.unwrap());
    let state = Arc::new(FrontState {
      maybe_backend: Mutex::new(Some(backend(backend_port))),
      generation: AtomicU64::new(0),
      mappings: Mutex::new(HashMap::new()),
    });
    let (socket, shared) = (Arc::clone(&front), Arc::clone(&state));
    tokio::spawn(async move {
      let mut buffer = vec![0u8; 65_536];
      while let Ok((length, from)) = socket.recv_from(&mut buffer).await {
        let Some(sfu) = *shared.maybe_backend.lock().unwrap() else {
          continue;
        };
        let upstream = mapping(&socket, &shared, from);
        // Best effort, like any network.
        let _ = upstream.send_to(&buffer[..length], sfu).await;
      }
    });
    Self { front, state }
  }

  fn port(&self) -> u16 {
    self.front.local_addr().unwrap().port()
  }

  /// The restarted SFU answers at the address from now on.
  fn switch_to(&self, backend_port: u16) {
    self.state.generation.fetch_add(1, Ordering::Relaxed);
    *self.state.maybe_backend.lock().unwrap() = Some(backend(backend_port));
  }

  /// Nothing answers at the address any more.
  fn go_dark(&self) {
    self.state.generation.fetch_add(1, Ordering::Relaxed);
    *self.state.maybe_backend.lock().unwrap() = None;
  }
}

/// The client's upstream socket of the current generation, created on its first packet.
fn mapping(front: &Arc<UdpSocket>, state: &Arc<FrontState>, client: SocketAddr) -> Arc<UdpSocket> {
  let generation = state.generation.load(Ordering::Relaxed);
  let mut mappings = state.mappings.lock().unwrap();
  if let Some((mapped_generation, upstream)) = mappings.get(&client)
    && *mapped_generation == generation
  {
    return Arc::clone(upstream);
  }
  let socket = std::net::UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).unwrap();
  socket.set_nonblocking(true).unwrap();
  let upstream = Arc::new(UdpSocket::from_std(socket).unwrap());
  mappings.insert(client, (generation, Arc::clone(&upstream)));
  let (front, state, downstream) = (Arc::clone(front), Arc::clone(state), Arc::clone(&upstream));
  tokio::spawn(async move {
    let mut buffer = vec![0u8; 65_536];
    while let Ok((length, _)) = downstream.recv_from(&mut buffer).await {
      if state.generation.load(Ordering::Relaxed) != generation {
        return;
      }
      let _ = front.send_to(&buffer[..length], client).await;
    }
  });
  upstream
}

/// Tickets for the SFU's public address, as `teamcraft-server` hands them out.
struct FrontTickets {
  url: String,
  device: u64,
  announced: AnnouncedHash,
}

impl JoinTicketSource for FrontTickets {
  fn fetch(&self) -> TicketFuture {
    let ticket = JoinTicket {
      sfu_url: self.url.clone(),
      token: token(self.device),
      certificate_hashes: vec![*self.announced.lock().unwrap()],
    };
    Box::pin(async move { Ok(ticket) })
  }
}

async fn join(front: &SfuFront, announced: &AnnouncedHash, device: u64) -> NativeCall {
  let engine = CallEngineConfig {
    user_id: id_of(device),
    device_id: id_of(device),
    capabilities: MediaCapabilities::default(),
    bwe: BweConfig::default(),
  };
  let url = sfu_url(front.port());
  let mut config = NativeCallConfig::new(url.clone(), token(device), engine);
  config.certificate_hashes = vec![*announced.lock().unwrap()];
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
    going_away_spread: Duration::from_millis(200),
    ..ResilienceConfig::default()
  };
  config.maybe_ticket_source = Some(Arc::new(FrontTickets {
    url,
    device,
    announced: Arc::clone(announced),
  }));
  NativeCall::join(config).await.unwrap()
}

async fn wait_for_secured(call: &mut NativeCall) {
  wait_for(call, |event| matches!(event, CallEngineEvent::Secured { .. })).await;
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

fn identity() -> Identity {
  Identity::self_signed(["localhost", "127.0.0.1", "::1"]).unwrap()
}

fn announce(identity: &Identity) -> AnnouncedHash {
  Arc::new(Mutex::new(certificate_hash(identity)))
}

fn certificate_hash(identity: &Identity) -> [u8; 32] {
  *identity.certificate_chain().as_slice()[0].hash().as_ref()
}

fn backend(port: u16) -> SocketAddr {
  SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port)
}

fn token(device: u64) -> String {
  format!("dev:{CALL_ID}:{device}:{device}:{CONVERSATION_ID}")
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
