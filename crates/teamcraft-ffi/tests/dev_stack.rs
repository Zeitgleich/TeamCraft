//! A mobile client (through the FFI) and a core client talk through the real server (feature
//! `integration`). Needs the dev stack and `TEAMCRAFT_TEST_SERVER_URL` (see teamcraft-core's
//! README); skips itself without the variable.
#![cfg(feature = "integration")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use teamcraft_core::api::{HttpServerApi, StaticToken};
use teamcraft_core::clock::{Clock, SystemClock};
use teamcraft_core::store::MemoryStore;
use teamcraft_core::{ClientSettings, EnrollmentOptions, EnrollmentSecret, PendingEnrollment};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_crypto::secret::SecretBytes;
use teamcraft_ffi::{AccountState, AppConfig, EventListener, TeamCraft, TokenProvider};
use teamcraft_proto::{CreateSpaceRequest, JoinPolicy, SpaceResponse};

/// Same as teamcraft-core's dev-stack tests, so both recover the same identities.
const PASSPHRASE: &str = "dev-stack integration passphrase";

#[tokio::test]
async fn mobile_client_receives_and_answers_a_message() {
  let Some(server_url) = std::env::var("TEAMCRAFT_TEST_SERVER_URL").ok() else {
    eprintln!("skipping: TEAMCRAFT_TEST_SERVER_URL is not set");
    return;
  };
  let (alice_token, bob_token) = (dev_token("alice"), dev_token("bob"));

  // Bob is the phone.
  let directory = std::env::temp_dir().join(format!("teamcraft-ffi-it-{}", SystemClock.now_ms()));
  let changes = Arc::new(Changes(AtomicU32::new(0)));
  let phone = TeamCraft::new(
    AppConfig {
      server_url: server_url.clone(),
      data_dir: directory.to_string_lossy().into_owned(),
      account: "bob".into(),
      database_key: vec![42; 32],
    },
    Arc::new(Token(bob_token.clone())),
    changes.clone(),
  )
  .unwrap();
  let state = phone.enroll("bob phone".into(), PASSPHRASE.into()).await.unwrap();
  let AccountState::Ready { user_id: bob_id, .. } = state else {
    panic!("not enrolled: {state:?}");
  };
  phone.start().await.unwrap();
  // The app starts again on every foreground; a running sync loop must survive that.
  phone.start().await.unwrap();

  // Alice is a desktop-like core client.
  let api = HttpServerApi::new(server_url.clone(), StaticToken(alice_token.clone()));
  let mut alice = PendingEnrollment::new()
    .complete(
      api,
      Arc::new(MemoryStore::new()),
      Box::new(SystemClock),
      ClientSettings::default(),
      EnrollmentOptions {
        device_name: "alice ffi integration".into(),
        secret: EnrollmentSecret::Passphrase(SecretBytes::new(PASSPHRASE.as_bytes().to_vec())),
        argon2: Argon2Settings {
          memory_kib: 1024,
          iterations: 1,
          parallelism: 1,
        },
        maybe_attestation: None,
      },
    )
    .await
    .unwrap();
  let (mut alice_gateway, _) = alice.connect_gateway().await.unwrap();
  let space = create_open_space(&server_url, &alice_token, &bob_token).await;
  let channel = alice.create_channel(space, "mobile-integration").await.unwrap();
  alice.add_member(channel, bob_id.parse().unwrap()).await.unwrap();
  alice.send_text(channel, None, "hello phone").await.unwrap();

  let channel_id = channel.to_string();
  wait_until("the phone shows Alice's message", || async {
    phone
      .messages(channel_id.clone(), 20)
      .await
      .is_ok_and(|messages| messages.iter().any(|message| message.body == "hello phone"))
  })
  .await;
  assert!(
    changes.0.load(Ordering::Relaxed) > 0,
    "the app was told to re-read"
  );
  assert!(
    phone
      .conversations()
      .await
      .unwrap()
      .iter()
      .any(|conversation| conversation.id == channel_id)
  );

  phone
    .send_message(channel_id.clone(), None, "hello desktop".into())
    .await
    .unwrap();
  let deadline = Instant::now() + Duration::from_secs(20);
  loop {
    alice.pump(&mut alice_gateway).await.unwrap();
    let texts: Vec<String> = teamcraft_core::store::LocalStore::messages(
      alice.store().as_ref(),
      channel,
      teamcraft_core::store::MessageFilter::TopLevel,
    )
    .unwrap()
    .into_iter()
    .filter_map(|message| match message.content {
      teamcraft_core::store::MessageContent::Text { body, .. } => Some(body),
      _ => None,
    })
    .collect();
    if texts.iter().any(|text| text == "hello desktop") {
      break;
    }
    assert!(Instant::now() < deadline, "Alice never got the phone's reply");
    tokio::time::sleep(Duration::from_millis(200)).await;
  }

  // The JSON bridge the app uses for everything else: snapshot, window, actions, queries.
  let snapshot: serde_json::Value = serde_json::from_str(&phone.snapshot().await.unwrap()).unwrap();
  assert!(
    snapshot["conversations"]
      .as_array()
      .unwrap()
      .iter()
      .any(|conversation| conversation["id"] == channel_id.as_str())
  );
  let window: serde_json::Value =
    serde_json::from_str(&phone.message_window(channel_id.clone(), 0, 50).await.unwrap()).unwrap();
  let alices = window["messages"]
    .as_array()
    .unwrap()
    .iter()
    .find(|message| message["body"] == "hello phone")
    .unwrap()["id"]
    .clone();
  phone
    .act(
      serde_json::json!({
        "action": "toggle_reaction", "conversation": channel_id, "message": alices, "emoji": "👍"
      })
      .to_string(),
    )
    .await
    .unwrap();
  let window: serde_json::Value =
    serde_json::from_str(&phone.message_window(channel_id.clone(), 0, 50).await.unwrap()).unwrap();
  let reacted = window["messages"]
    .as_array()
    .unwrap()
    .iter()
    .find(|message| message["id"] == alices)
    .unwrap();
  assert_eq!(reacted["reactions"][0]["emoji"], "👍");
  let answer: serde_json::Value = serde_json::from_str(
    &phone
      .query(serde_json::json!({"kind": "followed_threads"}).to_string())
      .await
      .unwrap(),
  )
  .unwrap();
  assert_eq!(answer["answer"], "threads");
  let me: serde_json::Value =
    serde_json::from_str(&phone.api_request("GET".into(), "/me".into(), None).await.unwrap()).unwrap();
  assert_eq!(me["id"], bob_id.as_str());
  assert!(phone.act("{\"action\":\"publish_policy\"}".into()).await.is_err());

  phone.stop().await.unwrap();
  std::fs::remove_dir_all(&directory).unwrap();
}

struct Token(String);

impl TokenProvider for Token {
  fn access_token(&self) -> Option<String> {
    Some(self.0.clone())
  }
}

struct Changes(AtomicU32);

impl EventListener for Changes {
  fn on_change(&self) {
    self.0.fetch_add(1, Ordering::Relaxed);
  }
}

async fn wait_until<F, Fut>(what: &str, mut done: F)
where
  F: FnMut() -> Fut,
  Fut: std::future::Future<Output = bool>,
{
  let deadline = Instant::now() + Duration::from_secs(20);
  while !done().await {
    assert!(Instant::now() < deadline, "timed out waiting until {what}");
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
}

async fn create_open_space(
  server_url: &str,
  owner_token: &str,
  member_token: &str,
) -> teamcraft_proto::SpaceId {
  let http = reqwest::Client::new();
  let request = CreateSpaceRequest {
    name: format!("Mobile {}", SystemClock.now_ms()),
    maybe_icon: None,
    maybe_color: None,
    maybe_description: None,
    join_policy: JoinPolicy::Open,
  };
  let space: SpaceResponse = http
    .post(format!("{server_url}/api/v1/spaces"))
    .bearer_auth(owner_token)
    .json(&request)
    .send()
    .await
    .unwrap()
    .error_for_status()
    .unwrap()
    .json()
    .await
    .unwrap();
  http
    .post(format!("{server_url}/api/v1/spaces/{}/join", space.id))
    .bearer_auth(member_token)
    .send()
    .await
    .unwrap()
    .error_for_status()
    .unwrap();
  space.id
}

fn dev_token(user: &str) -> String {
  let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../dev/dev-token.py");
  let output = Command::new("python3")
    .arg(script)
    .arg(user)
    .output()
    .expect("python3 runs dev-token.py");
  assert!(output.status.success(), "dev-token.py failed");
  String::from_utf8(output.stdout).unwrap().trim().to_string()
}
