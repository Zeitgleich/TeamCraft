//! Legal hold (issue #211, data-export.md §4.2) against a real server and Postgres: who may place
//! one, what the server refuses, that retention and deletions leave a held channel alone, and that
//! placing and releasing are audited.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_db::testing::sqlx;
use teamcraft_proto::{PolicyDocument, WorkspacePolicy};
use teamcraft_server::services::retention::run_once;
use teamcraft_server::testing::{Reply, TestApp, TestUser};
use time::{Duration, OffsetDateTime};

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

#[tokio::test]
async fn only_owners_place_holds() {
  let app = app!();
  let (alice, bob, carol, gus) = (owner(), admin(), member("carol"), guest("gus"));
  let (_, channel, _) = space_of_alice(&app, &alice).await;
  for user in [&bob, &carol, &gus] {
    let device = register_device(&app, user).await;
    let reply = sign(&app, user, &device, 0, |p| hold(p, &channel, "Case 1")).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.body);
  }
  let device = register_device(&app, &alice).await;
  let placed = sign(&app, &alice, &device, 0, |p| hold(p, &channel, "Case 1")).await;
  assert_eq!(placed.status, StatusCode::CREATED, "{}", placed.body);
  // Everybody reads the signed list, members too: that is what their banner is built from.
  let read = app.get(&carol, "/workspace/policy").await;
  assert_eq!(
    read.body["policy"]["compliance_archive"]["legal_holds"][0]["reference"],
    "Case 1"
  );
  app.stop().await;
}

#[tokio::test]
async fn invalid_holds_are_refused() {
  let app = app!();
  let alice = owner();
  let (_, channel, device) = space_of_alice(&app, &alice).await;
  for reference in ["", " Case", "line\nbreak", &"x".repeat(65)] {
    let reply = sign(&app, &alice, &device, 0, |p| hold(p, &channel, reference)).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{reference:?}");
  }
  let twice = sign(&app, &alice, &device, 0, |p| {
    hold(p, &channel, "A");
    hold(p, &channel, "B");
  })
  .await;
  assert_eq!(twice.status, StatusCode::BAD_REQUEST);
  // The vault node doesn't exist yet: archiving stays off whatever the holds say.
  let archive = sign(&app, &alice, &device, 0, |p| {
    hold(p, &channel, "A");
    p["compliance_archive"]["mode"] = json!("on");
  })
  .await;
  assert_eq!(archive.status, StatusCode::BAD_REQUEST);
  assert_eq!(app.get(&alice, "/workspace/policy").await.body["version"], 0);
  app.stop().await;
}

#[tokio::test]
async fn only_existing_channels_can_be_held() {
  let app = app!();
  let (alice, dave, erin) = (owner(), member("dave"), member("erin"));
  let (_, channel, device) = space_of_alice(&app, &alice).await;
  let (dave_id, erin_id) = (user_id(&app, &dave).await, user_id(&app, &erin).await);
  let dm = app.post(&alice, "/dms", json!({"user_id": dave_id})).await;
  assert_eq!(dm.status, StatusCode::CREATED, "{}", dm.body);
  let group = app
    .post(&alice, "/group-dms", json!({"user_ids": [dave_id, erin_id]}))
    .await;
  assert_eq!(group.status, StatusCode::CREATED, "{}", group.body);
  // A DM and a group: account deletion removes them whatever a hold says. And an unknown ID.
  for target in [dm.id(), group.id(), "999999999".to_owned()] {
    let reply = sign(&app, &alice, &device, 0, |p| hold(p, &target, "Case 1")).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{target}: {}", reply.body);
  }
  let placed = sign(&app, &alice, &device, 0, |p| hold(p, &channel, "Case 1")).await;
  assert_eq!(placed.status, StatusCode::CREATED, "{}", placed.body);
  app.stop().await;
}

#[tokio::test]
async fn the_audit_compares_with_the_stored_policy_not_this_instances_cache() {
  let app = app!();
  let alice = owner();
  let (_, channel, device) = space_of_alice(&app, &alice).await;
  sign(&app, &alice, &device, 0, |p| hold(p, &channel, "Case 1")).await;
  // Another instance stored it, or this one restarted with a cache that missed it.
  app.state.policy.set(WorkspacePolicy::default());
  let again = sign(&app, &alice, &device, 1, |p| hold(p, &channel, "Case 1")).await;
  assert_eq!(again.status, StatusCode::CREATED, "{}", again.body);
  let audit = app.get(&alice, "/admin/audit?action=legal_hold_placed").await;
  assert_eq!(
    audit.body["entries"].as_array().unwrap().len(),
    1,
    "{}",
    audit.body
  );
  // A stale expected version conflicts: nothing is stored and nothing audited.
  let stale = sign(&app, &alice, &device, 1, |_| {}).await;
  assert_eq!(stale.status, StatusCode::CONFLICT, "{}", stale.body);
  let released = app.get(&alice, "/admin/audit?action=legal_hold_released").await;
  assert!(
    released.body["entries"].as_array().unwrap().is_empty(),
    "{}",
    released.body
  );
  app.stop().await;
}

#[tokio::test]
async fn retention_keeps_a_held_channel_until_the_hold_is_released() {
  let app = app!();
  let alice = owner();
  let (_, channel, device) = space_of_alice(&app, &alice).await;
  for n in 1..=3 {
    send(&app, &alice, &channel, &device, n).await;
  }
  let retention = app
    .put(
      &alice,
      &format!("/conversations/{channel}/retention"),
      json!({"retention": {"max_age_days": 30}}),
    )
    .await;
  assert_eq!(retention.status, StatusCode::OK, "{}", retention.body);
  let placed = sign(&app, &alice, &device, 0, |p| hold(p, &channel, "Case 1")).await;
  assert_eq!(placed.status, StatusCode::CREATED, "{}", placed.body);
  let later = OffsetDateTime::now_utc() + Duration::days(31);

  let held = run_once(&app.state, later).await.unwrap();
  assert_eq!(held.messages_deleted, 0);
  assert_eq!(message_count(&app, &alice, &channel).await, 3);

  let released = sign(&app, &alice, &device, 1, |_| {}).await;
  assert_eq!(released.status, StatusCode::CREATED, "{}", released.body);
  let after = run_once(&app.state, later).await.unwrap();
  assert_eq!(after.messages_deleted, 3);
  app.stop().await;
}

#[tokio::test]
async fn a_held_channel_and_its_space_cant_be_deleted() {
  let app = app!();
  let alice = owner();
  let (space, channel, device) = space_of_alice(&app, &alice).await;
  sign(&app, &alice, &device, 0, |p| hold(p, &channel, "Case 1")).await;

  let channel_reply = app.delete(&alice, &format!("/conversations/{channel}")).await;
  assert_eq!(
    channel_reply.status,
    StatusCode::CONFLICT,
    "{}",
    channel_reply.body
  );
  assert_eq!(channel_reply.body["error_code"], "legal_hold");
  let space_reply = app.delete(&alice, &format!("/spaces/{space}")).await;
  assert_eq!(space_reply.status, StatusCode::CONFLICT, "{}", space_reply.body);
  assert_eq!(space_reply.body["error_code"], "legal_hold");
  let alice_id = user_id(&app, &alice).await;
  let leave = app
    .delete(
      &alice,
      &format!("/conversations/{channel}/members/{alice_id}?delete_if_last=true"),
    )
    .await;
  assert_eq!(leave.status, StatusCode::CONFLICT, "{}", leave.body);
  assert_eq!(leave.body["error_code"], "legal_hold");

  sign(&app, &alice, &device, 1, |_| {}).await;
  let deleted = app.delete(&alice, &format!("/conversations/{channel}")).await;
  assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.body);
  app.stop().await;
}

/// Leaving a space with `delete_if_last` deletes the private channels left without members, but not
/// a held one: it stays, without members, and the leave goes through (#2143).
#[tokio::test]
async fn leaving_a_space_keeps_a_held_private_channel() {
  let app = app!();
  let (alice, ann) = (owner(), member("ann"));
  let device = register_device(&app, &alice).await;
  let space = create_space(&app, &alice, "Team", "open").await;
  let ann_id = user_id(&app, &ann).await;
  add_space_member(&app, &alice, &space, &ann_id, "member").await;
  let held = create_channel(&app, &ann, &space, "held", "private").await;
  let alone = create_channel(&app, &ann, &space, "alone", "private").await;
  sign(&app, &alice, &device, 0, |p| hold(p, &held, "Case 1")).await;

  let left = app
    .delete(
      &ann,
      &format!("/spaces/{space}/members/{ann_id}?delete_if_last=true"),
    )
    .await;
  assert_eq!(left.status, StatusCode::NO_CONTENT, "{}", left.body);

  assert!(!is_there(&app, &alone).await);
  assert!(is_there(&app, &held).await);
  app.stop().await;
}

#[tokio::test]
async fn placing_and_releasing_are_audited_with_the_signing_owner() {
  let app = app!();
  let alice = owner();
  let (_, channel, device) = space_of_alice(&app, &alice).await;
  let alice_id = user_id(&app, &alice).await;
  sign(&app, &alice, &device, 0, |p| hold(p, &channel, "Case 1")).await;
  // Re-signing with the same hold audits nothing new.
  sign(&app, &alice, &device, 1, |p| hold(p, &channel, "Case 1")).await;
  sign(&app, &alice, &device, 2, |_| {}).await;

  for (action, version) in [("legal_hold_placed", 1), ("legal_hold_released", 3)] {
    let audit = app.get(&alice, &format!("/admin/audit?action={action}")).await;
    let entries = audit.body["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "{action}: {}", audit.body);
    assert_eq!(entries[0]["actor_user_id"], json!(alice_id));
    assert_eq!(entries[0]["target_kind"], "conversation");
    assert_eq!(entries[0]["target_id"], json!(channel));
    let detail: Value = serde_json::from_str(entries[0]["detail"].as_str().unwrap()).unwrap();
    assert_eq!(detail["reference"], "Case 1");
    assert_eq!(detail["policy_version"], version);
  }
  app.stop().await;
}

/// A space of alice's with one private channel, and alice's device: (space, channel, device).
async fn space_of_alice(app: &TestApp, alice: &TestUser) -> (String, String, String) {
  let device = register_device(app, alice).await;
  let space = create_space(app, alice, "Team", "invite_only").await;
  let channel = create_channel(app, alice, &space, "general", "private").await;
  (space, channel, device)
}

fn hold(policy: &mut Value, channel: &str, reference: &str) {
  let holds = &mut policy["compliance_archive"]["legal_holds"];
  if !holds.is_array() {
    *holds = json!([]);
  }
  holds.as_array_mut().unwrap().push(json!({
    "conversation_id": channel,
    "reference": reference,
    "placed_at": 1_700_000_000_000_i64,
  }));
}

/// Appends a policy signed by `user`'s device after version `expected`, from the default policy
/// with `edit` applied. The server doesn't verify the signature; clients do.
async fn sign(
  app: &TestApp,
  user: &TestUser,
  device: &str,
  expected: u64,
  edit: impl FnOnce(&mut Value),
) -> Reply {
  let mut value = serde_json::to_value(PolicyDocument {
    version: expected + 1,
    policy: WorkspacePolicy::default(),
  })
  .unwrap();
  edit(&mut value["policy"]);
  app
    .post(
      user,
      "/workspace/policy",
      json!({
        "expected_version": expected,
        "payload": b64(&serde_json::to_vec(&value).unwrap()),
        "signature": b64(b"sig"),
        "signer_device_id": device,
      }),
    )
    .await
}

async fn message_count(app: &TestApp, user: &TestUser, channel: &str) -> usize {
  app
    .get(user, &format!("/conversations/{channel}/messages?limit=200"))
    .await
    .body["messages"]
    .as_array()
    .unwrap()
    .len()
}

fn b64(bytes: &[u8]) -> String {
  use base64::Engine;
  base64::engine::general_purpose::STANDARD.encode(bytes)
}

async fn is_there(app: &TestApp, channel: &str) -> bool {
  let found: Option<i64> =
    sqlx::query_scalar("SELECT id FROM conversations WHERE id = $1 AND deleted_at IS NULL")
      .bind(channel.parse::<i64>().unwrap())
      .fetch_optional(app.sql_pool())
      .await
      .unwrap();
  found.is_some()
}
