//! The signed roster and workspace policy against a real server and Postgres.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_proto::{PolicyDocument, WorkspacePolicy};
use teamcraft_server::testing::{Reply, TestApp, TestUser};

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

fn b64(bytes: &[u8]) -> String {
  use base64::Engine;
  base64::engine::general_purpose::STANDARD.encode(bytes)
}

async fn append(
  app: &TestApp,
  user: &TestUser,
  device: &str,
  path: &str,
  expected: u64,
  payload: &[u8],
) -> Reply {
  app
    .post(
      user,
      path,
      json!({"expected_version": expected, "payload": b64(payload), "signature": b64(b"sig"), "signer_device_id": device}),
    )
    .await
}

/// A roster document with one owner; `tag` tells entries apart (the server stores the bytes as they are).
fn roster(tag: &str) -> Vec<u8> {
  roster_with(tag, &["owner", "admin"])
}

fn roster_with(tag: &str, roles: &[&str]) -> Vec<u8> {
  let members: Vec<Value> = roles
    .iter()
    .enumerate()
    .map(|(index, role)| json!({"user_id": (index + 1).to_string(), "role": role, "identity_key": "AAAA"}))
    .collect();
  serde_json::to_vec(&json!({"version": 0, "members": members, "tag": tag})).unwrap()
}

fn policy_payload(version: u64, edit: impl FnOnce(&mut Value)) -> Vec<u8> {
  let mut value = serde_json::to_value(PolicyDocument {
    version,
    policy: WorkspacePolicy::default(),
  })
  .unwrap();
  edit(&mut value["policy"]);
  serde_json::to_vec(&value).unwrap()
}

#[tokio::test]
async fn only_owners_append_and_everybody_can_read_the_roster() {
  let app = app!();
  let (alice, bob, carol) = (owner(), admin(), member("carol"));
  let (alice_device, bob_device) = (
    register_device(&app, &alice).await,
    register_device(&app, &bob).await,
  );
  enroll_identity(&app, &alice).await;

  let denied = append(&app, &bob, &bob_device, "/workspace/roster", 0, b"admin-entry").await;
  assert_eq!(
    denied.status,
    StatusCode::FORBIDDEN,
    "admins don't sign the roster"
  );
  let first = append(
    &app,
    &alice,
    &alice_device,
    "/workspace/roster",
    0,
    &roster("entry-1"),
  )
  .await;
  assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
  assert_eq!(first.body["version"], 1);
  assert_eq!(first.body["signer_device_id"], alice_device);
  append(
    &app,
    &alice,
    &alice_device,
    "/workspace/roster",
    1,
    &roster("entry-2"),
  )
  .await;

  let listed = app.get(&carol, "/workspace/roster").await;
  let versions: Vec<_> = listed.body["entries"]
    .as_array()
    .unwrap()
    .iter()
    .map(|e| e["version"].clone())
    .collect();
  assert_eq!(versions, vec![json!(1), json!(2)]);
  assert_eq!(listed.body["entries"][0]["payload"], b64(&roster("entry-1")));
  let tail = app.get(&carol, "/workspace/roster?after=1").await;
  assert_eq!(tail.body["entries"].as_array().unwrap().len(), 1);
  app.stop().await;
}

#[tokio::test]
async fn the_root_of_trust_needs_a_recoverable_identity() {
  let app = app!();
  let alice = owner();
  let device = register_device(&app, &alice).await;
  // No identity key yet, or one from an older client without a key bag: the genesis roster would
  // pin a root of trust nobody can recover (e2ee.md §3.2).
  let refused = append(&app, &alice, &device, "/workspace/roster", 0, &roster("genesis")).await;
  assert_eq!(refused.error_code(), "conflict", "{}", refused.body);
  enroll_identity(&app, &alice).await;
  let first = append(&app, &alice, &device, "/workspace/roster", 0, &roster("genesis")).await;
  assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
  app.stop().await;
}

#[tokio::test]
async fn a_roster_without_an_owner_is_refused() {
  let app = app!();
  let alice = owner();
  let device = register_device(&app, &alice).await;
  enroll_identity(&app, &alice).await;
  let first = append(&app, &alice, &device, "/workspace/roster", 0, &roster("genesis")).await;
  assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);

  // admin.md §2.3: nobody could sign the next roster. Clients refuse such an entry too.
  for roles in [&["admin"][..], &[][..]] {
    let orphaned = append(
      &app,
      &alice,
      &device,
      "/workspace/roster",
      1,
      &roster_with("orphaned", roles),
    )
    .await;
    assert_eq!(
      (orphaned.status, orphaned.error_code()),
      (StatusCode::BAD_REQUEST, "last_owner"),
      "{}",
      orphaned.body
    );
  }
  let garbage = append(&app, &alice, &device, "/workspace/roster", 1, b"not a roster").await;
  assert_eq!(garbage.error_code(), "invalid_request");
  let listed = app.get(&alice, "/workspace/roster").await;
  assert_eq!(
    listed.body["entries"].as_array().unwrap().len(),
    1,
    "nothing was stored"
  );
  app.stop().await;
}

#[tokio::test]
async fn the_signer_device_must_be_the_callers() {
  let app = app!();
  let (alice, bob) = (owner(), admin());
  let bob_device = register_device(&app, &bob).await;
  let stolen = append(&app, &alice, &bob_device, "/workspace/roster", 0, b"x").await;
  assert_eq!(stolen.error_code(), "device_not_found");
  let alice_device = register_device(&app, &alice).await;
  let empty = append(&app, &alice, &alice_device, "/workspace/roster", 0, b"").await;
  assert_eq!(empty.status, StatusCode::BAD_REQUEST);
  app.stop().await;
}

#[tokio::test]
async fn stale_versions_conflict_and_concurrent_appends_have_one_winner() {
  let app = app!();
  let alice = owner();
  let device = register_device(&app, &alice).await;
  enroll_identity(&app, &alice).await;
  append(&app, &alice, &device, "/workspace/roster", 0, &roster("one")).await;

  let stale = append(&app, &alice, &device, "/workspace/roster", 0, &roster("again")).await;
  assert_eq!(
    (stale.status, stale.error_code()),
    (StatusCode::CONFLICT, "version_conflict")
  );
  let gap = append(&app, &alice, &device, "/workspace/roster", 5, &roster("gap")).await;
  assert_eq!(gap.error_code(), "version_conflict");

  let attempts = (0..8).map(|n| {
    let (app, alice, device) = (&app, &alice, device.clone());
    async move {
      append(
        app,
        alice,
        &device,
        "/workspace/roster",
        1,
        &roster(&format!("race-{n}")),
      )
      .await
    }
  });
  let replies = futures_util::future::join_all(attempts).await;
  assert_eq!(
    replies.iter().filter(|r| r.status == StatusCode::CREATED).count(),
    1
  );
  let listed = app.get(&alice, "/workspace/roster").await;
  assert_eq!(
    listed.body["entries"].as_array().unwrap().len(),
    2,
    "no entry was lost or duplicated"
  );
  app.stop().await;
}

#[tokio::test]
async fn the_policy_defaults_until_an_owner_signs_one() {
  let app = app!();
  let (alice, carol) = (owner(), member("carol"));
  let device = register_device(&app, &alice).await;

  let initial = app.get(&carol, "/workspace/policy").await;
  assert_eq!(initial.body["version"], 0);
  assert_eq!(initial.body["policy"]["admin_self_join"], "visible");
  assert_eq!(initial.body["policy"]["compliance_archive"]["mode"], "off");

  let payload = policy_payload(1, |p| {
    p["admin_self_join"] = json!("owner_approval");
    p["retention"]["default"] = json!({"max_age_days": 30});
    p["retention"]["limit"] = json!({"max_age_days": 365});
    p["link_previews"] = json!({"mode": "allow_list", "domains": ["example.com"]});
  });
  let appended = append(&app, &alice, &device, "/workspace/policy", 0, &payload).await;
  assert_eq!(appended.status, StatusCode::CREATED, "{}", appended.body);

  let current = app.get(&carol, "/workspace/policy").await;
  assert_eq!(current.body["version"], 1);
  assert_eq!(current.body["policy"]["admin_self_join"], "owner_approval");
  assert_eq!(current.body["policy"]["retention"]["default"]["max_age_days"], 30);
  // The signed bytes are served verbatim so clients can verify the signature.
  assert_eq!(current.body["entry"]["payload"], b64(&payload));
  assert_eq!(
    app.get(&carol, "/workspace/policy/log").await.body["entries"]
      .as_array()
      .unwrap()
      .len(),
    1
  );
  app.stop().await;
}

#[tokio::test]
async fn invalid_policies_are_rejected_without_changing_the_log() {
  let app = app!();
  let alice = owner();
  let device = register_device(&app, &alice).await;
  let path = "/workspace/policy";

  let wrong_version = append(&app, &alice, &device, path, 0, &policy_payload(7, |_| {})).await;
  assert_eq!(wrong_version.status, StatusCode::BAD_REQUEST);
  let archive = append(
    &app,
    &alice,
    &device,
    path,
    0,
    &policy_payload(1, |p| p["compliance_archive"]["mode"] = json!("on")),
  )
  .await;
  assert_eq!(archive.status, StatusCode::BAD_REQUEST);
  let unlimited = append(
    &app,
    &alice,
    &device,
    path,
    0,
    &policy_payload(1, |p| p["retention"]["limit"] = json!({"max_age_days": 90})),
  )
  .await;
  assert_eq!(
    unlimited.status,
    StatusCode::BAD_REQUEST,
    "default is unlimited but the limit is not"
  );
  assert_eq!(
    append(&app, &alice, &device, path, 0, b"not json").await.status,
    StatusCode::BAD_REQUEST
  );
  assert_eq!(app.get(&alice, "/workspace/policy").await.body["version"], 0);
  assert_eq!(
    append(&app, &alice, &device, path, 0, &policy_payload(1, |_| {}))
      .await
      .status,
    StatusCode::CREATED
  );
  app.stop().await;
}

#[tokio::test]
async fn connected_clients_are_told_about_new_entries() {
  use futures_util::{SinkExt, StreamExt};
  use teamcraft_proto::{DeviceActivity, GatewayClientMessage, GatewayEvent, GatewayServerMessage};
  use tokio_tungstenite::tungstenite::Message;

  let app = app!();
  let (alice, carol) = (owner(), member("carol"));
  let (device, carol_device) = (
    register_device(&app, &alice).await,
    register_device(&app, &carol).await,
  );
  enroll_identity(&app, &alice).await;

  let (mut socket, _) = tokio_tungstenite::connect_async(&app.ws_url).await.unwrap();
  socket.next().await;
  let identify = GatewayClientMessage::Identify {
    token: app.token(&carol),
    device_id: serde_json::from_value(json!(carol_device)).unwrap(),
    activity: DeviceActivity::Active,
  };
  socket
    .send(Message::Text(serde_json::to_string(&identify).unwrap().into()))
    .await
    .unwrap();
  socket.next().await;

  append(&app, &alice, &device, "/workspace/roster", 0, &roster("entry")).await;
  let found = tokio::time::timeout(std::time::Duration::from_secs(5), async {
    loop {
      let Some(Ok(Message::Text(text))) = socket.next().await else {
        panic!("closed")
      };
      if let Ok(GatewayServerMessage::Event {
        event: GatewayEvent::WorkspaceUpdate { kind, version },
        ..
      }) = serde_json::from_str(text.as_str())
      {
        return (kind.to_string(), version);
      }
    }
  })
  .await
  .expect("no workspace update arrived");
  assert_eq!(found, ("roster".to_owned(), 1));
  app.stop().await;
}
