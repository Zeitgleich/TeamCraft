//! The app on a bad network (robustness.md, "Network loss and change", "Hostile networks"; issue
//! #972): a client driven by the same runner and connection supervisor as the apps, its store on a
//! real disk, reaching the server through a relay whose port the kernel makes bad
//! ([`teamcraft_e2e::netem`]): a link that goes dead for seconds or for longer than the resume
//! window, connections reset, no UDP, heavy changing packet loss with jitter ("a train"), and a link
//! that drops out for a few seconds again and again. Others keep writing meanwhile.
//!
//! Every scenario checks that each message arrives exactly once and measures how long the client
//! takes to notice and to recover. Run inside a network namespace:
//!
//! ```sh
//! scripts/e2e-network.sh
//! ```

use std::time::{Duration, Instant};

use teamcraft_core::connection::ConnectionState;
use teamcraft_core::store::LocalStore;
use teamcraft_e2e::app::{App, DiskDir, disk_store, mobile_store};
use teamcraft_e2e::netem::{Condition, PortConditions};
use teamcraft_e2e::relay::TcpRelay;
use teamcraft_e2e::scenario::space_with;
use teamcraft_e2e::{Peer, Stack};
use teamcraft_proto::ConversationId;
use tokio::task::LocalSet;

/// Channels the app's user shares with the writers.
const CHANNELS: usize = 4;
/// Short, so a dead link of a minute outlasts it, as a night's standby does.
const RESUME_WINDOW_SECS: u64 = 15;
/// Pause between two messages the writers send while the link is bad.
const WRITE_EVERY: Duration = Duration::from_millis(700);
/// Longest any message may take to arrive once the link is good again.
const ARRIVAL_TIMEOUT: Duration = Duration::from_secs(90);

/// The link goes dead for a few seconds, shorter than a heartbeat: nothing may get lost, and the
/// messages come once the link is back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the network namespace: scripts/e2e-network.sh"]
async fn a_short_dead_link() {
  run(Plan::steady(
    "short dead link",
    Condition::Blackhole,
    Duration::from_secs(8),
  ))
  .await;
}

/// The link stays dead past the resume window (a night's standby, a train tunnel): the session is
/// gone and the app comes back through a full sync, told by the platform that the network is back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the network namespace: scripts/e2e-network.sh"]
async fn a_long_dead_link_then_the_network_is_back() {
  let mut plan = Plan::steady(
    "long dead link, network signal",
    Condition::Blackhole,
    Duration::from_secs(120),
  );
  plan.signals_network = true;
  run(plan).await;
}

/// The same without the platform's signal (a network change the OS didn't report): the app finds
/// out by itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the network namespace: scripts/e2e-network.sh"]
async fn a_long_dead_link_without_a_signal() {
  run(Plan::steady(
    "long dead link, no signal",
    Condition::Blackhole,
    Duration::from_secs(120),
  ))
  .await;
}

/// Connections are reset at once (a firewall that rejects, a server port that closed).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the network namespace: scripts/e2e-network.sh"]
async fn connections_are_reset() {
  run(Plan::steady("resets", Condition::Reset, Duration::from_secs(10))).await;
}

/// No UDP at all: chat runs over TCP and must not notice.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the network namespace: scripts/e2e-network.sh"]
async fn no_udp() {
  let mut plan = Plan::steady("no UDP", Condition::NoUdp, Duration::from_secs(15));
  plan.must_stay_live = true;
  run(plan).await;
}

/// Heavy loss that changes every few seconds, with jitter: messages keep coming, late but complete.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the network namespace: scripts/e2e-network.sh"]
async fn heavy_changing_loss() {
  let steps = [5.0, 40.0, 15.0, 60.0, 25.0, 50.0, 10.0, 35.0]
    .into_iter()
    .map(|loss_percent| {
      (
        Condition::Lossy {
          delay: Duration::from_millis(80),
          jitter: Duration::from_millis(60),
          loss_percent,
          maybe_rate_kbit: None,
        },
        Duration::from_secs(4),
      )
    })
    .collect();
  run(Plan {
    name: "heavy changing loss",
    steps,
    signals_network: false,
    must_stay_live: false,
  })
  .await;
}

/// The link drops out for two seconds, again and again, without losing packets in between.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the network namespace: scripts/e2e-network.sh"]
async fn a_link_that_drops_out_again_and_again() {
  let mut steps = Vec::new();
  for _ in 0..8 {
    steps.push((Condition::Blackhole, Duration::from_secs(2)));
    steps.push((Condition::Clear, Duration::from_secs(3)));
  }
  run(Plan {
    name: "drop-outs",
    steps,
    signals_network: false,
    must_stay_live: false,
  })
  .await;
}

/// A narrow, slow link after a dead one (a phone on one bar): the full sync gets through it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the network namespace: scripts/e2e-network.sh"]
async fn a_slow_narrow_link_after_a_dead_one() {
  run(Plan {
    name: "dead, then slow and narrow",
    steps: vec![
      (Condition::Blackhole, Duration::from_secs(30)),
      (
        Condition::Lossy {
          delay: Duration::from_millis(300),
          jitter: Duration::from_millis(100),
          loss_percent: 3.0,
          maybe_rate_kbit: Some(96),
        },
        Duration::from_secs(20),
      ),
    ],
    signals_network: true,
    must_stay_live: false,
  })
  .await;
}

/// What a scenario does to the link, in steps, while the writers write.
struct Plan {
  name: &'static str,
  steps: Vec<(Condition, Duration)>,
  /// The platform reports the network as back when the link is restored.
  signals_network: bool,
  /// Messages must keep arriving while the condition holds (it shouldn't affect chat at all).
  must_stay_live: bool,
}

impl Plan {
  fn steady(name: &'static str, condition: Condition, duration: Duration) -> Self {
    Self {
      name,
      steps: vec![(condition, duration)],
      signals_network: false,
      must_stay_live: false,
    }
  }
}

/// The desktop's store, or with `E2E_STORE=mobile` the mobile apps'.
async fn run(plan: Plan) {
  let is_mobile = std::env::var("E2E_STORE").is_ok_and(|store| store == "mobile");
  let local = LocalSet::new();
  if is_mobile {
    local.run_until(run_local(plan, "mobile", mobile_store)).await;
  } else {
    local.run_until(run_local(plan, "desktop", disk_store)).await;
  }
}

async fn run_local<S: LocalStore + Clone + 'static>(
  plan: Plan,
  store_name: &str,
  open_store: fn(&std::path::Path) -> S,
) {
  let Some(stack) = Stack::builder()
    .with_env("TEAMCRAFT_SECURITY__RATE_LIMITS__IS_ENABLED", "false")
    .with_env(
      "TEAMCRAFT_GATEWAY__RESUME_WINDOW_SECS",
      &RESUME_WINDOW_SECS.to_string(),
    )
    .start()
    .await
  else {
    return;
  };
  let server_address = stack.base_url.trim_start_matches("http://").to_owned();
  let relay = TcpRelay::start(&server_address).await.unwrap();
  let Some(link) = PortConditions::take(relay.port) else {
    stack.stop().await;
    return;
  };
  let directory = DiskDir::new("network");
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut bob = Peer::enroll(&stack, "bob").await;
  let carol = Peer::enroll_with_store(&stack, "carol", "carol desktop", open_store(directory.path())).await;
  let space = space_with(&stack, &alice, "Network", &[(&bob, "member")]).await;
  stack
    .add_space_member(
      &alice.tokens,
      &space.to_string(),
      &carol.user_id().to_string(),
      "member",
    )
    .await;
  let mut channels = Vec::new();
  for index in 0..CHANNELS {
    let channel = alice
      .client
      .create_channel(space, &format!("network-{index}"))
      .await
      .unwrap();
    for member in [bob.user_id(), carol.user_id()] {
      alice.client.add_member(channel, member).await.unwrap();
    }
    channels.push(channel);
  }
  bob.wait_joined(*channels.last().unwrap()).await;

  let app = App::start(
    &format!("http://127.0.0.1:{}", relay.port),
    carol.tokens.clone(),
    carol.store.clone(),
  );
  drop(carol);
  app.wait_connected(Duration::from_secs(60)).await;
  let mut expected: Vec<(ConversationId, String)> = Vec::new();
  let before = write(&mut alice, channels[0], &format!("{}: before", plan.name)).await;
  let steady = app.wait_for(&before.0, &before.1, Duration::from_secs(30)).await;
  expected.push(before);

  // The condition holds while alice and bob keep writing.
  let states = app.record_states();
  let started = Instant::now();
  let mut live_delays: Vec<Duration> = Vec::new();
  let mut sent = 0_usize;
  for (condition, duration) in &plan.steps {
    link.set(*condition);
    let until = Instant::now() + *duration;
    while Instant::now() < until {
      let channel = channels[sent % channels.len()];
      let writer = if sent.is_multiple_of(2) {
        &mut alice
      } else {
        &mut bob
      };
      let message = write(writer, channel, &format!("{}: during {sent}", plan.name)).await;
      sent += 1;
      if plan.must_stay_live {
        live_delays.push(
          app
            .wait_for(&message.0, &message.1, Duration::from_secs(10))
            .await,
        );
      }
      expected.push(message);
      tokio::time::sleep(WRITE_EVERY).await;
    }
  }
  let bad_for = started.elapsed();
  link.set(Condition::Clear);
  let restored = Instant::now();
  if plan.signals_network {
    app.network.notify();
  }
  let after = write(&mut alice, channels[0], &format!("{}: after", plan.name)).await;
  expected.push(after.clone());
  app.wait_for(&after.0, &after.1, ARRIVAL_TIMEOUT).await;
  let first_live = restored.elapsed();
  app.wait_all(&expected, ARRIVAL_TIMEOUT).await;
  let all_arrived = restored.elapsed();
  app.wait_connected(ARRIVAL_TIMEOUT).await;
  app.assert_each_once(&channels, &expected);
  let states = states.borrow().clone();
  let maybe_noticed = states
    .iter()
    .find(|(_, state)| *state != ConnectionState::Connected)
    .map(|(at, _)| at.saturating_duration_since(started));
  let maybe_reconnected = states
    .iter()
    .find(|(at, state)| *at >= restored && *state == ConnectionState::Connected)
    .map(|(at, _)| at.saturating_duration_since(restored));
  let reconnects = states
    .iter()
    .filter(|(_, state)| *state == ConnectionState::Connecting)
    .count();
  eprintln!(
    "network ({store_name}): {}: bad for {:.1} s, {} messages meanwhile; steady delivery {} ms; noticed after {}; \
     {reconnects} connection attempts; after the link was back: connected {}, newest message {} ms, all {} ms{}",
    plan.name,
    bad_for.as_secs_f64(),
    sent,
    steady.as_millis(),
    maybe_noticed.map_or("never".to_owned(), |noticed| format!(
      "{:.1} s",
      noticed.as_secs_f64()
    )),
    maybe_reconnected.map_or("stayed connected".to_owned(), |connected| format!(
      "after {} ms",
      connected.as_millis()
    )),
    first_live.as_millis(),
    all_arrived.as_millis(),
    if live_delays.is_empty() {
      String::new()
    } else {
      format!(
        "; live while bad: max {} ms",
        live_delays.iter().max().unwrap().as_millis()
      )
    }
  );
  drop(link);
  app.stop().await;
  drop(relay);
  stack.stop().await;
  drop(directory);
}

async fn write(peer: &mut Peer, channel: ConversationId, text: &str) -> (ConversationId, String) {
  peer.client.send_text(channel, None, text).await.unwrap();
  (channel, text.to_owned())
}
