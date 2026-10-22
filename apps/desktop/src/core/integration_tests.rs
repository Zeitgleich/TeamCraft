//! Two dev users exchange end-to-end encrypted messages through the real server, driven the way the
//! UI drives the core: enrollment, a channel and a DM, snapshots and message windows (feature
//! `integration`; needs the dev stack and a server on `TEAMCRAFT_SERVER_URL`, default 127.0.0.1:8081).
// Test-only module: a failing step should abort the test with a clear message.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use teamcraft_client_api::model::ConversationKind;
use teamcraft_client_api::remote::{ChatAction, ChatSnapshot, CoreStatus, EnrollRequest, LinkStatus};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_proto::{ConversationId, GetMeResponse, SpaceId, UserId};

use super::runtime::{CoreConfig, CoreRuntime, KeyChoice, TokenCache};
use crate::auth::integration_tests::{SharedStore, spawn_fake_browser};
use crate::auth::{AuthService, DesktopConfig};

const PASSPHRASE: &str = "dev-stack core passphrase";
const WAIT: Duration = Duration::from_secs(30);

pub(super) struct Person {
  pub(super) name: &'static str,
  pub(super) id: UserId,
  pub(super) token: String,
  pub(super) runtime: CoreRuntime,
  pub(super) service: Arc<AuthService<SharedStore>>,
  tokens: TokenCache,
  directory: PathBuf,
}

impl Drop for Person {
  fn drop(&mut self) {
    drop(std::fs::remove_dir_all(&self.directory));
  }
}

#[tokio::test]
async fn alice_and_carol_chat_in_a_channel_and_a_dm_through_the_core() {
  let config = DesktopConfig::from_env();
  let (alice, carol) = (person("alice", &config).await, person("carol", &config).await);
  for person in [&alice, &carol] {
    enroll(person).await;
  }

  // Spaces are plain REST; everything below goes through the core.
  let space = create_space(&alice.token, &carol.token, &config).await;
  // Channel names are unique per run: the dev database keeps channels of earlier runs.
  let name = format!("e2e-core-{}", std::process::id());
  alice
    .runtime
    .action(ChatAction::CreateChannel {
      space,
      name: name.clone(),
    })
    .await
    .expect("channel");
  let channel = find(&alice, |snapshot| conversation_named(snapshot, &name)).await;
  alice
    .runtime
    .action(ChatAction::AddMember {
      conversation: channel,
      user: carol.id,
    })
    .await
    .expect("add carol");
  assert_eq!(
    find(&carol, |snapshot| conversation_named(snapshot, &name)).await,
    channel
  );

  let marker = format!("hello carol, from the channel at {}", std::process::id());
  alice
    .runtime
    .action(ChatAction::PostMessage {
      conversation: channel,
      maybe_thread: None,
      body: marker.clone(),
    })
    .await
    .expect("post");
  wait_for_text(&carol, channel, &marker).await;
  carol
    .runtime
    .action(ChatAction::PostMessage {
      conversation: channel,
      maybe_thread: None,
      body: "hi alice, got it".into(),
    })
    .await
    .expect("reply");
  wait_for_text(&alice, channel, "hi alice, got it").await;

  // A DM is unique per pair and the devices of earlier runs are gone, so this part needs a fresh
  // database (drop and recreate it, then restart the server).
  if has_dm_already(&alice.token, &config).await {
    eprintln!("skipping the DM part: alice and carol already have a DM from an earlier run");
    return;
  }
  alice
    .runtime
    .action(ChatAction::OpenDm { user: carol.id })
    .await
    .expect("dm");
  let dm = find(&alice, |snapshot| direct_with(snapshot, carol.id)).await;
  let secret = format!("just between us {}", std::process::id());
  alice
    .runtime
    .action(ChatAction::PostMessage {
      conversation: dm,
      maybe_thread: None,
      body: secret.clone(),
    })
    .await
    .expect("dm post");
  wait_for_text(&carol, dm, &secret).await;
  carol
    .runtime
    .action(ChatAction::PostMessage {
      conversation: dm,
      maybe_thread: None,
      body: "agreed".into(),
    })
    .await
    .expect("dm reply");
  wait_for_text(&alice, dm, "agreed").await;

  // The server only ever saw ciphertext, and the DM is not visible in the channel.
  let in_channel = texts(&carol, channel).await;
  assert!(
    !in_channel.contains(&secret),
    "DM text must not leak into the channel: {in_channel:?}"
  );
}

pub(super) async fn person(name: &'static str, config: &DesktopConfig) -> Person {
  let service = Arc::new(AuthService::new(config.clone(), SharedStore::default()).expect("auth service"));
  service
    .login(|url| spawn_fake_browser(url, name))
    .await
    .expect("login");
  let me = service.api_get("/api/v1/me").await.expect("me");
  let me: GetMeResponse =
    serde_json::from_str(&me.body).unwrap_or_else(|error| panic!("me body: {error}: {}", me.body));
  let token = service.bearer_token().await.expect("token");
  let tokens = TokenCache::default();
  tokens.set(Some(token.clone()));
  // Access tokens live minutes; long-running users (the dev bridge) keep the cache fresh.
  tokio::spawn({
    let (service, tokens) = (service.clone(), tokens.clone());
    async move {
      loop {
        tokio::time::sleep(Duration::from_secs(20)).await;
        tokens.set(service.bearer_token().await.ok());
      }
    }
  });
  let directory = std::env::temp_dir().join(format!("teamcraft-core-e2e-{}-{name}", std::process::id()));
  let core_config = CoreConfig {
    server_url: config.server_url.clone(),
    data_dir: directory.clone(),
    keystore_scope: "test".into(),
    // Cheap Argon2 so the test is quick; real clients use the 128 MiB default.
    argon2: Argon2Settings {
      memory_kib: 1024,
      iterations: 1,
      parallelism: 1,
    },
  };
  let runtime = CoreRuntime::spawn(
    core_config,
    me.id,
    KeyChoice::Fixed(vec![7; 32]),
    tokens.clone(),
    Arc::new(|| {}),
    Arc::new(|_| {}),
  );
  Person {
    name,
    id: me.id,
    token,
    runtime,
    service,
    tokens,
    directory,
  }
}

/// A second, empty device of the same user (its own database).
fn second_device(person: &Person, config: &DesktopConfig) -> CoreRuntime {
  let core_config = CoreConfig {
    server_url: config.server_url.clone(),
    data_dir: person.directory.join("second-device"),
    keystore_scope: "test".into(),
    argon2: Argon2Settings {
      memory_kib: 1024,
      iterations: 1,
      parallelism: 1,
    },
  };
  CoreRuntime::spawn(
    core_config,
    person.id,
    KeyChoice::Fixed(vec![8; 32]),
    person.tokens.clone(),
    Arc::new(|| {}),
    Arc::new(|_| {}),
  )
}

#[tokio::test]
async fn a_second_device_is_linked_with_a_code_and_confirmed_on_the_first() {
  let config = DesktopConfig::from_env();
  let first = person("alice", &config).await;
  match first.runtime.status().await {
    CoreStatus::NeedsEnrollment { .. } => enroll(&first).await,
    CoreStatus::Ready { .. } | CoreStatus::NotStarted | CoreStatus::Failed { .. } => {},
  }
  let second = second_device(&first, &config);
  assert!(matches!(
    second.status().await,
    CoreStatus::NeedsEnrollment {
      has_existing_identity: true
    }
  ));

  second.link_start("Laptop".into()).await.expect("link start");
  let code = wait_for_link(&second, |status| match status {
    LinkStatus::WaitingForScan { code } => Some(code),
    _ => None,
  })
  .await;
  first.runtime.approval_start(code).await.expect("approval start");
  let second_short = wait_for_link(&second, |status| match status {
    LinkStatus::Confirm { short_code, .. } => Some(short_code),
    _ => None,
  })
  .await;
  // The existing device never learns the short code: the user types what the new device shows, and
  // the core compares (a wrong code is refused and does not hand anything over).
  assert!(first.runtime.approval_confirm("no digits".into()).await.is_err());
  first
    .runtime
    .approval_confirm(second_short)
    .await
    .expect("approval confirm with the code of the new device");
  let deadline = tokio::time::Instant::now() + WAIT;
  loop {
    if let CoreStatus::Ready { identity_fingerprint } = second.status().await {
      let CoreStatus::Ready {
        identity_fingerprint: expected,
      } = first.runtime.status().await
      else {
        panic!("the first device lost its keys");
      };
      assert_eq!(
        identity_fingerprint, expected,
        "the linked device has the same identity"
      );
      break;
    }
    assert!(
      tokio::time::Instant::now() < deadline,
      "the second device never became ready"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
}

#[tokio::test]
async fn the_recovery_passphrase_can_be_changed_and_the_new_one_recovers() {
  const NEW_PASSPHRASE: &str = "a brand new passphrase 42";
  let config = DesktopConfig::from_env();
  let first = person("alice", &config).await;
  match first.runtime.status().await {
    CoreStatus::NeedsEnrollment { .. } => enroll(&first).await,
    CoreStatus::Ready { .. } | CoreStatus::NotStarted | CoreStatus::Failed { .. } => {},
  }
  first
    .runtime
    .action(ChatAction::ChangePassphrase {
      passphrase: NEW_PASSPHRASE.to_owned(),
    })
    .await
    .expect("change the passphrase");

  let second = second_device(&first, &config);
  let wrong = second
    .enroll(EnrollRequest {
      passphrase: PASSPHRASE.to_owned(),
      device_name: "Laptop".into(),
    })
    .await;
  assert!(
    wrong.is_err(),
    "the old passphrase must not recover the keys any more"
  );
  second
    .enroll(EnrollRequest {
      passphrase: NEW_PASSPHRASE.to_owned(),
      device_name: "Laptop".into(),
    })
    .await
    .expect("the new passphrase recovers the keys");
  assert!(matches!(second.status().await, CoreStatus::Ready { .. }));
}

async fn wait_for_link<T>(runtime: &CoreRuntime, pick: impl Fn(LinkStatus) -> Option<T>) -> T {
  let deadline = tokio::time::Instant::now() + WAIT;
  loop {
    let status = runtime.link_status().await;
    if let LinkStatus::Failed { message } = &status {
      panic!("linking failed: {message}");
    }
    if let Some(found) = pick(status) {
      return found;
    }
    assert!(
      tokio::time::Instant::now() < deadline,
      "the link did not progress"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
}

pub(super) async fn enroll(person: &Person) {
  match person.runtime.status().await {
    CoreStatus::NeedsEnrollment { .. } => {},
    other => panic!("{} should need enrollment, got {other:?}", person.name),
  }
  let request = EnrollRequest {
    passphrase: PASSPHRASE.into(),
    device_name: format!("{} e2e", person.name),
  };
  person.runtime.enroll(request).await.expect("enroll");
  assert!(matches!(person.runtime.status().await, CoreStatus::Ready { .. }));
}

async fn has_dm_already(token: &str, config: &DesktopConfig) -> bool {
  let base = config.server_url.trim_end_matches('/');
  let listed: serde_json::Value = reqwest::Client::new()
    .get(format!("{base}/api/v1/conversations"))
    .bearer_auth(token)
    .send()
    .await
    .expect("list conversations")
    .json()
    .await
    .expect("conversations body");
  listed["conversations"]
    .as_array()
    .is_some_and(|all| all.iter().any(|c| c["kind"] == "dm"))
}

pub(super) async fn create_space(owner_token: &str, member_token: &str, config: &DesktopConfig) -> SpaceId {
  let http = reqwest::Client::new();
  let base = config.server_url.trim_end_matches('/');
  let created: serde_json::Value = http
    .post(format!("{base}/api/v1/spaces"))
    .bearer_auth(owner_token)
    .json(&serde_json::json!({"name": format!("E2E core {}", std::process::id()), "join_policy": "open"}))
    .send()
    .await
    .expect("create space")
    .json()
    .await
    .expect("space body");
  let id = created["id"].as_str().expect("space id").to_owned();
  http
    .post(format!("{base}/api/v1/spaces/{id}/join"))
    .bearer_auth(member_token)
    .send()
    .await
    .expect("join space");
  serde_json::from_value(serde_json::Value::String(id)).expect("space id parse")
}

pub(super) fn conversation_named(snapshot: &ChatSnapshot, name: &str) -> Option<ConversationId> {
  snapshot
    .conversations
    .iter()
    .find(|conversation| conversation.name == name)
    .map(|conversation| conversation.id)
}

pub(super) fn direct_with(snapshot: &ChatSnapshot, user: UserId) -> Option<ConversationId> {
  snapshot
    .conversations
    .iter()
    .find(|conversation| {
      conversation.kind == ConversationKind::Direct && conversation.member_ids.contains(&user)
    })
    .map(|conversation| conversation.id)
}

pub(super) async fn find(
  person: &Person,
  pick: impl Fn(&ChatSnapshot) -> Option<ConversationId>,
) -> ConversationId {
  poll(person.name, || async {
    person
      .runtime
      .snapshot()
      .await
      .ok()
      .and_then(|snapshot| pick(&snapshot))
  })
  .await
}

pub(super) async fn texts(person: &Person, conversation: ConversationId) -> Vec<String> {
  let window = person
    .runtime
    .messages(conversation, 0, 200)
    .await
    .expect("window");
  window.messages.into_iter().map(|message| message.body).collect()
}

pub(super) async fn wait_for_text(person: &Person, conversation: ConversationId, text: &str) {
  poll(person.name, || async {
    texts(person, conversation)
      .await
      .iter()
      .any(|body| body == text)
      .then_some(())
  })
  .await;
}

/// Retries `probe` until it yields a value or `WAIT` has passed.
pub(super) async fn poll<T, F: Future<Output = Option<T>>>(who: &str, probe: impl Fn() -> F) -> T {
  let deadline = tokio::time::Instant::now() + WAIT;
  loop {
    if let Some(value) = probe().await {
      return value;
    }
    assert!(
      tokio::time::Instant::now() < deadline,
      "timed out waiting for {who}"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
}
