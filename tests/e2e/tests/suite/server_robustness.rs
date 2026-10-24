//! The server under the conditions of .claude/rules/robustness.md ("Server side"): restart and
//! SIGTERM with clients connected, gateway resume against full resync, an expired resume window,
//! retried and concurrent writes, Postgres going away for a moment, a consumer that stops reading,
//! and the process' memory and CPU. Results and method: docs/performance.md ("Server process").

use std::time::{Duration, Instant};

use reqwest::StatusCode;
use teamcraft_core::GatewayOutcome;
use teamcraft_e2e::process::{ProcessStats, is_tcp_established};
use teamcraft_e2e::raw_gateway::RawGateway;
use teamcraft_e2e::scenario::{channel_with, space_with};
use teamcraft_e2e::{Peer, Stack, UserTokens};
use teamcraft_proto::{
  Base64Bytes, ClientMessageId, ConversationId, DeviceId, GatewayClientMessage, GatewayServerMessage,
  SendMessageRequest,
};

/// Small replay buffer and window so the tests can run past both quickly.
const RESUME_BUFFER_EVENTS: usize = 20;
const RESUME_WINDOW_SECS: u64 = 2;

mod resume {
  use super::*;

  /// Within the window the server replays exactly what was missed; past the buffer it refuses, and
  /// the client starts a new session (and refetches over REST).
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn a_resume_replays_what_was_missed_and_a_gap_asks_for_a_new_session() {
    let Some(mut rig) = Rig::start(Stack::builder()).await else {
      return;
    };
    let carol_token = rig.carol.tokens.token_async().await;
    let carol_device = rig.carol.client.device_id();

    let mut first = RawGateway::connect(&rig.stack).await;
    let session = first.identify(carol_token.clone(), carol_device).await;
    rig.post_messages(5, 64).await;
    let mut last_seq = 0;
    while last_seq < 5 {
      match first.receive().await {
        Some(GatewayServerMessage::Event { seq, .. }) => last_seq = seq,
        Some(_) => {},
        None => panic!("the connection ended before five events arrived"),
      }
    }
    drop(first);

    rig.post_messages(3, 64).await;
    let mut second = RawGateway::connect(&rig.stack).await;
    let answer = second
      .resume(carol_token.clone(), carol_device, &session, last_seq)
      .await;
    assert_eq!(answer, Some(GatewayServerMessage::Resumed { replayed: 3 }));
    for expected in 6..=8 {
      match second.receive().await {
        Some(GatewayServerMessage::Event { seq, .. }) => assert_eq!(seq, expected),
        other => panic!("expected replayed event {expected}, got {other:?}"),
      }
    }
    drop(second);

    // More than the buffer holds: the oldest missed events are gone.
    rig.post_messages(RESUME_BUFFER_EVENTS + 5, 64).await;
    let mut third = RawGateway::connect(&rig.stack).await;
    let answer = third.resume(carol_token, carol_device, &session, 8).await;
    assert_eq!(answer, Some(GatewayServerMessage::SessionInvalid));
    rig.finish().await;
  }

  /// Past the resume window the session is gone even if no event was missed.
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn a_resume_after_the_window_asks_for_a_new_session() {
    let Some(rig) = Rig::start(Stack::builder()).await else {
      return;
    };
    let carol_token = rig.carol.tokens.token_async().await;
    let carol_device = rig.carol.client.device_id();
    let mut first = RawGateway::connect(&rig.stack).await;
    let session = first.identify(carol_token.clone(), carol_device).await;
    drop(first);

    // The window is measured by the server's clock; there is no way to advance it from outside.
    tokio::time::sleep(Duration::from_secs(RESUME_WINDOW_SECS + 2)).await;
    let mut second = RawGateway::connect(&rig.stack).await;
    let answer = second.resume(carol_token, carol_device, &session, 0).await;
    assert_eq!(answer, Some(GatewayServerMessage::SessionInvalid));
    rig.finish().await;
  }

  /// The client core end to end: a session the server forgot leads to a fresh session with a full
  /// sync, and the message sent meanwhile arrives.
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn the_client_falls_back_to_a_full_sync_when_its_session_is_gone() {
    let Some(mut rig) = Rig::start(Stack::builder()).await else {
      return;
    };
    // Carol's gateway connection ends when the server stops; the restart forgets all sessions.
    rig.stack.restart_server().await;
    rig
      .alice
      .client
      .send_text(rig.channel, None, "sent while carol was away")
      .await
      .unwrap();
    let (gateway, outcome) = rig.carol.client.connect_gateway().await.unwrap();
    rig.carol.gateway = gateway;
    assert_eq!(outcome, GatewayOutcome::Fresh);
    rig
      .carol
      .wait_for_text(rig.channel, "sent while carol was away")
      .await;
    rig.finish().await;
  }
}

mod restart {
  use super::*;

  /// SIGTERM with gateway connections open: the server exits within seconds (it does not wait for
  /// WebSockets), the connections end, and after the restart everything works without user action.
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn sigterm_with_connected_clients_drains_quickly_and_clients_recover() {
    let Some(mut rig) = Rig::start(Stack::builder()).await else {
      return;
    };
    let carol_token = rig.carol.tokens.token_async().await;
    let mut raw = RawGateway::connect(&rig.stack).await;
    raw.identify(carol_token, rig.carol.client.device_id()).await;

    let shutdown = rig.stack.restart_server().await;
    eprintln!("PERF sigterm with 3 gateway connections: exit after {shutdown:?}");
    assert!(
      shutdown < Duration::from_secs(10),
      "the server took {shutdown:?} to stop"
    );
    let (_, ending) = raw.drain(Duration::from_secs(5)).await;
    eprintln!("PERF sigterm: raw gateway connection ended with {ending:?}");
    assert!(
      ending.is_some(),
      "the gateway connection stayed open after SIGTERM"
    );

    // Both clients reconnect and catch up.
    let (gateway, _) = rig.alice.client.connect_gateway().await.unwrap();
    rig.alice.gateway = gateway;
    rig
      .alice
      .client
      .send_text(rig.channel, None, "after the restart")
      .await
      .unwrap();
    let (gateway, outcome) = rig.carol.client.connect_gateway().await.unwrap();
    rig.carol.gateway = gateway;
    assert_eq!(outcome, GatewayOutcome::Fresh);
    rig.carol.wait_for_text(rig.channel, "after the restart").await;
    rig.finish().await;
  }
}

mod retries {
  use super::*;

  /// A retried send after a lost response, also several at once (two devices of a flaky client,
  /// or an outbox flush racing a manual retry): one message, the same ID for everybody.
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn concurrent_retries_of_one_send_store_one_message() {
    let Some(rig) = Rig::start(Stack::builder()).await else {
      return;
    };
    let request = message_request(rig.alice.client.device_id(), 128);
    let token = rig.alice.tokens.token_async().await;
    let attempts = (0..10).map(|_| {
      let http = rig.stack.http.clone();
      let url = messages_url(&rig.stack, rig.channel);
      let body = serde_json::to_value(&request).unwrap();
      let token = token.clone();
      tokio::spawn(async move {
        let response = http
          .post(url)
          .bearer_auth(token)
          .json(&body)
          .send()
          .await
          .unwrap();
        let status = response.status();
        let body: serde_json::Value = response.json().await.unwrap_or_default();
        (status, body["id"].as_str().unwrap_or_default().to_owned())
      })
    });
    let mut answers = Vec::new();
    for attempt in attempts {
      answers.push(attempt.await.unwrap());
    }
    let created = answers
      .iter()
      .filter(|(status, _)| *status == StatusCode::CREATED)
      .count();
    let repeated = answers
      .iter()
      .filter(|(status, _)| *status == StatusCode::OK)
      .count();
    assert_eq!((created, repeated), (1, 9), "answers: {answers:?}");
    assert!(
      answers.iter().all(|(_, id)| *id == answers[0].1),
      "different IDs: {answers:?}"
    );
    let history = rig
      .stack
      .get(
        &rig.alice.tokens,
        &format!("/conversations/{}/messages?limit=50", rig.channel),
      )
      .await
      .expect(StatusCode::OK);
    let stored = history.body["messages"]
      .as_array()
      .unwrap()
      .iter()
      .filter(|message| message["client_message_id"] == serde_json::json!(request.client_message_id))
      .count();
    assert_eq!(stored, 1);
    rig.finish().await;
  }

  /// A late or replayed read position never moves it backwards.
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn a_late_read_position_does_not_move_it_back() {
    let Some(mut rig) = Rig::start(Stack::builder()).await else {
      return;
    };
    let ids = rig.post_messages(10, 64).await;
    let path = format!("/conversations/{}/read-position", rig.channel);
    rig
      .stack
      .put(
        &rig.carol.tokens,
        &path,
        serde_json::json!({"message_id": ids[6]}),
      )
      .await
      .expect(StatusCode::NO_CONTENT);
    // The same request again, then an older one arriving late.
    for id in [&ids[6], &ids[1]] {
      rig
        .stack
        .put(&rig.carol.tokens, &path, serde_json::json!({"message_id": id}))
        .await
        .expect(StatusCode::NO_CONTENT);
    }
    assert_eq!(unread(&rig.stack, &rig.carol.tokens, rig.channel).await, 3);
    rig.finish().await;
  }
}

mod database_outage {
  use super::*;

  /// Postgres unreachable for a few seconds: requests fail fast with 503, liveness stays up and
  /// readiness reports it, the process keeps running, open gateway connections survive, and
  /// everything works again once Postgres is back, without a restart.
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn a_short_postgres_outage_fails_fast_and_heals_without_a_restart() {
    let Some(mut rig) = Rig::start(Stack::builder().with_database_relay()).await else {
      return;
    };
    let carol_token = rig.carol.tokens.token_async().await;
    let mut raw = RawGateway::connect(&rig.stack).await;
    raw.identify(carol_token, rig.carol.client.device_id()).await;
    // Tokens are fetched before the outage: the identity provider is not what is tested here.
    let alice_token = rig.alice.tokens.token_async().await;

    rig.stack.database_relay().cut();
    let started = Instant::now();
    let send = send_raw(
      &rig.stack,
      &alice_token,
      rig.channel,
      rig.alice.client.device_id(),
      64,
    )
    .await;
    let failed_after = started.elapsed();
    eprintln!("PERF postgres down: send answered {send} after {failed_after:?}");
    // 503 tells clients (and their outboxes) to retry; it is not a server fault.
    assert_eq!(send, StatusCode::SERVICE_UNAVAILABLE, "a send without a database");
    assert!(
      failed_after < Duration::from_secs(10),
      "the request hung for {failed_after:?}"
    );
    let health = rig
      .stack
      .http
      .get(format!("{}/healthz", rig.stack.base_url))
      .send()
      .await
      .unwrap();
    assert_eq!(
      health.status(),
      StatusCode::OK,
      "liveness must not depend on Postgres"
    );
    let ready = rig
      .stack
      .http
      .get(format!("{}/readyz", rig.stack.base_url))
      .send()
      .await
      .unwrap();
    assert_eq!(ready.status(), StatusCode::SERVICE_UNAVAILABLE);
    raw.send(&GatewayClientMessage::Heartbeat).await;
    assert_eq!(raw.receive().await, Some(GatewayServerMessage::HeartbeatAck));
    assert!(
      rig.stack.server().is_running(),
      "the server exited without its database"
    );

    rig.stack.database_relay().restore();
    let deadline = Instant::now() + Duration::from_secs(30);
    let recovered_after = loop {
      let status = send_raw(
        &rig.stack,
        &alice_token,
        rig.channel,
        rig.alice.client.device_id(),
        64,
      )
      .await;
      if status == StatusCode::CREATED {
        break started.elapsed();
      }
      assert!(
        Instant::now() < deadline,
        "sends still fail ({status}) after Postgres came back"
      );
      tokio::time::sleep(Duration::from_millis(200)).await;
    };
    eprintln!("PERF postgres down: first successful send {recovered_after:?} after the cut");
    // The open connection gets the event of the send that worked.
    loop {
      match raw.receive().await {
        Some(GatewayServerMessage::Event { .. }) => break,
        Some(_) => {},
        None => panic!("the gateway connection did not survive the outage"),
      }
    }
    rig.finish().await;
  }

  /// A server that starts while Postgres is down waits for it instead of exiting (no crash loop
  /// under a restart policy, no ordering requirement on the deployment).
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn a_server_started_while_postgres_is_down_waits_for_it() {
    let Some(mut stack) = Stack::builder().with_database_relay().start().await else {
      return;
    };
    stack.server().terminate(Duration::from_secs(30)).await.unwrap();
    stack.database_relay().cut();
    stack.spawn_server().unwrap();
    let starting = stack.wait_for_server(Duration::from_secs(8)).await;
    assert!(
      starting.is_err(),
      "the server answered /healthz without a database"
    );
    assert!(
      stack.server().is_running(),
      "the server exited while Postgres was down:\n{}",
      stack.server().tail(10)
    );
    assert!(
      stack
        .server()
        .plain_log()
        .contains("the database is unreachable, retrying"),
      "no retry in the log:\n{}",
      stack.server().tail(10)
    );
    stack.database_relay().restore();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
      let healthy = stack
        .http
        .get(format!("{}/readyz", stack.base_url))
        .send()
        .await
        .is_ok_and(|response| response.status().is_success());
      if healthy {
        break;
      }
      assert!(
        Instant::now() < deadline,
        "the server did not come up after Postgres returned"
      );
      tokio::time::sleep(Duration::from_millis(250)).await;
    }
    stack.stop().await;
  }
}

mod slow_consumer {
  use super::*;

  const MESSAGES: usize = 3000;
  const MESSAGE_BYTES: usize = 16 * 1024;

  /// A client that stops reading (a frozen tab, a stalled proxy): the server's queue for it is
  /// bounded, it drops the connection instead of blocking or growing, and the other connections
  /// keep getting every event.
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn a_consumer_that_stops_reading_is_dropped_and_memory_stays_bounded() {
    let builder = Stack::builder().with_env("TEAMCRAFT_SECURITY__RATE_LIMITS__IS_ENABLED", "false");
    let Some(mut rig) = Rig::start(builder).await else {
      return;
    };
    let carol_token = rig.carol.tokens.token_async().await;
    let carol_device = rig.carol.client.device_id();
    let mut stalled = RawGateway::connect(&rig.stack).await;
    stalled.identify(carol_token.clone(), carol_device).await;
    // A healthy consumer of the same events, reading all the time.
    let mut healthy = RawGateway::connect(&rig.stack).await;
    healthy.identify(carol_token, carol_device).await;
    let healthy_reader = tokio::spawn(async move { healthy.drain(Duration::from_secs(120)).await });

    let before = rig.stats();
    let started = Instant::now();
    rig.post_messages(MESSAGES, MESSAGE_BYTES).await;
    let elapsed = started.elapsed();
    let after = rig.stats();
    eprintln!(
      "PERF slow consumer: {MESSAGES} x {} KiB in {elapsed:?}; RSS {:.0} -> {:.0} MiB, peak {:.0} MiB",
      MESSAGE_BYTES / 1024,
      before.rss_mib(),
      after.rss_mib(),
      after.peak_rss_bytes as f64 / (1024.0 * 1024.0)
    );

    // What the stalled connection still gets once it reads again: at most the queue, then the end.
    let (queued, ending) = stalled.drain(Duration::from_secs(30)).await;
    eprintln!("PERF slow consumer: the stalled connection still delivered {queued} events, then {ending:?}");
    assert!(
      ending.is_some(),
      "the stalled connection is still open after the flood"
    );
    assert!(queued < MESSAGES, "the stalled connection was never cut off");

    rig.post_messages(1, 64).await;
    let (received, _) = healthy_reader.await.unwrap();
    assert!(
      received > MESSAGES,
      "the healthy consumer got {received} of {} events",
      MESSAGES + 1
    );
    // Bounded: the stalled connection's queue holds at most 1024 events; with the resume buffer
    // (1000 per session) and the allocator's slack that is well below 1 GiB at 16 KiB each.
    assert!(
      after.peak_rss_bytes < 1024 * 1024 * 1024,
      "peak RSS {} MiB",
      after.peak_rss_bytes / (1024 * 1024)
    );
    rig.finish().await;
  }

  /// A client that never reads again, not even after the flood: the server must not keep its
  /// connection (and the queued frames) for as long as TCP would.
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn a_consumer_that_never_reads_again_is_disconnected() {
    let builder = Stack::builder().with_env("TEAMCRAFT_SECURITY__RATE_LIMITS__IS_ENABLED", "false");
    let Some(mut rig) = Rig::start(builder).await else {
      return;
    };
    let server_port: u16 = rig.stack.base_url.rsplit(':').next().unwrap().parse().unwrap();
    let carol_token = rig.carol.tokens.token_async().await;
    let mut stalled = RawGateway::connect(&rig.stack).await;
    stalled.identify(carol_token, rig.carol.client.device_id()).await;
    let client_port = stalled.local_port();
    assert!(is_tcp_established(server_port, client_port));

    // Enough to fill the socket buffers on both sides and the connection's queue.
    rig.post_messages(1500, 16 * 1024).await;
    let started = Instant::now();
    let deadline = started + Duration::from_secs(90);
    while is_tcp_established(server_port, client_port) {
      assert!(
        Instant::now() < deadline,
        "the server still holds the connection of a client that stopped reading"
      );
      tokio::time::sleep(Duration::from_millis(500)).await;
    }
    eprintln!(
      "PERF slow consumer: the server let go of a connection that stopped reading {:?} after the flood",
      started.elapsed()
    );
    drop(stalled);
    rig.finish().await;
  }
}

mod resources {
  use super::*;

  const CONNECTIONS: usize = 200;
  const IDLE: Duration = Duration::from_secs(30);
  const LOAD_MESSAGES: usize = 2000;
  /// What the clients use (the server's 30 s, capped at 25 s for proxies).
  const HEARTBEAT_EVERY: Duration = Duration::from_secs(25);

  /// Memory and CPU of the server process: idle with many connections, then under a burst of
  /// sends that every connection receives. Prints the numbers for docs/performance.md and checks
  /// generous bounds.
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  #[ignore = "measurement, about a minute; run with --ignored --nocapture"]
  async fn server_memory_and_cpu_idle_and_under_load() {
    let builder = Stack::builder()
      .with_env("TEAMCRAFT_SECURITY__RATE_LIMITS__IS_ENABLED", "false")
      .with_env("TEAMCRAFT_GATEWAY__MAX_SESSIONS_PER_DEVICE", "300")
      .with_env("TEAMCRAFT_GATEWAY__MAX_SESSIONS_PER_USER", "300")
      .with_env("TEAMCRAFT_LOGGING__FILTER", "warn");
    let Some(mut rig) = Rig::start(builder).await else {
      return;
    };
    let fresh = rig.stats();
    let carol_token = rig.carol.tokens.token_async().await;
    let carol_device = rig.carol.client.device_id();
    let mut readers = Vec::new();
    for _ in 0..CONNECTIONS {
      let mut gateway = RawGateway::connect(&rig.stack).await;
      gateway.identify(carol_token.clone(), carol_device).await;
      readers.push(tokio::spawn(async move { keep_reading(gateway).await }));
    }
    let connected = rig.stats();
    tokio::time::sleep(IDLE).await;
    let idle = rig.stats();
    let idle_cpu = idle.cpu_percent_since(&connected);

    let started = Instant::now();
    rig.post_messages(LOAD_MESSAGES, 1024).await;
    let elapsed = started.elapsed();
    let loaded = rig.stats();
    let load_cpu = loaded.cpu_percent_since(&idle);
    let mut delivered = 0;
    for reader in readers {
      delivered += reader.await.unwrap();
    }
    eprintln!(
      "PERF server process: fresh {:.0} MiB; {CONNECTIONS} connections {:.0} MiB; idle {IDLE:?}: \
       {idle_cpu:.2} % CPU, {:.0} MiB",
      fresh.rss_mib(),
      connected.rss_mib(),
      idle.rss_mib(),
    );
    eprintln!(
      "PERF server process: {LOAD_MESSAGES} sends of 1 KiB to {CONNECTIONS} connections in {elapsed:?} \
       ({:.0}/s), {load_cpu:.0} % CPU, RSS {:.0} MiB, peak {:.0} MiB, {} threads, {delivered} deliveries",
      LOAD_MESSAGES as f64 / elapsed.as_secs_f64(),
      loaded.rss_mib(),
      loaded.peak_rss_bytes as f64 / (1024.0 * 1024.0),
      loaded.threads,
    );
    assert!(idle_cpu < 5.0, "idle CPU {idle_cpu:.2} %");
    assert!(loaded.peak_rss_bytes < 1024 * 1024 * 1024, "peak RSS");
    rig.finish().await;
  }

  /// Reads until the connection ends or the events stopped coming; returns their number. Sends a
  /// heartbeat every 25 s like the real clients do.
  async fn keep_reading(mut gateway: RawGateway) -> usize {
    let mut events = 0;
    loop {
      let (count, ending) = gateway.drain(HEARTBEAT_EVERY).await;
      events += count;
      if ending.is_some() || (count == 0 && events > 0) {
        return events;
      }
      gateway.send(&GatewayClientMessage::Heartbeat).await;
    }
  }
}

/// Two enrolled users in one channel, on a server built from `builder` with a small resume buffer
/// and window.
struct Rig {
  stack: Stack,
  alice: Peer,
  carol: Peer,
  channel: ConversationId,
}

impl Rig {
  async fn start(builder: teamcraft_e2e::StackBuilder) -> Option<Self> {
    let stack = builder
      .with_env(
        "TEAMCRAFT_GATEWAY__RESUME_BUFFER_EVENTS",
        &RESUME_BUFFER_EVENTS.to_string(),
      )
      .with_env(
        "TEAMCRAFT_GATEWAY__RESUME_WINDOW_SECS",
        &RESUME_WINDOW_SECS.to_string(),
      )
      .start()
      .await?;
    let mut alice = Peer::enroll(&stack, "alice").await;
    let mut carol = Peer::enroll(&stack, "carol").await;
    let space = space_with(&stack, &alice, "Robustness", &[(&carol, "member")]).await;
    let channel = channel_with(&mut alice, space, "robustness", &mut [&mut carol]).await;
    Some(Self {
      stack,
      alice,
      carol,
      channel,
    })
  }

  /// Posts `count` messages of `bytes` opaque ciphertext from Alice's device, eight at a time.
  /// The server never decrypts, so random bytes are as good as real ciphertext here. Returns the
  /// message IDs in the server's order.
  async fn post_messages(&mut self, count: usize, bytes: usize) -> Vec<String> {
    let token = self.alice.tokens.token_async().await;
    let device = self.alice.client.device_id();
    let url = messages_url(&self.stack, self.channel);
    let mut ids = Vec::with_capacity(count);
    for chunk_start in (0..count).step_by(8) {
      let sends = (chunk_start..count.min(chunk_start + 8)).map(|_| {
        let http = self.stack.http.clone();
        let url = url.clone();
        let token = token.clone();
        let body = serde_json::to_value(message_request(device, bytes)).unwrap();
        tokio::spawn(async move {
          let response = http
            .post(url)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .unwrap();
          assert_eq!(response.status(), StatusCode::CREATED);
          let body: serde_json::Value = response.json().await.unwrap();
          body["id"].as_str().unwrap().to_owned()
        })
      });
      let sends: Vec<_> = sends.collect();
      for send in sends {
        ids.push(send.await.unwrap());
      }
    }
    ids.sort_by_key(|id| id.parse::<u64>().expect("snowflake ID"));
    ids
  }

  fn stats(&mut self) -> ProcessStats {
    self.stack.server().stats().expect("server process stats (/proc)")
  }

  async fn finish(self) {
    self.stack.stop().await;
  }
}

fn message_request(device_id: DeviceId, bytes: usize) -> SendMessageRequest {
  SendMessageRequest {
    client_message_id: ClientMessageId::from_bytes(rand_bytes()),
    device_id,
    content_key_id: Base64Bytes::from(b"key".to_vec()),
    ciphertext: Base64Bytes::from(vec![0xab; bytes]),
    maybe_thread_id: None,
    mentioned_user_ids: Vec::new(),
    is_urgent: false,
  }
}

fn messages_url(stack: &Stack, channel: ConversationId) -> String {
  format!("{}/api/v1/conversations/{channel}/messages", stack.base_url)
}

async fn send_raw(
  stack: &Stack,
  token: &str,
  channel: ConversationId,
  device: DeviceId,
  bytes: usize,
) -> StatusCode {
  stack
    .http
    .post(messages_url(stack, channel))
    .bearer_auth(token)
    .json(&message_request(device, bytes))
    .timeout(Duration::from_secs(60))
    .send()
    .await
    .map_or(StatusCode::GATEWAY_TIMEOUT, |response| response.status())
}

async fn unread(stack: &Stack, user: &UserTokens, channel: ConversationId) -> u64 {
  let reply = stack.get(user, "/unread").await.expect(StatusCode::OK);
  reply.body["conversations"]
    .as_array()
    .unwrap()
    .iter()
    .find(|entry| entry["conversation_id"] == serde_json::json!(channel))
    .map_or(0, |entry| entry["unread_count"].as_u64().unwrap())
}

fn rand_bytes() -> [u8; 16] {
  use rand_core::RngCore;
  let mut bytes = [0; 16];
  rand_core::OsRng.fill_bytes(&mut bytes);
  bytes
}
