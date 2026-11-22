//! Legal hold (issue #211, data-export.md §4.2) against a real server and Postgres: who may place
//! one, what the server refuses, that retention and deletions leave a held conversation alone, that a
//! person's hold covers their conversations and keeps their direct messages through an account
//! deletion, that a withdrawn message waits for the release (issue #2135), and that placing and
//! releasing are audited.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_db::DeletionGuard;
use teamcraft_db::testing::sqlx;
use teamcraft_proto::{PolicyDocument, WorkspacePolicy};
use teamcraft_server::services::admin::{delete_account, delete_leftover_direct_conversations};
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
async fn only_existing_channels_and_direct_messages_can_be_held() {
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
  // An unknown ID, and an unknown person.
  let unknown = sign(&app, &alice, &device, 0, |p| hold(p, "999999999", "Case 1")).await;
  assert_eq!(unknown.status, StatusCode::BAD_REQUEST, "{}", unknown.body);
  let nobody = sign(&app, &alice, &device, 0, |p| {
    hold_person(p, "999999999", "Case 1", None)
  })
  .await;
  assert_eq!(nobody.status, StatusCode::BAD_REQUEST, "{}", nobody.body);
  // A channel, a DM and a group (issue #2135): account deletion keeps them now.
  let placed = sign(&app, &alice, &device, 0, |p| {
    hold(p, &channel, "Case 1");
    hold(p, &dm.id(), "Case 1");
    hold(p, &group.id(), "Case 1");
  })
  .await;
  assert_eq!(placed.status, StatusCode::CREATED, "{}", placed.body);
  app.stop().await;
}

/// A person's hold covers the direct message they are in: deleting the other participant's account
/// keeps it with its messages until the hold is released; then the housekeeping deletes it.
#[tokio::test]
async fn a_person_hold_keeps_their_direct_messages_through_an_account_deletion() {
  let app = app!();
  let (alice, dave, erin) = (owner(), member("dave"), member("erin"));
  let alice_device = register_device(&app, &alice).await;
  let dave_device = register_device(&app, &dave).await;
  let (dave_id, erin_id) = (user_id(&app, &dave).await, user_id(&app, &erin).await);
  let dm = app.post(&dave, "/dms", json!({"user_id": erin_id})).await.id();
  send(&app, &dave, &dm, &dave_device, 1).await;
  let held = sign(&app, &alice, &alice_device, 0, |p| {
    hold_person(p, &dave_id, "Case 9", Some("direct"));
  })
  .await;
  assert_eq!(held.status, StatusCode::CREATED, "{}", held.body);

  delete_account(
    &app.state,
    None,
    erin_id.parse().unwrap(),
    DeletionGuard::Any,
    &json!({}),
  )
  .await
  .unwrap();
  assert!(row_exists(&app, &dm).await);
  assert_eq!(message_count(&app, &dave, &dm).await, 1);
  // The housekeeping leaves it alone while held.
  assert_eq!(delete_leftover_direct_conversations(&app.state).await.unwrap(), 0);
  assert!(row_exists(&app, &dm).await);

  let released = sign(&app, &alice, &alice_device, 1, |_| {}).await;
  assert_eq!(released.status, StatusCode::CREATED, "{}", released.body);
  assert_eq!(delete_leftover_direct_conversations(&app.state).await.unwrap(), 1);
  assert!(!row_exists(&app, &dm).await);

  let audit = app.get(&alice, "/admin/audit?action=legal_hold_placed").await;
  let entries = audit.body["entries"].as_array().unwrap();
  assert_eq!(entries.len(), 1, "{}", audit.body);
  assert_eq!(entries[0]["target_kind"], "user");
  assert_eq!(entries[0]["target_id"], json!(dave_id));
  let detail: Value = serde_json::from_str(entries[0]["detail"].as_str().unwrap()).unwrap();
  assert_eq!(detail["scope"], "direct");
  app.stop().await;
}

/// A person's hold on a channel member keeps the channel from deletion and retention, like a hold
/// on the channel; a hold on their direct messages only doesn't.
#[tokio::test]
async fn a_person_hold_covers_the_channels_they_are_in() {
  let app = app!();
  let alice = owner();
  let (_, channel, device) = space_of_alice(&app, &alice).await;
  let alice_id = user_id(&app, &alice).await;
  sign(&app, &alice, &device, 0, |p| {
    hold_person(p, &alice_id, "Case 2", Some("direct"))
  })
  .await;
  sign(&app, &alice, &device, 1, |p| {
    hold_person(p, &alice_id, "Case 2", None)
  })
  .await;
  let refused = app.delete(&alice, &format!("/conversations/{channel}")).await;
  assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.body);
  assert_eq!(refused.body["error_code"], "legal_hold");

  sign(&app, &alice, &device, 2, |p| {
    hold_person(p, &alice_id, "Case 2", Some("direct"))
  })
  .await;
  let deleted = app.delete(&alice, &format!("/conversations/{channel}")).await;
  assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.body);
  app.stop().await;
}

/// A member's deletion of their own message (`deletes`): without a hold the ciphertext goes at once;
/// under one it is marked, hidden from history, kept, and removed after the release. Somebody else's
/// message is never touched, and a repeat changes nothing.
#[tokio::test]
async fn a_withdrawn_message_waits_for_the_release() {
  let app = app!();
  let (alice, carol) = (owner(), member("carol"));
  let (space, channel, device) = space_of_alice(&app, &alice).await;
  let carol_id = user_id(&app, &carol).await;
  let carol_device = register_device(&app, &carol).await;
  add_space_member(&app, &alice, &space, &carol_id, "member").await;
  add_channel_member(&app, &alice, &channel, &carol_id).await;
  send(&app, &alice, &channel, &device, 1).await;
  send(&app, &alice, &channel, &device, 2).await;
  send(&app, &carol, &channel, &carol_device, 3).await;

  // Carol can't withdraw alice's message.
  withdraw(&app, &carol, &channel, &carol_device, 10, 1).await;
  assert_eq!(stored_messages(&app, &channel).await, 4);
  // Without a hold the withdrawn message goes at once.
  withdraw(&app, &alice, &channel, &device, 11, 2).await;
  assert_eq!(message_count(&app, &alice, &channel).await, 4);
  assert_eq!(stored_messages(&app, &channel).await, 4);

  sign(&app, &alice, &device, 0, |p| hold(p, &channel, "Case 3")).await;
  for _ in 0..2 {
    withdraw(&app, &alice, &channel, &device, 12, 1).await;
  }
  // Hidden from every member, kept on the server.
  assert_eq!(message_count(&app, &carol, &channel).await, 4);
  assert_eq!(stored_messages(&app, &channel).await, 5);
  let report = run_once(&app.state, OffsetDateTime::now_utc()).await.unwrap();
  assert_eq!(report.held_deletions_applied, 0);
  assert_eq!(stored_messages(&app, &channel).await, 5);

  // The release applies it.
  sign(&app, &alice, &device, 1, |_| {}).await;
  assert_eq!(stored_messages(&app, &channel).await, 4);
  let audit = app
    .get(&alice, "/admin/audit?action=legal_hold_deletions_applied")
    .await;
  assert_eq!(
    audit.body["entries"].as_array().unwrap().len(),
    1,
    "{}",
    audit.body
  );
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

fn hold_person(policy: &mut Value, user: &str, reference: &str, maybe_scope: Option<&str>) {
  let people = &mut policy["compliance_archive"]["person_holds"];
  *people = json!([]);
  let mut hold = json!({
    "user_id": user,
    "reference": reference,
    "placed_at": 1_700_000_000_000_i64,
  });
  if let Some(scope) = maybe_scope {
    hold["scope"] = json!(scope);
  }
  people.as_array_mut().unwrap().push(hold);
}

/// Sends message `n` that withdraws the sender's message `target` (both by their test client IDs).
async fn withdraw(app: &TestApp, user: &TestUser, conversation: &str, device: &str, n: u8, target: u8) {
  let mut body = message_body(n, device);
  body["deletes"] = json!(client_id(target));
  let reply = app
    .post(user, &format!("/conversations/{conversation}/messages"), body)
    .await;
  assert!(reply.status.is_success(), "{}", reply.body);
}

/// Every message row of the conversation, also those hidden from history.
async fn stored_messages(app: &TestApp, conversation: &str) -> i64 {
  sqlx::query_scalar("SELECT count(*) FROM messages WHERE conversation_id = $1")
    .bind(conversation.parse::<i64>().unwrap())
    .fetch_one(app.sql_pool())
    .await
    .unwrap()
}

async fn row_exists(app: &TestApp, conversation: &str) -> bool {
  let found: Option<i64> = sqlx::query_scalar("SELECT id FROM conversations WHERE id = $1")
    .bind(conversation.parse::<i64>().unwrap())
    .fetch_optional(app.sql_pool())
    .await
    .unwrap();
  found.is_some()
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
