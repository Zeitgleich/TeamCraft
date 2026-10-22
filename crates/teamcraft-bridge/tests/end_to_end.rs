//! A webhook POST arrives as a decrypted message for a member client, through a real server and
//! Postgres (the server's testing harness, needs `DATABASE_URL`; skipped without).
//!
//! alice (owner) and bob (member) share an integration channel with the bridge's bot. The bridge
//! runs next to the test; bob reads what the tools sent after decrypting it himself.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use teamcraft_bridge::app::{enroll_or_open, start_endpoints, worker_parts};
use teamcraft_bridge::config::BridgeConfig;
use teamcraft_bridge::worker::Worker;
use teamcraft_core::api::{HttpGateway, HttpServerApi, StaticToken};
use teamcraft_core::clock::SystemClock;
use teamcraft_core::store::{LocalStore, MemoryStore, MessageContent, MessageFilter};
use teamcraft_core::{Client, ClientSettings, EnrollmentOptions, PendingEnrollment};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_crypto::secret::SecretBytes;
use teamcraft_proto::{ConversationId, UserId};
use teamcraft_server::testing::{TestApp, TestUser};
use tokio::sync::mpsc;

type TestClient = Client<HttpServerApi<StaticToken>, Arc<MemoryStore>>;

const SOURCE_TOKEN: &str = "e2e-source-token-0123456789";
const PASSPHRASE: &str = "bridge test passphrase";

fn weak_argon2() -> Argon2Settings {
  Argon2Settings {
    memory_kib: 1024,
    iterations: 1,
    parallelism: 1,
  }
}

async fn enroll(app: &TestApp, token: String, device: &str) -> (TestClient, Arc<MemoryStore>) {
  let store = Arc::new(MemoryStore::new());
  let api = HttpServerApi::new(app.base_url.clone(), StaticToken(token));
  let options = EnrollmentOptions {
    device_name: device.into(),
    passphrase: SecretBytes::new(PASSPHRASE.as_bytes().to_vec()),
    argon2: weak_argon2(),
    maybe_attestation: None,
    maybe_org_recovery: None,
    maybe_linked: None,
  };
  let client = PendingEnrollment::new()
    .complete(
      api,
      store.clone(),
      Box::new(SystemClock),
      ClientSettings::default(),
      options,
    )
    .await
    .unwrap();
  (client, store)
}

fn texts(store: &MemoryStore, conversation: ConversationId) -> Vec<(String, Vec<UserId>)> {
  store
    .messages(conversation, MessageFilter::TopLevel)
    .unwrap()
    .into_iter()
    .filter_map(|message| match message.content {
      MessageContent::Text { body, mentions } => Some((body, mentions)),
      _ => None,
    })
    .collect()
}

/// Applies gateway frames until `done`, with a deadline (real network delivery takes a moment).
async fn pump_until(
  what: &str,
  client: &mut TestClient,
  gateway: &mut HttpGateway,
  mut done: impl FnMut(&mut TestClient) -> bool,
) {
  let deadline = Instant::now() + Duration::from_secs(30);
  while Instant::now() < deadline {
    client.pump(gateway).await.unwrap();
    if done(client) {
      return;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
  panic!("timed out waiting until {what}");
}

async fn post(url: &str, content_type: &str, body: &str) -> reqwest::StatusCode {
  reqwest::Client::new()
    .post(url)
    .header("content-type", content_type)
    .body(body.to_owned())
    .send()
    .await
    .unwrap()
    .status()
}

#[tokio::test]
async fn webhooks_and_logs_arrive_decrypted_for_a_member_and_the_server_sees_only_ciphertext() {
  let Some(app) = TestApp::start().await else { return };
  let (alice_user, bob_user) = (
    TestUser::new("alice", &["owner"]),
    TestUser::new("bob", &["member"]),
  );
  let (mut alice, _) = enroll(&app, app.token(&alice_user), "alice laptop").await;
  let (mut bob, bob_store) = enroll(&app, app.token(&bob_user), "bob laptop").await;
  let (mut bob_gateway, _) = bob.connect_gateway().await.unwrap();

  // An admin registers the bridge; the bot enrolls its own device like any client.
  let registered = app
    .post(&alice_user, "/admin/bots", json!({"name": "NAS bridge"}))
    .await;
  assert_eq!(
    registered.status,
    reqwest::StatusCode::CREATED,
    "{}",
    registered.body
  );
  let bot_token = registered.body["token"].as_str().unwrap().to_owned();
  let bot_id = registered.body["bot"]["id"].as_str().unwrap().to_owned();

  // alice founds the integration channel and adds bob and the bot to the space.
  let space = app
    .post(
      &alice_user,
      "/spaces",
      json!({"name": "Ops", "join_policy": "invite_only"}),
    )
    .await
    .id();
  let channel = alice
    .create_channel(space.parse().unwrap(), "nas-logs")
    .await
    .unwrap();
  let flagged = app
    .patch(
      &alice_user,
      &format!("/conversations/{channel}"),
      json!({"is_integration": true}),
    )
    .await;
  assert_eq!(flagged.status, reqwest::StatusCode::OK, "{}", flagged.body);
  let bob_id = bob.user_id().to_string();
  for (user, role) in [(&bob_id, "member"), (&bot_id, "guest")] {
    let added = app
      .post(
        &alice_user,
        &format!("/spaces/{space}/members"),
        json!({"user_id": user, "role": role}),
      )
      .await;
    assert_eq!(added.status, reqwest::StatusCode::CREATED, "{}", added.body);
  }

  // The bridge: configured like a real one, started against the test server.
  let mut config = BridgeConfig::from_toml(&format!(
    r#"
    [server]
    url = "{url}"
    bot_token = "{bot_token}"
    [store]
    path = "unused-in-this-test.db"
    [identity]
    passphrase = "{PASSPHRASE}"
    [listen]
    addrs = ["127.0.0.1:0"]
    [batching]
    interval_ms = 200
    [index]
    interval_secs = 0
    [[sources]]
    name = "nas"
    token = "{SOURCE_TOKEN}"
    conversation_id = "{channel}"
    danger_is_alert = true
    mentions = [{{ user_id = "{bob_id}", name = "bob" }}]
    "#,
    url = app.base_url
  ))
  .unwrap();
  config.identity.argon2_memory_kib = weak_argon2().memory_kib;
  config.identity.argon2_iterations = weak_argon2().iterations;
  let bot_api = HttpServerApi::new(app.base_url.clone(), StaticToken(bot_token));
  let bot_client = enroll_or_open(bot_api, Arc::new(MemoryStore::new()), &config)
    .await
    .unwrap();
  assert_eq!(bot_client.user_id().to_string(), bot_id);

  // Now the bot has keys: alice adds bob and the bot to the channel.
  alice.add_member(channel, bob.user_id()).await.unwrap();
  alice.add_member(channel, bot_client.user_id()).await.unwrap();
  pump_until("bob joins", &mut bob, &mut bob_gateway, |client| {
    client.has_group(channel).unwrap()
  })
  .await;

  let (sender, receiver) = mpsc::channel(64);
  let endpoints = start_endpoints(&config, sender).await.unwrap();
  let base = format!("http://{}", endpoints.addrs[0]);
  let (sources, settings) = worker_parts(&config);
  let worker = Worker::new(bot_client, sources, receiver, settings);

  let script = async {
    // A Slack-style webhook.
    let status = post(
      &format!("{base}/hooks/{SOURCE_TOKEN}"),
      "application/json",
      r#"{"text":"Deploy <https://ci.example.com/42|#42> finished","username":"ci"}"#,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    pump_until("bob decrypts the webhook", &mut bob, &mut bob_gateway, |_| {
      texts(&bob_store, channel)
        .iter()
        .any(|(body, _)| body == "**ci**\nDeploy [#42](https://ci.example.com/42) finished")
    })
    .await;

    // A danger alert mentions bob (his ID is visible to the server, the text is not).
    let status = post(
      &format!("{base}/hooks/{SOURCE_TOKEN}"),
      "application/json",
      r#"{"attachments":[{"color":"danger","title":"Backup failed","text":"nas is unreachable"}]}"#,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let bob_user_id = bob.user_id();
    pump_until("bob decrypts the alert", &mut bob, &mut bob_gateway, |_| {
      texts(&bob_store, channel)
        .iter()
        .any(|(body, mentions)| body.contains("Backup failed") && mentions == &vec![bob_user_id])
    })
    .await;

    // Log lines arrive as one batch in a code block.
    let status = post(
      &format!("{base}/ingest/{SOURCE_TOKEN}"),
      "text/plain",
      "INFO started\nWARN disk at 91%\nINFO done\n",
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    pump_until("bob decrypts the log batch", &mut bob, &mut bob_gateway, |_| {
      texts(&bob_store, channel)
        .iter()
        .any(|(body, _)| body == "```\nINFO started\nWARN disk at 91%\nINFO done\n```")
    })
    .await;

    // The server holds ciphertext only.
    let stored = app
      .get(&alice_user, &format!("/conversations/{channel}/messages"))
      .await;
    let raw = stored.body.to_string();
    for secret in ["Deploy", "Backup failed", "disk at 91%"] {
      assert!(!raw.contains(secret), "{secret} leaked");
    }
    let from_bot = stored.body["messages"].as_array().unwrap();
    assert_eq!(from_bot.len(), 3);
    assert!(
      from_bot
        .iter()
        .all(|message| message["sender_user_id"] == bot_id.as_str())
    );

    // Bad input is refused without reaching the channel.
    assert_eq!(
      post(&format!("{base}/hooks/{SOURCE_TOKEN}"), "application/json", "{}").await,
      reqwest::StatusCode::BAD_REQUEST
    );
    assert_eq!(
      post(
        &format!("{base}/hooks/wrong"),
        "application/json",
        r#"{"text":"x"}"#
      )
      .await,
      reqwest::StatusCode::NOT_FOUND
    );
    endpoints.shutdown().await;
  };
  let (_, ()) = tokio::join!(worker.run(), script);
  app.stop().await;
}
