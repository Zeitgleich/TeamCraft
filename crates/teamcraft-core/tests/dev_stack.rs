//! Two dev users exchange encrypted messages through the real server (feature `integration`).
//!
//! Needs the dev stack and a running server (README.md, "Run the server"), named by
//! `TEAMCRAFT_TEST_SERVER_URL` (e.g. `http://127.0.0.1:8081`); without it the tests skip themselves,
//! like the database tests do without `DATABASE_URL`. Tokens come from `dev/dev-token.py`.
#![cfg(feature = "integration")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use teamcraft_core::LinkOffer;
use teamcraft_core::api::{HttpGateway, HttpServerApi, StaticToken};
use teamcraft_core::attestation::{AttestationError, write_identity_attribute};
use teamcraft_core::client::{NewDeviceLink, OutgoingFile};
use teamcraft_core::clock::{Clock, SystemClock};
use teamcraft_core::store::{LocalStore, MemoryStore, MessageContent, MessageFilter};
use teamcraft_core::workspace::RosterRole;
use teamcraft_core::{Client, ClientSettings, EnrollmentOptions, PendingEnrollment};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_crypto::secret::SecretBytes;
use teamcraft_proto::{
  AdminSelfJoin, ConversationId, CreateSpaceRequest, JoinPolicy, PushProvider, SpaceId, SpaceResponse,
  WorkspacePolicy,
};

const ISSUER: &str = "http://127.0.0.1:8080/realms/teamcraft-dev";
/// The dev database keeps identities between runs; later runs recover them with this passphrase.
const PASSPHRASE: &str = "dev-stack integration passphrase";

type DevClient = Client<HttpServerApi<StaticToken>, Arc<MemoryStore>>;

#[tokio::test]
async fn two_dev_users_exchange_encrypted_messages() {
  if server_url().is_none() {
    eprintln!("skipping: TEAMCRAFT_TEST_SERVER_URL is not set");
    return;
  }
  let (alice_token, bob_token) = (dev_token("alice"), dev_token("bob"));
  let (mut alice, alice_store) = enroll("alice", &alice_token).await;
  let (mut bob, bob_store) = enroll("bob", &bob_token).await;
  let (mut alice_gateway, _) = alice.connect_gateway().await.unwrap();
  let (mut bob_gateway, _) = bob.connect_gateway().await.unwrap();

  // A fresh space and channel per run: the dev database keeps conversations whose devices from
  // earlier runs are gone.
  let space = create_open_space(&alice_token, &bob_token).await;
  let channel = alice.create_channel(space, "core-integration").await.unwrap();
  alice.add_member(channel, bob.user_id()).await.unwrap();
  // Bob joins through the welcome the gateway delivers, then gets the content keys.
  pump_until("Bob joins the channel", &mut bob, &mut bob_gateway, |client| {
    client.has_group(channel).unwrap()
  })
  .await;

  let marker = format!("hello from Alice, {} ms", SystemClock.now_ms());
  alice.send_text(channel, None, &marker).await.unwrap();
  pump_until("Bob decrypts Alice's message", &mut bob, &mut bob_gateway, |_| {
    texts(&bob_store, channel).contains(&marker)
  })
  .await;

  bob.send_text(channel, None, "hello back").await.unwrap();
  pump_until(
    "Alice decrypts Bob's reply",
    &mut alice,
    &mut alice_gateway,
    |_| {
      texts(&alice_store, channel)
        .iter()
        .any(|text| text == "hello back")
    },
  )
  .await;
}

#[tokio::test]
async fn owner_signed_policy_reaches_other_clients() {
  if server_url().is_none() {
    eprintln!("skipping: TEAMCRAFT_TEST_SERVER_URL is not set");
    return;
  }
  let (alice_token, bob_token) = (dev_token("alice"), dev_token("bob"));
  let (mut alice, _) = enroll("alice", &alice_token).await;
  let (mut bob, _) = enroll("bob", &bob_token).await;
  let roles = [
    (alice.user_id(), RosterRole::Owner),
    (bob.user_id(), RosterRole::Admin),
  ];
  alice.publish_roster(&roles).await.unwrap();
  let policy = WorkspacePolicy {
    admin_self_join: AdminSelfJoin::OwnerApproval,
    ..WorkspacePolicy::default()
  };
  alice.publish_policy(policy).await.unwrap();

  bob.sync_workspace().await.unwrap();
  let workspace = bob.workspace().unwrap();
  assert_eq!(workspace.maybe_rejection, None);
  assert!(workspace.is_admin(bob.user_id()));
  assert_eq!(workspace.policy.admin_self_join, AdminSelfJoin::OwnerApproval);
}

#[tokio::test]
async fn files_and_push_registration_work_against_the_server() {
  if server_url().is_none() {
    eprintln!("skipping: TEAMCRAFT_TEST_SERVER_URL is not set");
    return;
  }
  let (alice_token, bob_token) = (dev_token("alice"), dev_token("bob"));
  let (mut alice, _) = enroll("alice", &alice_token).await;
  let (mut bob, _) = enroll("bob", &bob_token).await;
  let (mut bob_gateway, _) = bob.connect_gateway().await.unwrap();
  let space = create_open_space(&alice_token, &bob_token).await;
  let channel = alice.create_channel(space, "core-files").await.unwrap();
  alice.add_member(channel, bob.user_id()).await.unwrap();
  pump_until("Bob joins the channel", &mut bob, &mut bob_gateway, |client| {
    client.has_group(channel).unwrap()
  })
  .await;

  let content = format!("integration file {}", SystemClock.now_ms()).into_bytes();
  let file = OutgoingFile {
    file_name: "notes.txt".into(),
    media_type: "text/plain".into(),
    content: content.clone(),
  };
  let message = alice.send_file(channel, None, file).await.unwrap();
  pump_until(
    "Bob receives the file message",
    &mut bob,
    &mut bob_gateway,
    |client| {
      LocalStore::message(client.store().as_ref(), channel, message)
        .unwrap()
        .is_some()
    },
  )
  .await;
  assert_eq!(
    bob.fetch_file(channel, message).await.unwrap().expose_secret(),
    content.as_slice()
  );

  alice
    .register_push(PushProvider::Fcm, "integration-test-token")
    .await
    .unwrap();
}

#[tokio::test]
async fn a_second_device_is_linked_through_the_relay() {
  if server_url().is_none() {
    eprintln!("skipping: TEAMCRAFT_TEST_SERVER_URL is not set");
    return;
  }
  let alice_token = dev_token("alice");
  let (mut phone, _) = enroll("alice", &alice_token).await;
  let laptop_api = HttpServerApi::new(required_server_url(), StaticToken(alice_token.clone()));
  let offer = LinkOffer::generate();
  let code = offer.to_code();
  let (laptop_side, phone_side) = tokio::join!(
    NewDeviceLink::connect(&laptop_api, &offer),
    phone.start_link_approval(&code)
  );
  let (laptop_side, phone_side) = (laptop_side.unwrap(), phone_side.unwrap());
  assert_eq!(laptop_side.short_code(), phone_side.short_code());
  phone.complete_link_approval(phone_side).await.unwrap();
  let secrets = laptop_side.receive_secrets().await.unwrap();

  let options = EnrollmentOptions {
    device_name: "alice linked laptop".into(),
    passphrase: SecretBytes::new(b"unused".to_vec()),
    argon2: Argon2Settings {
      memory_kib: 1024,
      iterations: 1,
      parallelism: 1,
    },
    maybe_attestation: None,
    maybe_org_recovery: None,
    maybe_linked: Some(secrets),
  };
  let laptop = PendingEnrollment::new()
    .complete(
      laptop_api,
      Arc::new(MemoryStore::new()),
      Box::new(SystemClock),
      ClientSettings::default(),
      options,
    )
    .await
    .unwrap();
  assert_eq!(laptop.identity_fingerprint(), phone.identity_fingerprint());
}

#[tokio::test]
async fn identity_attribute_can_be_attested_or_reports_why_not() {
  if server_url().is_none() {
    eprintln!("skipping: TEAMCRAFT_TEST_SERVER_URL is not set");
    return;
  }
  let token = dev_token("carol");
  let result = write_identity_attribute(&reqwest::Client::new(), ISSUER, &token, "00ff").await;
  // The dev realm neither grants the public client access to the account API nor declares
  // tc_identity_key in its user profile; every "not available" outcome means enrolling without
  // attestation (crate README).
  match result {
    Ok(()) | Err(AttestationError::AttributeNotStored | AttestationError::NotPermitted) => {},
    Err(error) => panic!("unexpected attestation error: {error}"),
  }
}

async fn enroll(user: &str, token: &str) -> (DevClient, Arc<MemoryStore>) {
  // Tests run in parallel; the first enrollment of a dev user must finish before the next one
  // starts, or both race to publish an identity key (the loser gets `AlreadyEnrolled`).
  static ENROLLMENT: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
  let _turn = ENROLLMENT
    .get_or_init(|| tokio::sync::Mutex::new(()))
    .lock()
    .await;
  let api = HttpServerApi::new(required_server_url(), StaticToken(token.to_string()));
  let store = Arc::new(MemoryStore::new());
  let options = EnrollmentOptions {
    device_name: format!("{user} integration test"),
    passphrase: SecretBytes::new(PASSPHRASE.as_bytes().to_vec()),
    argon2: Argon2Settings {
      memory_kib: 1024,
      iterations: 1,
      parallelism: 1,
    },
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

/// Space setup isn't part of the client core; plain REST calls do it.
async fn create_open_space(owner_token: &str, member_token: &str) -> SpaceId {
  let http = reqwest::Client::new();
  let request = CreateSpaceRequest {
    name: format!("Integration {}", SystemClock.now_ms()),
    maybe_icon: None,
    maybe_color: None,
    maybe_description: None,
    join_policy: JoinPolicy::Open,
  };
  let space: SpaceResponse = http
    .post(format!("{}/api/v1/spaces", required_server_url()))
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
    .post(format!(
      "{}/api/v1/spaces/{}/join",
      required_server_url(),
      space.id
    ))
    .bearer_auth(member_token)
    .send()
    .await
    .unwrap()
    .error_for_status()
    .unwrap();
  space.id
}

/// The server under test; `None` skips the tests.
fn server_url() -> Option<String> {
  std::env::var("TEAMCRAFT_TEST_SERVER_URL").ok()
}

fn required_server_url() -> String {
  server_url().expect("checked at the start of each test")
}

fn dev_token(user: &str) -> String {
  let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../dev/dev-token.py");
  let output = Command::new("python3")
    .arg(script)
    .arg(user)
    .output()
    .expect("python3 runs dev-token.py");
  assert!(
    output.status.success(),
    "dev-token.py failed: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn texts(store: &MemoryStore, conversation: ConversationId) -> Vec<String> {
  store
    .messages(conversation, MessageFilter::TopLevel)
    .unwrap()
    .into_iter()
    .filter_map(|message| match message.content {
      MessageContent::Text { body, .. } => Some(body),
      _ => None,
    })
    .collect()
}

/// Real network delivery takes a moment: apply gateway frames until `done`, with a deadline.
async fn pump_until(
  what: &str,
  client: &mut DevClient,
  gateway: &mut HttpGateway,
  mut done: impl FnMut(&mut DevClient) -> bool,
) {
  let deadline = Instant::now() + Duration::from_secs(15);
  while Instant::now() < deadline {
    client.pump(gateway).await.unwrap();
    if done(client) {
      return;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
  panic!("timed out waiting until {what}");
}
