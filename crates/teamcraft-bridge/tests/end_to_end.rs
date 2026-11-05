//! A webhook POST arrives as a decrypted message for a member client, through a real server and
//! Postgres (the server's testing harness, needs `DATABASE_URL`; skipped without).
//!
//! alice (owner, the channel's admin) and bob (member) share an integration channel with the
//! bridge's bot. The bridge runs next to the test; bob reads what the tools sent after decrypting
//! it himself. Sources come from the configuration file, or from the app: control messages that
//! alice's and bob's clients write into the channel.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use rand_core::OsRng;
use serde_json::json;
use teamcraft_bridge::app::{Endpoints, enroll_or_open, start_endpoints, worker_settings};
use teamcraft_bridge::config::BridgeConfig;
use teamcraft_bridge::sources::SourceRegistry;
use teamcraft_bridge::worker::Worker;
use teamcraft_core::api::{HttpGateway, HttpServerApi, StaticToken};
use teamcraft_core::clock::SystemClock;
use teamcraft_core::integration_sources::{is_known_bot, shown_source};
use teamcraft_core::store::{LocalStore, MemoryStore, MessageContent, MessageFilter};
use teamcraft_core::workspace::RosterRole;
use teamcraft_core::{Client, ClientSettings, EnrollmentOptions, EnrollmentSecret, PendingEnrollment};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_crypto::secret::SecretBytes;
use teamcraft_crypto::source_token::{SourceToken, source_picture_sha256};
use teamcraft_proto::{
  Base64Bytes, ConversationId, SourceChange, SourceId, SourceIdentity, SourceKind, SourceOptions,
  SourcePictureHash, SourceTokenHash, UserId,
};
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
    secret: EnrollmentSecret::Passphrase(SecretBytes::new(PASSPHRASE.as_bytes().to_vec())),
    argon2: weak_argon2(),
    maybe_attestation: None,
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
      MessageContent::Text { body, mentions, .. } => Some((body, mentions)),
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

/// A JSON webhook with an `Idempotency-Key`: the status, and whether the bridge answered from
/// memory.
async fn post_with_key(url: &str, key: &str, body: &str) -> (reqwest::StatusCode, bool) {
  let response = reqwest::Client::new()
    .post(url)
    .header("content-type", "application/json")
    .header("idempotency-key", key)
    .body(body.to_owned())
    .send()
    .await
    .unwrap();
  let is_replayed = response.headers().contains_key("idempotent-replayed");
  (response.status(), is_replayed)
}

/// Everything the tests share: the server, alice and bob with their gateways, the channel and the
/// bot's enrolled client.
struct Setup {
  app: TestApp,
  alice_user: TestUser,
  alice: TestClient,
  bob: TestClient,
  bob_store: Arc<MemoryStore>,
  bob_gateway: HttpGateway,
  bot_id: String,
  bot_token: String,
  bot_store: Arc<MemoryStore>,
  channel: ConversationId,
}

async fn setup(app: TestApp) -> Setup {
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

  // The bot enrolls; then alice adds bob and the bot to the channel.
  let bot_store = Arc::new(MemoryStore::new());
  let config = bridge_config(&app, &bot_token, channel, &bob_id, true);
  let bot_api = HttpServerApi::new(app.base_url.clone(), StaticToken(bot_token.clone()));
  let bot_client = enroll_or_open(bot_api, bot_store.clone(), &config).await.unwrap();
  assert_eq!(bot_client.user_id().to_string(), bot_id);
  alice.add_member(channel, bob.user_id()).await.unwrap();
  alice.add_member(channel, bot_client.user_id()).await.unwrap();
  pump_until("bob joins", &mut bob, &mut bob_gateway, |client| {
    client.has_group(channel).unwrap()
  })
  .await;
  drop(bot_client);
  Setup {
    app,
    alice_user,
    alice,
    bob,
    bob_store,
    bob_gateway,
    bot_id,
    bot_token,
    bot_store,
    channel,
  }
}

/// The bridge's configuration; `with_static_source` adds a `[[sources]]` block for the channel.
fn bridge_config(
  app: &TestApp,
  bot_token: &str,
  channel: ConversationId,
  bob_id: &str,
  with_static_source: bool,
) -> BridgeConfig {
  let source = if with_static_source {
    format!(
      r#"
      [[sources]]
      name = "nas"
      token = "{SOURCE_TOKEN}"
      conversation_id = "{channel}"
      danger_is_alert = true
      mentions = [{{ user_id = "{bob_id}", name = "bob" }}]
      "#
    )
  } else {
    String::new()
  };
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
    [files]
    spool_dir = "{spool}"
    {source}
    "#,
    url = app.base_url,
    spool = std::env::temp_dir()
      .join(format!("teamcraft-bridge-e2e-{}", std::process::id()))
      .display()
  ))
  .unwrap();
  config.identity.argon2_memory_kib = weak_argon2().memory_kib;
  config.identity.argon2_iterations = weak_argon2().iterations;
  config
}

/// Opens the bot's client from its store and starts endpoints and worker: what a (re)start of the
/// bridge does.
async fn start_bridge(
  setup: &Setup,
  config: &BridgeConfig,
) -> (
  Endpoints,
  String,
  Worker<HttpServerApi<StaticToken>, Arc<MemoryStore>>,
) {
  let api = HttpServerApi::new(setup.app.base_url.clone(), StaticToken(setup.bot_token.clone()));
  let client = enroll_or_open(api, setup.bot_store.clone(), config)
    .await
    .unwrap();
  let (sender, receiver) = mpsc::channel(64);
  let sources = SourceRegistry::new(config);
  let endpoints = start_endpoints(config, sources.clone(), sender, None)
    .await
    .unwrap();
  let base = format!("http://{}", endpoints.addrs[0]);
  let worker = Worker::new(client, sources, receiver, worker_settings(config));
  (endpoints, base, worker)
}

#[tokio::test]
async fn webhooks_and_logs_arrive_decrypted_for_a_member_and_the_server_sees_only_ciphertext() {
  let Some(app) = TestApp::start().await else { return };
  let mut setup = setup(app).await;
  let (channel, bot_id) = (setup.channel, setup.bot_id.clone());
  let bob_id = setup.bob.user_id().to_string();
  let config = bridge_config(&setup.app, &setup.bot_token, channel, &bob_id, true);
  let (endpoints, base, worker) = start_bridge(&setup, &config).await;
  let Setup {
    app,
    alice_user,
    bob,
    bob_store,
    bob_gateway,
    ..
  } = &mut setup;

  let script = async {
    // A Slack-style webhook, and the producer's retry with the same Idempotency-Key, as after an
    // answer lost on the way: answered from memory and posted once (counted below).
    let deploy = r#"{"text":"Deploy <https://ci.example.com/42|#42> finished","username":"ci"}"#;
    let hook = format!("{base}/hooks/{SOURCE_TOKEN}");
    assert_eq!(
      post_with_key(&hook, "deploy-42", deploy).await,
      (reqwest::StatusCode::OK, false)
    );
    assert_eq!(
      post_with_key(&hook, "deploy-42", deploy).await,
      (reqwest::StatusCode::OK, true)
    );
    pump_until("bob decrypts the webhook", bob, bob_gateway, |_| {
      texts(bob_store, channel)
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
    pump_until("bob decrypts the alert", bob, bob_gateway, |_| {
      texts(bob_store, channel)
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
    pump_until("bob decrypts the log batch", bob, bob_gateway, |_| {
      texts(bob_store, channel)
        .iter()
        .any(|(body, _)| body == "```\nINFO started\nWARN disk at 91%\nINFO done\n```")
    })
    .await;

    // The server holds ciphertext only.
    let stored = app
      .get(alice_user, &format!("/conversations/{channel}/messages"))
      .await;
    let raw = stored.body.to_string();
    for secret in ["Deploy", "Backup failed", "disk at 91%"] {
      assert!(!raw.contains(secret), "{secret} leaked");
    }
    // Besides the bot's three messages, the channel holds the encrypted system messages the
    // members' clients post (channel created, member added).
    let from_bot: Vec<_> = stored.body["messages"]
      .as_array()
      .unwrap()
      .iter()
      .filter(|message| message["sender_user_id"] == bot_id.as_str())
      .collect();
    assert_eq!(from_bot.len(), 3, "the retried webhook is in it once");

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
  setup.app.stop().await;
}

#[tokio::test]
async fn sources_added_in_the_app_work_without_configuration_and_survive_a_restart() {
  let Some(app) = TestApp::start().await else { return };
  let mut setup = setup(app).await;
  let channel = setup.channel;
  let bob_id = setup.bob.user_id().to_string();
  let config = bridge_config(&setup.app, &setup.bot_token, channel, &bob_id, false);
  assert!(config.sources.is_empty(), "no source in the file");
  // alice signs the workspace roster as its first owner: only roster admins manage sources.
  let alice_id = setup.alice.user_id();
  setup
    .alice
    .publish_roster(&[(alice_id, RosterRole::Owner)])
    .await
    .unwrap();

  // bob is a member, but not in the roster: his source must not open.
  let bobs_token = SourceToken::generate(&mut OsRng);
  let (_, bobs_change) = created("bob's", SourceKind::Webhook, &bobs_token);
  setup.bob.send_source_change(channel, bobs_change).await.unwrap();
  // alice is a roster admin: hers opens, and bob can't revoke it.
  let token = SourceToken::generate(&mut OsRng);
  let (source, change) = created("ci", SourceKind::Webhook, &token);
  setup.alice.send_source_change(channel, change).await.unwrap();
  setup
    .bob
    .send_source_change(channel, SourceChange::Revoked { source })
    .await
    .unwrap();

  let (endpoints, base, worker) = start_bridge(&setup, &config).await;
  let hook = format!("{base}/hooks/{}", token.expose_secret());
  let script = async {
    wait_for_status("alice's source opens", &hook, reqwest::StatusCode::OK).await;
    let bobs_hook = format!("{base}/hooks/{}", bobs_token.expose_secret());
    assert_eq!(
      post(&bobs_hook, "application/json", r#"{"text":"x"}"#).await,
      reqwest::StatusCode::NOT_FOUND,
      "a member's source stays closed"
    );
    let ingest = format!("{base}/ingest/{}", token.expose_secret());
    assert_eq!(
      post(&ingest, "text/plain", "x\n").await,
      reqwest::StatusCode::NOT_FOUND,
      "a webhook source takes no log lines"
    );
    let sent = post(
      &hook,
      "application/json",
      r#"{"text":"from the app","username":"ci"}"#,
    )
    .await;
    assert_eq!(sent, reqwest::StatusCode::OK);
    pump_until(
      "bob decrypts the webhook",
      &mut setup.bob,
      &mut setup.bob_gateway,
      |_| {
        texts(&setup.bob_store, channel)
          .iter()
          .any(|(body, _)| body == "**ci**\nfrom the app")
      },
    )
    .await;
    // The message states its source, under the name its owner gave it: a webhook's `username`
    // stays in the body and changes nothing (docs/bridge.md, "Source identity").
    let message = setup
      .bob_store
      .messages(channel, MessageFilter::TopLevel)
      .unwrap()
      .into_iter()
      .find(|message| matches!(&message.content, MessageContent::Text { body, .. } if body.ends_with("from the app")))
      .unwrap();
    let MessageContent::Text { meta, .. } = &message.content else {
      unreachable!("found by its text")
    };
    let identity = meta.maybe_source.as_deref().unwrap();
    assert_eq!(identity.maybe_id, Some(source));
    assert_eq!(identity.name, "ci");
    assert_eq!(
      identity.maybe_picture_sha256, None,
      "no picture set yet: the bot's shows"
    );
    let bot = message.sender_user_id;
    assert!(is_known_bot(setup.bob_store.as_ref(), bot).unwrap());

    // alice renames the source and gives it a picture on her device: bob, another member, sees
    // both on the next message, not only alice (issue #1299).
    let picture = small_png();
    let picture_sha256 = SourcePictureHash::from_bytes(source_picture_sha256(&picture));
    setup
      .alice
      .send_source_change(
        channel,
        SourceChange::Renamed {
          source,
          name: "Deploys".into(),
        },
      )
      .await
      .unwrap();
    setup
      .alice
      .send_source_change(
        channel,
        SourceChange::PictureChanged {
          source,
          maybe_picture: Some(Base64Bytes::new(picture.clone())),
        },
      )
      .await
      .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let renamed = loop {
      assert!(
        Instant::now() < deadline,
        "timed out waiting for the renamed source"
      );
      let sent = post(&hook, "application/json", r#"{"text":"after the rename"}"#).await;
      assert_eq!(sent, reqwest::StatusCode::OK);
      tokio::time::sleep(Duration::from_millis(500)).await;
      setup.bob.pump(&mut setup.bob_gateway).await.unwrap();
      let maybe_renamed = source_identities(&setup.bob_store, channel)
        .into_iter()
        .find(|(_, identity)| identity.name == "Deploys" && identity.maybe_picture_sha256.is_some());
      if let Some(found) = maybe_renamed {
        break found;
      }
    };
    let (sender, identity) = renamed;
    assert_eq!(sender, bot);
    assert_eq!(identity.maybe_id, Some(source));
    assert_eq!(identity.maybe_picture_sha256, Some(picture_sha256));
    assert_eq!(
      identity.maybe_picture, None,
      "a device keeps only the hash in the message"
    );
    let shown = shown_source(&identity, is_known_bot(setup.bob_store.as_ref(), sender).unwrap()).unwrap();
    assert_eq!(shown.name, "Deploys");
    assert_eq!(
      setup
        .bob
        .integration_source_picture(channel, picture_sha256)
        .unwrap(),
      Some(picture),
      "the member's device holds the picture the message names"
    );
    endpoints.shutdown().await;
  };
  let (client, ()) = tokio::join!(worker.run(), script);
  drop(client);

  // A restart (a new client on the same database) serves the source from the store at once.
  let (endpoints, base, worker) = start_bridge(&setup, &config).await;
  let hook = format!("{base}/hooks/{}", token.expose_secret());
  let script = async {
    assert_eq!(
      post(&hook, "application/json", r#"{"text":"after restart"}"#).await,
      reqwest::StatusCode::OK,
      "known from the store before the gateway is back"
    );
    // alice revokes it: the token stops working.
    setup
      .alice
      .send_source_change(channel, SourceChange::Revoked { source })
      .await
      .unwrap();
    wait_for_status("the revoked token closes", &hook, reqwest::StatusCode::NOT_FOUND).await;
    endpoints.shutdown().await;
  };
  let _ = tokio::join!(worker.run(), script);
  setup.app.stop().await;
}

/// The sender and source identity of every top-level text message that states one.
fn source_identities(store: &MemoryStore, conversation: ConversationId) -> Vec<(UserId, SourceIdentity)> {
  store
    .messages(conversation, MessageFilter::TopLevel)
    .unwrap()
    .into_iter()
    .filter_map(|message| match message.content {
      MessageContent::Text { meta, .. } => meta
        .maybe_source
        .map(|identity| (message.sender_user_id, *identity)),
      _ => None,
    })
    .collect()
}

/// A valid PNG header (8 × 8 pixels) and an empty image chunk: all a source picture is checked for.
fn small_png() -> Vec<u8> {
  let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];
  bytes.extend(*b"IHDR");
  bytes.extend(8_u32.to_be_bytes());
  bytes.extend(8_u32.to_be_bytes());
  bytes.extend([8, 6, 0, 0, 0]);
  bytes.extend([0; 4]);
  bytes.extend(0_u32.to_be_bytes());
  bytes.extend(*b"IDAT");
  bytes.extend([0; 4]);
  bytes
}

fn created(name: &str, kind: SourceKind, token: &SourceToken) -> (SourceId, SourceChange) {
  let source = SourceId::random();
  (
    source,
    SourceChange::Created {
      source,
      name: name.into(),
      kind,
      token_sha256: SourceTokenHash::from_bytes(token.sha256()),
      options: SourceOptions::default(),
    },
  )
}

/// Posts a small webhook until the answer is `wanted` (the bridge picks changes up once a second).
async fn wait_for_status(what: &str, url: &str, wanted: reqwest::StatusCode) {
  let deadline = Instant::now() + Duration::from_secs(30);
  while Instant::now() < deadline {
    if post(url, "application/json", r#"{"text":"probe"}"#).await == wanted {
      return;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
  panic!("timed out waiting until {what}");
}
