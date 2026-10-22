//! Load test: gateway fan-out to hundreds of connections while a bot posts continuously, and how
//! much that slows down a small channel next door (ARCHITECTURE.md §1.1). Slow, so it only runs on
//! request:
//!
//! ```sh
//! DATABASE_URL=postgres://... cargo test -p teamcraft-server --features integration \
//!   --test perf_gateway -- --ignored --nocapture
//! ```
//!
//! `TEAMCRAFT_PERF_CONNECTIONS` (default 500), `TEAMCRAFT_PERF_BOT_PER_MINUTE` (300) and
//! `TEAMCRAFT_PERF_SECONDS` (30) set the load. Client and server share one process and one machine,
//! so the numbers are an upper bound for latency and say nothing about network distance.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use futures_util::StreamExt;
use reqwest::Method;
use serde_json::json;
use teamcraft_proto::{GatewayEvent, GatewayServerMessage};
use teamcraft_server::testing::{TestApp, TestUser};
use tokio_tungstenite::tungstenite::Message;

type Arrivals = Arc<Mutex<Vec<(usize, String, Instant)>>>;

fn env(name: &str, default: u64) -> u64 {
  std::env::var(name)
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(default)
}

fn summary(label: &str, mut times: Vec<Duration>) -> Duration {
  times.sort();
  let at = |p: f64| times[((times.len() as f64 - 1.0) * p).round() as usize];
  println!(
    "PERF {label}: p50 {:?} p95 {:?} p99 {:?} max {:?} ({} samples)",
    at(0.5),
    at(0.95),
    at(0.99),
    times[times.len() - 1],
    times.len()
  );
  at(0.5)
}

#[tokio::test]
#[ignore = "load test, a minute or more of runtime; run with --ignored"]
async fn fan_out_to_hundreds_of_connections_does_not_slow_other_channels() {
  let _ = tracing_subscriber::fmt()
    .with_max_level(tracing::Level::WARN)
    .try_init();
  let connections = usize::try_from(env("TEAMCRAFT_PERF_CONNECTIONS", 500)).unwrap();
  let per_minute = env("TEAMCRAFT_PERF_BOT_PER_MINUTE", 300);
  let seconds = env("TEAMCRAFT_PERF_SECONDS", 30);
  let Some(app) = TestApp::start().await else { return };
  let alice = owner();
  let admin_user = admin();

  // A big channel with `connections` members, a small one with two of them, and a bot in the big one.
  let users: Vec<TestUser> = (0..connections).map(|n| member(&format!("u{n}"))).collect();
  let space = create_space(&app, &alice, "Load", "invite_only").await;
  let big = create_channel(&app, &alice, &space, "big", "private").await;
  let small = create_channel(&app, &alice, &space, "small", "private").await;
  let mut devices = Vec::new();
  let started = Instant::now();
  for chunk in users.chunks(50) {
    let ids = futures_util::future::join_all(chunk.iter().map(|user| user_id(&app, user))).await;
    let registered =
      futures_util::future::join_all(chunk.iter().map(|user| register_device(&app, user))).await;
    for (id, device) in ids.into_iter().zip(registered) {
      add_space_member(&app, &alice, &space, &id, "member").await;
      add_channel_member(&app, &alice, &big, &id).await;
      devices.push(device);
    }
  }
  let probe_ids = [user_id(&app, &users[0]).await, user_id(&app, &users[1]).await];
  for id in &probe_ids {
    add_channel_member(&app, &alice, &small, id).await;
  }
  let bot = app
    .post(&admin_user, "/admin/bots", json!({"name": "load bot"}))
    .await;
  let (bot_id, bot_token) = (
    bot.body["bot"]["id"].as_str().unwrap().to_owned(),
    bot.body["token"].as_str().unwrap().to_owned(),
  );
  add_space_member(&app, &alice, &space, &bot_id, "guest").await;
  add_channel_member(&app, &alice, &big, &bot_id).await;
  let bot_device = call_with_token(
    &app,
    &bot_token,
    Method::POST,
    "/devices",
    Some(json!({"name": "bot", "public_key": "cHVibGlj", "cross_signature": "c2ln"})),
  )
  .await
  .1["id"]
    .as_str()
    .unwrap()
    .to_owned();
  println!("PERF set up {connections} users in {:?}", started.elapsed());

  // Connect everybody; every socket records when each message_create arrives.
  let arrivals: Arrivals = Arc::default();
  let connect_started = Instant::now();
  for (offset, chunk) in users.chunks(50).enumerate() {
    let sockets = futures_util::future::join_all(chunk.iter().enumerate().map(|(i, user)| {
      let (app, device) = (&app, &devices[offset * 50 + i]);
      async move { ws::connect(app, user, device).await }
    }))
    .await;
    for (i, mut socket) in sockets.into_iter().enumerate() {
      let (index, arrivals) = (offset * 50 + i, arrivals.clone());
      tokio::spawn(async move {
        while let Some(Ok(Message::Text(text))) = socket.next().await {
          if let Ok(GatewayServerMessage::Event {
            event: GatewayEvent::MessageCreate { message },
            ..
          }) = serde_json::from_str(text.as_str())
          {
            arrivals
              .lock()
              .unwrap()
              .push((index, message.id.to_string(), Instant::now()));
          }
        }
      });
    }
  }
  println!(
    "PERF {connections} gateway connections established in {:?}",
    connect_started.elapsed()
  );

  // The probe: u0 posts into the small channel twice a second; u1's socket sees it.
  let probe = |label: &'static str, duration: Duration, first_id: u32| {
    let (app, small, device, sender) = (&app, small.clone(), devices[0].clone(), &users[0]);
    let arrivals = arrivals.clone();
    async move {
      let mut sent: HashMap<String, Instant> = HashMap::new();
      let end = Instant::now() + duration;
      let mut n = first_id;
      while Instant::now() < end {
        n += 1;
        let mut body = message_body(1, &device);
        body["client_message_id"] = json!(format!("{n:032x}"));
        let at = Instant::now();
        let reply = app
          .post(sender, &format!("/conversations/{small}/messages"), body)
          .await;
        sent.insert(reply.id(), at);
        tokio::time::sleep(Duration::from_millis(500)).await;
      }
      tokio::time::sleep(Duration::from_secs(1)).await;
      let latencies: Vec<Duration> = arrivals
        .lock()
        .unwrap()
        .iter()
        .filter(|(socket, id, _)| *socket == 1 && sent.contains_key(id))
        .map(|(_, id, received)| received.duration_since(sent[id]))
        .collect();
      assert_eq!(
        latencies.len(),
        sent.len(),
        "{label}: every probe message reaches the other member"
      );
      summary(
        &format!("small channel, probe message to its other member ({label})"),
        latencies,
      )
    }
  };

  let baseline = probe("no bot traffic", Duration::from_secs(10), 0).await;

  // The bot posts to the big channel at the configured rate while the probe runs again.
  let interval = Duration::from_millis(60_000 / per_minute.max(1));
  let bot_sent: Arc<Mutex<HashMap<String, Instant>>> = Arc::default();
  let bot_task = {
    let (http, base, token, channel, device, sent) = (
      app.http.clone(),
      app.base_url.clone(),
      bot_token.clone(),
      big.clone(),
      bot_device.clone(),
      bot_sent.clone(),
    );
    let mut post_times = Vec::new();
    tokio::spawn(async move {
      let end = Instant::now() + Duration::from_secs(seconds);
      let mut n = 0_u32;
      let mut next = Instant::now();
      while Instant::now() < end {
        n += 1;
        let mut body = message_body(1, &device);
        body["client_message_id"] = json!(format!("{:032x}", 0x1_0000_0000_u64 + u64::from(n)));
        let at = Instant::now();
        let reply = http
          .post(format!("{base}/api/v1/conversations/{channel}/messages"))
          .bearer_auth(&token)
          .json(&body)
          .send()
          .await
          .unwrap();
        post_times.push(at.elapsed());
        let id = reply.json::<serde_json::Value>().await.unwrap()["id"]
          .as_str()
          .unwrap()
          .to_owned();
        sent.lock().unwrap().insert(id, at);
        next += interval;
        tokio::time::sleep_until(next.into()).await;
      }
      post_times
    })
  };
  let loaded = probe("while the bot posts", Duration::from_secs(seconds), 1_000_000).await;
  let post_times = bot_task.await.unwrap();
  tokio::time::sleep(Duration::from_secs(2)).await;
  summary("bot: POST /messages round trip", post_times);

  // Every connection got every bot message; fan-out latency to the big channel.
  let sent = bot_sent.lock().unwrap().clone();
  let arrived = arrivals.lock().unwrap().clone();
  let mut per_socket: HashMap<usize, usize> = HashMap::new();
  let mut latencies = Vec::new();
  for (socket, id, received) in &arrived {
    if let Some(at) = sent.get(id) {
      *per_socket.entry(*socket).or_default() += 1;
      latencies.push(received.duration_since(*at));
    }
  }
  println!(
    "PERF bot sent {} messages to {connections} connections = {} deliveries",
    sent.len(),
    latencies.len()
  );
  let slowest = summary("big channel: bot message to every member (fan-out)", latencies);
  assert_eq!(
    per_socket.len(),
    connections,
    "every connection received bot messages"
  );
  assert!(
    per_socket.values().all(|count| *count == sent.len()),
    "some connections missed messages"
  );
  println!("PERF probe latency grew from {baseline:?} to {loaded:?} under bot traffic");
  assert!(
    loaded < baseline * 20 + Duration::from_millis(100),
    "the small channel slowed down: {baseline:?} -> {loaded:?}"
  );
  assert!(slowest < Duration::from_secs(1));
  app.stop().await;
}
