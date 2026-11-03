//! Coming back to the app (issue #714): how long the core takes from "the app opens" to "synced",
//! phase by phase, and how many requests it makes, for a workspace that stayed as it was, one where
//! a few channels got messages, and a resume within the window. Real server, real Keycloak tokens,
//! real MLS; measured on the local machine and through a relay that adds [`FAR_RTT`] to every
//! round trip, like a phone or laptop far from the server. The numbers are in docs/performance.md
//! ("Coming back: requests and round trips").
//!
//! The budget: a fresh session on a workspace that didn't change costs a fixed number of requests,
//! whatever the number of conversations, and is synced within [`SYNC_BUDGET`] also at [`FAR_RTT`].

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use teamcraft_core::api::HttpServerApi;
use teamcraft_core::clock::SystemClock;
use teamcraft_core::{Client, ClientSettings, GatewayOutcome};
use teamcraft_e2e::relay::TcpRelay;
use teamcraft_e2e::scenario::{channel_with, space_with};
use teamcraft_e2e::{Peer, Stack};
use teamcraft_proto::{ConversationId, GatewayServerMessage};

/// Spaces and channels per space Alice shares with Carol; `SYNC_PERF_SPACES` and
/// `SYNC_PERF_CHANNELS_PER_SPACE` measure a larger workspace (docs/performance.md).
const SPACES: usize = 3;
const CHANNELS_PER_SPACE: usize = 8;
/// Channels that get a message while Carol is away in the second scenario.
const CHANGED_CHANNELS: usize = 3;
/// The round trip the relay adds: a client in another region or on a good mobile network.
const FAR_RTT: Duration = Duration::from_millis(30);
/// Product requirement (#714): synced within this after the app opens, whatever the workspace's size.
const SYNC_BUDGET: Duration = Duration::from_millis(300);
/// Requests of a fresh session on an unchanged workspace, independent of its size: handshake,
/// workspace log, own summary, spaces, the sync heads, welcomes, read state, notification
/// settings, key packages, account data, presence, link requests and their like.
const MAX_UNCHANGED_REQUESTS: u64 = 18;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn coming_back_costs_what_changed_not_the_workspace() {
  let Some(stack) = Stack::builder()
    .with_env("TEAMCRAFT_LOGGING__FILTER", "info,tower_http=debug")
    // Seeding a large workspace is many requests in a short time.
    .with_env("TEAMCRAFT_SECURITY__RATE_LIMITS__IS_ENABLED", "false")
    .start()
    .await
  else {
    return;
  };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut carol = Peer::enroll(&stack, "carol").await;
  let mut channels = Vec::new();
  let seeding = Instant::now();
  let spaces = scale("SYNC_PERF_SPACES", SPACES);
  let channels_per_space = scale("SYNC_PERF_CHANNELS_PER_SPACE", CHANNELS_PER_SPACE);
  for space_index in 0..spaces {
    let space = space_with(
      &stack,
      &alice,
      &format!("Space {space_index}"),
      &[(&carol, "member")],
    )
    .await;
    for channel_index in 0..channels_per_space {
      let name = format!("channel-{space_index}-{channel_index}");
      channels.push(channel_with(&mut alice, space, &name, &mut [&mut carol]).await);
      // A large workspace takes longer to seed than the server waits for a heartbeat.
      alice.heartbeat().await;
      carol.heartbeat().await;
    }
  }
  for (index, channel) in channels.iter().enumerate() {
    alice
      .client
      .send_text(*channel, None, &format!("hello {index}"))
      .await
      .unwrap();
  }
  let last = *channels.last().unwrap();
  carol
    .wait_for_text(last, &format!("hello {}", channels.len() - 1))
    .await;
  eprintln!(
    "sync_perf: seeded {} channels in {:.1} s",
    channels.len(),
    seeding.elapsed().as_secs_f64()
  );
  let server_address = stack.base_url.trim_start_matches("http://").to_owned();
  let relay = TcpRelay::start_with_latency(&server_address, FAR_RTT)
    .await
    .unwrap();
  let far_url = format!("http://127.0.0.1:{}", relay.port);
  // A first fresh session settles what the seeding left (key packages, notes to self).
  reopen(&stack, &stack.base_url, &mut carol, "warm-up").await;

  let local = scenarios(
    &stack,
    &stack.base_url,
    &mut alice,
    &mut carol,
    &channels,
    "local",
  )
  .await;
  let far = scenarios(&stack, &far_url, &mut alice, &mut carol, &channels, "30 ms away").await;

  for (where_, measured) in [("local", &local), ("30 ms away", &far)] {
    assert!(
      measured.unchanged.requests <= MAX_UNCHANGED_REQUESTS,
      "{where_}: a fresh session on an unchanged workspace of {} channels made {} requests (budget {MAX_UNCHANGED_REQUESTS}): {:?}",
      channels.len(),
      measured.unchanged.requests,
      measured.unchanged.paths
    );
    assert!(
      measured.changed.requests <= MAX_UNCHANGED_REQUESTS + 2 * CHANGED_CHANNELS as u64,
      "{where_}: three changed channels made {} requests: {:?}",
      measured.changed.requests,
      measured.changed.paths
    );
    assert!(
      measured.unchanged.elapsed <= SYNC_BUDGET,
      "{where_}: a fresh session on an unchanged workspace took {:?} (budget {SYNC_BUDGET:?}): {:?}",
      measured.unchanged.elapsed,
      measured.unchanged.phases
    );
    assert!(
      measured.resumed.elapsed <= SYNC_BUDGET,
      "{where_}: a resume took {:?} (budget {SYNC_BUDGET:?})",
      measured.resumed.elapsed
    );
  }
  drop(relay);
  stack.stop().await;
}

fn scale(variable: &str, default: usize) -> usize {
  std::env::var(variable)
    .ok()
    .and_then(|value| value.parse().ok())
    .unwrap_or(default)
}

/// The three ways of coming back, measured one after the other.
struct Scenarios {
  unchanged: Measured,
  changed: Measured,
  resumed: Measured,
}

async fn scenarios(
  stack: &Stack,
  base_url: &str,
  alice: &mut Peer,
  carol: &mut Peer,
  channels: &[ConversationId],
  label: &str,
) -> Scenarios {
  let unchanged = reopen(stack, base_url, carol, &format!("{label}, unchanged workspace")).await;
  let mut sent = Vec::new();
  for channel in channels.iter().take(CHANGED_CHANNELS) {
    let text = format!("while away ({label}) in {channel}");
    alice.client.send_text(*channel, None, &text).await.unwrap();
    sent.push((*channel, text));
  }
  let changed = reopen(
    stack,
    base_url,
    carol,
    &format!("{label}, three channels changed"),
  )
  .await;
  for (channel, text) in sent {
    assert!(
      carol.texts(channel).contains(&text),
      "the message sent while carol was away arrives in {channel}"
    );
  }
  let resumed = resume(stack, carol, &format!("{label}, resume")).await;
  Scenarios {
    unchanged,
    changed,
    resumed,
  }
}

/// What one connection cost.
struct Measured {
  elapsed: Duration,
  requests: u64,
  phases: Vec<(&'static str, i64)>,
  paths: BTreeMap<String, usize>,
}

/// The app opens again after the resume window passed: a new client on the same store, a fresh
/// gateway session and its sync.
async fn reopen(stack: &Stack, base_url: &str, peer: &mut Peer, label: &str) -> Measured {
  // The server's resume window passed: the client is told its session is gone.
  peer
    .client
    .handle_frame(GatewayServerMessage::SessionInvalid)
    .await
    .unwrap();
  let requests_before = peer.tokens.uses();
  let log_before = stack.server_log().len();
  let started = Instant::now();
  let api = HttpServerApi::new(base_url, peer.tokens.clone());
  let mut client = Client::open(
    api,
    peer.store.clone(),
    Box::new(SystemClock),
    ClientSettings::default(),
  )
  .unwrap()
  .expect("an enrolled store");
  let opened = started.elapsed();
  let (gateway, outcome) = client.connect_gateway().await.unwrap();
  let elapsed = started.elapsed();
  assert_eq!(outcome, GatewayOutcome::Fresh);
  peer.client = client;
  peer.gateway = gateway;
  let measured = measured(stack, peer, elapsed, requests_before, log_before);
  report(label, &measured, Some(opened));
  measured
}

/// The connection drops and comes back within the resume window.
async fn resume(stack: &Stack, peer: &mut Peer, label: &str) -> Measured {
  let requests_before = peer.tokens.uses();
  let log_before = stack.server_log().len();
  let started = Instant::now();
  let (gateway, outcome) = peer.client.connect_gateway().await.unwrap();
  let elapsed = started.elapsed();
  assert_eq!(outcome, GatewayOutcome::Resumed);
  peer.gateway = gateway;
  let measured = measured(stack, peer, elapsed, requests_before, log_before);
  report(label, &measured, None);
  measured
}

fn measured(
  stack: &Stack,
  peer: &Peer,
  elapsed: Duration,
  requests_before: u64,
  log_before: usize,
) -> Measured {
  let log = stack.server_log();
  Measured {
    elapsed,
    requests: peer.tokens.uses() - requests_before,
    phases: peer
      .client
      .last_connection_phases()
      .iter()
      .map(|phase| (phase.name, phase.ms))
      .collect(),
    paths: request_paths(log.get(log_before..).unwrap_or_default()),
  }
}

fn report(label: &str, measured: &Measured, maybe_opened: Option<Duration>) {
  let phases: Vec<String> = measured
    .phases
    .iter()
    .map(|(name, ms)| format!("{name}={ms}"))
    .collect();
  eprintln!(
    "sync_perf: {label}: {} ms, {} requests{}\n  phases: {}\n  paths: {:?}",
    measured.elapsed.as_millis(),
    measured.requests,
    maybe_opened.map_or(String::new(), |opened| format!(
      ", store opened in {} ms",
      opened.as_millis()
    )),
    phases.join(" "),
    measured.paths
  );
}

/// The API paths the server log shows requests for, IDs replaced by `{id}`, with their counts.
fn request_paths(log: &str) -> BTreeMap<String, usize> {
  let mut paths = BTreeMap::new();
  for line in log
    .lines()
    .filter(|line| line.contains("started processing request"))
  {
    let Some(start) = line.find("uri=") else {
      continue;
    };
    let uri = line[start + 4..].split([' ', '}']).next().unwrap_or_default();
    let path = uri.split('?').next().unwrap_or_default();
    let normalized: Vec<&str> = path
      .split('/')
      .map(|part| {
        if !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()) {
          "{id}"
        } else {
          part
        }
      })
      .collect();
    *paths.entry(normalized.join("/")).or_insert(0) += 1;
  }
  paths
}
