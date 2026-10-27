//! Admins asking the owners of a private channel to let them in (admin.md §2.4, issue #776):
//! `--features integration`, needs `DATABASE_URL`.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use base64::Engine;
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_proto::{AdminJoinRequestChange, GatewayEvent, PolicyDocument, WorkspacePolicy};
use teamcraft_server::testing::{TestApp, TestUser};

use crate::common;
use common::ws::{connect, events};
use common::*;

const WAIT: Duration = Duration::from_millis(300);

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

#[tokio::test]
async fn an_owner_approves_a_request_and_the_admin_is_in() {
  let app = app!();
  let setup = Setup::new(&app, "owner_approval").await;
  let (alice, bob) = (owner(), admin());
  let owner_device = register_device(&app, &alice).await;
  let mut owner_socket = connect(&app, &alice, &owner_device).await;

  let asked = setup.ask(&app).await;
  assert_eq!(asked.status, StatusCode::CREATED, "{}", asked.body);
  assert_eq!(asked.body["user"]["id"], setup.bob_id.as_str());
  assert!(asked.body["expires_at"].as_i64() > asked.body["requested_at"].as_i64());
  assert!(
    update_of(
      &events(&mut owner_socket, WAIT).await,
      AdminJoinRequestChange::Requested
    ),
    "the owner is told"
  );

  // The owner and the admin see it; a plain member of the channel does not.
  assert_eq!(listed(&app, &alice).await, vec![setup.bob_id.clone()]);
  assert_eq!(listed(&app, &bob).await, vec![setup.bob_id.clone()]);
  assert!(listed(&app, &member("carol")).await.is_empty());

  let approved = app.post(&alice, &setup.approve_path(), json!({})).await;
  assert_eq!(approved.status, StatusCode::OK, "{}", approved.body);
  let members = app
    .get(&alice, &format!("/conversations/{}/members", setup.channel))
    .await;
  assert!(members.body.to_string().contains(&setup.bob_id));
  assert!(listed(&app, &alice).await.is_empty(), "settled");
  assert!(update_of(
    &events(&mut owner_socket, WAIT).await,
    AdminJoinRequestChange::Approved
  ));
  // A retry after a lost answer finds the admin in and goes on.
  let again = app.post(&alice, &setup.approve_path(), json!({})).await;
  assert_eq!(again.status, StatusCode::OK, "{}", again.body);
  app.stop().await;
}

#[tokio::test]
async fn asking_twice_keeps_one_request_and_tells_the_owners_once() {
  let app = app!();
  let setup = Setup::new(&app, "owner_approval").await;
  let alice = owner();
  let owner_device = register_device(&app, &alice).await;
  let mut owner_socket = connect(&app, &alice, &owner_device).await;

  let first = setup.ask(&app).await;
  let second = setup.ask(&app).await;
  assert_eq!(second.status, StatusCode::CREATED, "{}", second.body);
  assert_eq!(first.body["requested_at"], second.body["requested_at"]);
  let seen = events(&mut owner_socket, WAIT).await;
  let requested = seen
    .iter()
    .filter(|event| {
      matches!(
        event,
        GatewayEvent::AdminJoinRequestUpdate {
          change: AdminJoinRequestChange::Requested,
          ..
        }
      )
    })
    .count();
  assert_eq!(requested, 1, "{seen:?}");
  assert_eq!(listed(&app, &alice).await.len(), 1);
  app.stop().await;
}

#[tokio::test]
async fn declining_and_withdrawing_close_the_request_once() {
  let app = app!();
  let setup = Setup::new(&app, "owner_approval").await;
  let (alice, bob, carol) = (owner(), admin(), member("carol"));

  setup.ask(&app).await;
  // Only the channel's owners decide.
  let by_member = app.delete(&carol, &setup.request_path()).await;
  assert_eq!(by_member.status, StatusCode::FORBIDDEN, "{}", by_member.body);
  let declined = app.delete(&alice, &setup.request_path()).await;
  assert_eq!(declined.status, StatusCode::NO_CONTENT, "{}", declined.body);
  assert!(listed(&app, &bob).await.is_empty());
  // Declined on another device already: still fine.
  assert_eq!(
    app.delete(&alice, &setup.request_path()).await.status,
    StatusCode::NO_CONTENT
  );
  // Approving what is gone says so.
  let late = app.post(&alice, &setup.approve_path(), json!({})).await;
  assert_eq!(
    (late.status, late.error_code()),
    (StatusCode::NOT_FOUND, "admin_join_request_not_found")
  );

  setup.ask(&app).await;
  let withdrawn = app.delete(&bob, &setup.request_path()).await;
  assert_eq!(withdrawn.status, StatusCode::NO_CONTENT, "{}", withdrawn.body);
  assert!(listed(&app, &alice).await.is_empty());
  app.stop().await;
}

#[tokio::test]
async fn only_admins_ask_and_only_under_owner_approval() {
  let app = app!();
  let setup = Setup::new(&app, "visible").await;
  let carol = member("carol");

  // Under `visible` the admin joins by themselves.
  let not_needed = setup.ask(&app).await;
  assert_eq!(not_needed.status, StatusCode::CONFLICT, "{}", not_needed.body);
  publish_self_join(&app, 2, "disabled").await;
  let disabled = setup.ask(&app).await;
  assert_eq!(disabled.status, StatusCode::FORBIDDEN, "{}", disabled.body);

  publish_self_join(&app, 3, "owner_approval").await;
  let by_member = app
    .post(
      &carol,
      &format!("/conversations/{}/admin-join-requests", setup.public),
      json!({}),
    )
    .await;
  assert_eq!(by_member.status, StatusCode::FORBIDDEN, "{}", by_member.body);
  let public = app
    .post(
      &admin(),
      &format!("/conversations/{}/admin-join-requests", setup.public),
      json!({}),
    )
    .await;
  assert_eq!(public.status, StatusCode::CONFLICT, "{}", public.body);
  assert_eq!(setup.ask(&app).await.status, StatusCode::CREATED);
  app.stop().await;
}

#[tokio::test]
async fn a_policy_without_owner_approval_closes_open_requests() {
  let app = app!();
  let setup = Setup::new(&app, "owner_approval").await;
  let bob = admin();
  let device = register_device(&app, &bob).await;
  let mut socket = connect(&app, &bob, &device).await;

  setup.ask(&app).await;
  publish_self_join(&app, 2, "visible").await;
  assert!(listed(&app, &owner()).await.is_empty());
  assert!(update_of(
    &events(&mut socket, WAIT).await,
    AdminJoinRequestChange::Expired
  ));
  app.stop().await;
}

#[tokio::test]
async fn requests_expire_after_the_configured_days() {
  let app = app!();
  let setup = Setup::new(&app, "owner_approval").await;
  let alice = owner();
  let mut rules = app.get(&alice, "/admin/settings").await.body["server_rules"].clone();
  assert_eq!(rules["admin_join_request_days"], 7, "the default");
  rules["admin_join_request_days"] = json!(2);
  let saved = app
    .put(
      &alice,
      "/admin/settings",
      json!({"section": "server_rules", "values": rules.clone()}),
    )
    .await;
  assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
  rules["admin_join_request_days"] = json!(0);
  let refused = app
    .put(
      &alice,
      "/admin/settings",
      json!({"section": "server_rules", "values": rules}),
    )
    .await;
  assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);

  let asked = setup.ask(&app).await;
  let lifetime = asked.body["expires_at"].as_i64().unwrap() - asked.body["requested_at"].as_i64().unwrap();
  assert_eq!(lifetime, 2 * 86_400_000);

  // Two days later (the database's clock decides).
  teamcraft_db::testing::sqlx::query(
    "UPDATE admin_join_requests SET requested_at = requested_at - interval '3 days',
                                    expires_at = expires_at - interval '3 days'",
  )
  .execute(app.sql_pool())
  .await
  .unwrap();
  assert!(
    listed(&app, &alice).await.is_empty(),
    "expired requests are not listed"
  );
  let closed = teamcraft_server::services::admin_join_requests::sweep_expired(&app.state)
    .await
    .unwrap();
  assert_eq!(closed, 1);
  // Asking again opens a new request.
  let again = setup.ask(&app).await;
  assert_eq!(again.status, StatusCode::CREATED, "{}", again.body);
  assert_ne!(again.body["requested_at"], asked.body["requested_at"]);
  app.stop().await;
}

/// A space with a private channel `leads` owned by Alice (owner, root of trust), Carol a member of
/// it, and Bob (admin) in the space but not in the channel; the policy as given.
struct Setup {
  channel: String,
  public: String,
  bob_id: String,
}

impl Setup {
  async fn new(app: &TestApp, self_join: &str) -> Self {
    let (alice, bob, carol) = (owner(), admin(), member("carol"));
    let (bob_id, carol_id) = (user_id(app, &bob).await, user_id(app, &carol).await);
    let space = create_space(app, &alice, "Team", "invite_only").await;
    add_space_member(app, &alice, &space, &bob_id, "member").await;
    add_space_member(app, &alice, &space, &carol_id, "member").await;
    let channel = create_channel(app, &alice, &space, "leads", "private").await;
    let public = create_channel(app, &alice, &space, "general", "public").await;
    add_channel_member(app, &alice, &channel, &carol_id).await;
    publish_self_join(app, 1, self_join).await;
    Self {
      channel,
      public,
      bob_id,
    }
  }

  async fn ask(&self, app: &TestApp) -> teamcraft_server::testing::Reply {
    app
      .post(
        &admin(),
        &format!("/conversations/{}/admin-join-requests", self.channel),
        json!({}),
      )
      .await
  }

  fn request_path(&self) -> String {
    format!(
      "/conversations/{}/admin-join-requests/{}",
      self.channel, self.bob_id
    )
  }

  fn approve_path(&self) -> String {
    format!("{}/approve", self.request_path())
  }
}

/// The admins whose requests `user` sees.
async fn listed(app: &TestApp, user: &TestUser) -> Vec<String> {
  let reply = app.get(user, "/admin-join-requests").await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  reply.body["requests"]
    .as_array()
    .unwrap()
    .iter()
    .map(|request| request["user"]["id"].as_str().unwrap().to_owned())
    .collect()
}

fn update_of(seen: &[GatewayEvent], expected: AdminJoinRequestChange) -> bool {
  seen
    .iter()
    .any(|event| matches!(event, GatewayEvent::AdminJoinRequestUpdate { change, .. } if *change == expected))
}

async fn publish_self_join(app: &TestApp, version: u64, setting: &str) {
  let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
  let alice = owner();
  let device = match app.get(&alice, "/devices").await.body["devices"]
    .as_array()
    .and_then(|devices| devices.first())
  {
    Some(device) => device["id"].as_str().unwrap().to_owned(),
    None => register_device(app, &alice).await,
  };
  let mut value = serde_json::to_value(PolicyDocument {
    version,
    policy: WorkspacePolicy::default(),
  })
  .unwrap();
  value["policy"]["admin_self_join"] = json!(setting);
  let reply = app
    .post(
      &alice,
      "/workspace/policy",
      json!({
        "expected_version": version - 1,
        "payload": b64(&serde_json::to_vec(&value).unwrap()),
        "signature": b64(b"sig"),
        "signer_device_id": device,
      }),
    )
    .await;
  assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
}
