//! The connection supervisor against the fake gateway, on tokio's paused clock: dead connections,
//! stalled handshakes, network changes, wake-ups, refused tokens and backoff.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use common::{ClockJump, PASSPHRASE, TokioClock, options, settings};
use teamcraft_core::PendingEnrollment;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::clock::Clock;
use teamcraft_core::connection::{
  ConnectionSettings, ConnectionState, ConnectionStatus, ConnectionSupervisor, NetworkChanges,
};
use teamcraft_core::runner::{RunnerCommand, RunnerHandle, RunnerSettings, SyncRunner};
use teamcraft_core::store::MemoryStore;
use teamcraft_proto::{GatewayEvent, GatewayServerMessage, UserId};
use tokio::time::Instant;

/// Steps in which the tests look at the state.
const POLL: Duration = Duration::from_millis(100);

#[tokio::test(start_paused = true)]
async fn a_silently_dead_connection_is_replaced_within_a_minute() {
  let rig = Rig::start("Alice").await;
  rig
    .drive(|rig| async move {
      rig.wait_for(is_connected, Duration::from_secs(5)).await;
      rig.server.freeze_gateways(rig.user);
      // Two missed heartbeat answers (25 s each) and the probe slack.
      rig.wait_for(is_offline, Duration::from_secs(60)).await;
      rig.wait_for(is_connected, Duration::from_secs(5)).await;
      assert_eq!(rig.server.gateway_connect_count(), 2);
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_stalled_handshake_times_out_and_is_retried() {
  let rig = Rig::start("Bob").await;
  rig.server.set_gateway_silent(rig.user, true);
  rig
    .drive(|rig| async move {
      // The socket opens but `hello` never comes: the attempt must fail, not hang.
      rig.wait_for(is_offline, Duration::from_secs(16)).await;
      rig.server.set_gateway_silent(rig.user, false);
      rig.wait_for(is_connected, Duration::from_secs(35)).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_network_change_replaces_a_dead_connection_within_five_seconds() {
  let rig = Rig::start("Carol").await;
  rig
    .drive(|rig| async move {
      rig.wait_for(is_connected, Duration::from_secs(5)).await;
      // Wi-Fi to mobile: the old connection is half-open, a new one works.
      rig.server.freeze_gateways(rig.user);
      rig.network.notify();
      let started = Instant::now();
      rig.wait_for_new_connection(2, Duration::from_secs(5)).await;
      assert!(
        started.elapsed() < Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
      );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_dead_connection_found_by_the_probe_is_replaced_without_a_backoff() {
  let rig = Rig::start("Grace").await;
  rig
    .drive(|rig| async move {
      rig.wait_for(is_connected, Duration::from_secs(5)).await;
      // Back from the camera app: the socket died while the page was frozen.
      rig.server.freeze_gateways(rig.user);
      rig.network.notify();
      let started = Instant::now();
      rig.wait_for_new_connection(2, Duration::from_secs(4)).await;
      let probe = ConnectionSettings::default().probe_timeout;
      assert!(
        started.elapsed() <= probe + POLL * 3,
        "took {:?}",
        started.elapsed()
      );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_network_change_keeps_a_working_connection() {
  let rig = Rig::start("Dave").await;
  rig
    .drive(|rig| async move {
      rig.wait_for(is_connected, Duration::from_secs(5)).await;
      rig.network.notify();
      tokio::time::sleep(Duration::from_secs(10)).await;
      assert_eq!(rig.status.get(), ConnectionState::Connected);
      assert_eq!(
        rig.server.gateway_connect_count(),
        1,
        "the answered probe kept it"
      );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn waking_up_from_sleep_replaces_the_connection_at_once() {
  let rig = Rig::start("Erin").await;
  rig
    .drive(|rig| async move {
      rig.wait_for(is_connected, Duration::from_secs(5)).await;
      // A night in standby: the wall clock jumps, the connection died meanwhile.
      rig.server.freeze_gateways(rig.user);
      rig.jump.forward(Duration::from_secs(8 * 60 * 60));
      let started = Instant::now();
      rig.wait_for(is_offline, Duration::from_secs(10)).await;
      rig.wait_for(is_connected, Duration::from_secs(5)).await;
      assert!(
        started.elapsed() < Duration::from_secs(10),
        "took {:?}",
        started.elapsed()
      );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_refused_token_asks_to_sign_in_and_stops_retrying() {
  let rig = Rig::start("Frank").await;
  rig.server.set_signed_out(rig.user, true);
  rig
    .drive(|rig| async move {
      rig
        .wait_for(
          |state| state == ConnectionState::SignInRequired,
          Duration::from_secs(5),
        )
        .await;
      tokio::time::sleep(Duration::from_secs(10 * 60)).await;
      assert_eq!(rig.status.get(), ConnectionState::SignInRequired);
      assert_eq!(
        rig.server.gateway_refusal_count(),
        1,
        "the refused token is not offered again"
      );
      // Signed in again; the network change (or a new token) tries once more.
      rig.server.set_signed_out(rig.user, false);
      rig.network.notify();
      rig.wait_for(is_connected, Duration::from_secs(5)).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn an_unreachable_server_is_retried_with_growing_pauses() {
  let rig = Rig::start("Grace").await;
  rig.server.set_offline(rig.user, true);
  rig
    .drive(|rig| async move {
      tokio::time::sleep(Duration::from_secs(5 * 60)).await;
      let attempts = rig.server.gateway_connect_count();
      assert_eq!(attempts, 0, "connecting fails before a socket opens");
      assert!(matches!(rig.status.get(), ConnectionState::Offline { .. }));
      rig.server.set_offline(rig.user, false);
      // At most the capped pause (30 s) until the next attempt.
      rig.wait_for(is_connected, Duration::from_secs(31)).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_server_that_drops_every_connection_is_not_hammered() {
  let rig = Rig::start("Heidi").await;
  rig
    .drive(|rig| async move {
      rig.wait_for(is_connected, Duration::from_secs(5)).await;
      // The server accepts and drops each connection again right away, for two minutes.
      let started = Instant::now();
      while started.elapsed() < Duration::from_secs(120) {
        rig.server.drop_gateways(rig.user);
        tokio::time::sleep(POLL).await;
      }
      // Without a stable connection the backoff keeps growing: 1, 2, 4, 8, 16, 30, 30 ... s.
      let connects = rig.server.gateway_connect_count();
      assert!(connects <= 13, "{connects} connections in two minutes");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn an_idle_connection_does_not_report_changes_for_its_heartbeats() {
  let rig = Rig::start("Ivan").await;
  rig
    .drive(|rig| async move {
      rig.wait_for(is_connected, Duration::from_secs(5)).await;
      tokio::time::sleep(Duration::from_secs(10)).await;
      let before = rig.changes.load(Ordering::SeqCst);
      // Twelve heartbeats; each change makes the apps re-read the store and the web client
      // refetch its counters, so heartbeats must not count.
      tokio::time::sleep(Duration::from_secs(5 * 60)).await;
      let changes = rig.changes.load(Ordering::SeqCst) - before;
      assert_eq!(rig.status.get(), ConnectionState::Connected);
      // The account data sync (every minute) still reports one.
      assert!(changes <= 5, "{changes} changes in five idle minutes");
    })
    .await;
}

/// Issue #325: work that holds the loop longer than the server's heartbeat timeout (a big upload,
/// a resync of many conversations) keeps the connection, and what arrives meanwhile comes after it.
#[tokio::test(start_paused = true)]
async fn long_work_keeps_the_connection_and_holds_what_arrives() {
  let server = FakeServer::new();
  let api = server.add_user("Judy");
  let user = api.user_id();
  let (clock, _) = TokioClock::new();
  let mut client = PendingEnrollment::new()
    .complete(
      api,
      Arc::new(MemoryStore::new()),
      Box::new(clock.clone()),
      settings(),
      options(PASSPHRASE),
    )
    .await
    .unwrap();
  let timeout = Duration::from_secs(90);
  server.enforce_heartbeat_timeout(Arc::new(clock.clone()), timeout);
  let mut supervisor = ConnectionSupervisor::new(ConnectionSettings::default(), ConnectionStatus::new());
  assert!(supervisor.connect_if_due(&mut client).await.is_some());
  let publisher = server.clone();
  let work = async move {
    tokio::time::sleep(Duration::from_secs(60)).await;
    publisher.publish_event(&[user], &GatewayEvent::AnnouncementUpdate);
    tokio::time::sleep(timeout).await;
    "done"
  };
  assert_eq!(supervisor.keep_alive(clock.now_ms(), work).await, "done");
  let held = supervisor.receive().await;
  assert!(
    matches!(held, Some(Ok(GatewayServerMessage::Event { .. }))),
    "{held:?}"
  );
  supervisor.on_frame(&mut client, held).await;
  supervisor.on_tick(clock.now_ms()).await;
  assert_eq!(supervisor.state(), ConnectionState::Connected);
  assert_eq!(server.heartbeat_timeout_count(), 0);
  assert!(
    server.heartbeat_count() >= 5,
    "{} heartbeats",
    server.heartbeat_count()
  );
}

/// A runner on the fake server, with handles to steer the network and the clock.
struct Rig {
  server: FakeServer,
  user: UserId,
  network: NetworkChanges,
  status: ConnectionStatus,
  jump: ClockJump,
  /// How often the runner reported a change.
  changes: Arc<AtomicUsize>,
  handle: RunnerHandle,
  runner: SyncRunner<teamcraft_core::api::fake::FakeServerApi, Arc<MemoryStore>>,
}

/// What the test body gets: everything but the runner, which runs alongside.
#[derive(Clone)]
struct Steer {
  server: FakeServer,
  user: UserId,
  network: NetworkChanges,
  status: ConnectionStatus,
  jump: ClockJump,
  changes: Arc<AtomicUsize>,
}

impl Rig {
  async fn start(name: &str) -> Self {
    let server = FakeServer::new();
    let api = server.add_user(name);
    let user = api.user_id();
    let (clock, jump) = TokioClock::new();
    let client = PendingEnrollment::new()
      .complete(
        api,
        Arc::new(MemoryStore::new()),
        Box::new(clock),
        settings(),
        options(PASSPHRASE),
      )
      .await
      .unwrap();
    let changes = Arc::new(AtomicUsize::new(0));
    let counter = changes.clone();
    let (mut runner, handle) = SyncRunner::new(
      client,
      RunnerSettings::default(),
      Box::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
      }),
    );
    let network = NetworkChanges::new();
    runner.watch_network(&network);
    let status = runner.connection_status();
    Self {
      server,
      user,
      network,
      status,
      jump,
      changes,
      handle,
      runner,
    }
  }

  /// Runs the runner while `body` steers; stops it afterwards.
  async fn drive<F, Fut>(self, body: F)
  where
    F: FnOnce(Steer) -> Fut,
    Fut: Future<Output = ()>,
  {
    let steer = Steer {
      server: self.server,
      user: self.user,
      network: self.network,
      status: self.status,
      jump: self.jump,
      changes: self.changes,
    };
    let handle = self.handle;
    let test = async move {
      body(steer).await;
      assert!(handle.send(RunnerCommand::Stop));
    };
    tokio::join!(self.runner.run(), test);
  }
}

impl Steer {
  /// Waits until the state matches, at most `limit`.
  async fn wait_for(&self, matches: impl Fn(ConnectionState) -> bool, limit: Duration) {
    let started = Instant::now();
    while !matches(self.status.get()) {
      assert!(
        started.elapsed() <= limit,
        "still {:?} after {limit:?}",
        self.status.get()
      );
      tokio::time::sleep(POLL).await;
    }
  }
}

impl Steer {
  /// Waits until the `count`th gateway connection is up, at most `limit`.
  async fn wait_for_new_connection(&self, count: u64, limit: Duration) {
    let started = Instant::now();
    while self.server.gateway_connect_count() < count || !is_connected(self.status.get()) {
      assert!(
        started.elapsed() <= limit,
        "still {:?} after {limit:?}",
        self.status.get()
      );
      tokio::time::sleep(POLL).await;
    }
  }
}

fn is_connected(state: ConnectionState) -> bool {
  state == ConnectionState::Connected
}

fn is_offline(state: ConnectionState) -> bool {
  matches!(state, ConnectionState::Offline { .. })
}
