//! Admins asking the owners of a private channel to let them in (admin.md §2.4.1, issue #776):
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

mod approving {
  use super::*;

  #[tokio::test]
  async fn an_owner_approves_a_request_and_the_admin_is_in() {
    let app = app!();
    let setup = Setup::new(&app, "owner_approval").await;
    let (alice, bob) = (owner(), admin());
    let mut owner_socket = socket(&app, &alice).await;

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
    assert_eq!(listed(&app, &alice).await, vec![setup.bob_id.clone()]);
    assert_eq!(listed(&app, &bob).await, vec![setup.bob_id.clone()]);

    let approved = app.post(&alice, &setup.approve_path(), json!({})).await;
    assert_eq!(approved.status, StatusCode::OK, "{}", approved.body);
    assert!(setup.is_member(&app, &setup.bob_id).await);
    assert!(listed(&app, &alice).await.is_empty(), "settled");
    assert!(update_of(
      &events(&mut owner_socket, WAIT).await,
      AdminJoinRequestChange::Approved
    ));
    let audit = app
      .get(&alice, "/admin/audit?action=admin_join_request_approved")
      .await;
    assert_eq!(
      audit.body["entries"].as_array().unwrap().len(),
      1,
      "{}",
      audit.body
    );

    // The same owner's retry after a lost answer goes on, and tells nobody again.
    let again = app.post(&alice, &setup.approve_path(), json!({})).await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.body);
    let after_retry = events(&mut owner_socket, WAIT).await;
    assert!(
      !after_retry.iter().any(|event| matches!(
        event,
        GatewayEvent::AdminJoinRequestUpdate { .. } | GatewayEvent::MembershipAdd { .. }
      )),
      "{after_retry:?}"
    );
    app.stop().await;
  }

  /// Review M2: of two owners exactly one approves; the other is told it is gone, and a decline
  /// after the approval changes nothing.
  #[tokio::test]
  async fn a_second_owner_neither_approves_again_nor_declines_an_approval() {
    let app = app!();
    let setup = Setup::new(&app, "owner_approval").await;
    let (alice, carol) = (owner(), member("carol"));
    setup.make_owner(&app, &setup.carol_id).await;

    setup.ask(&app).await;
    let first = app.post(&alice, &setup.approve_path(), json!({})).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    let second = app.post(&carol, &setup.approve_path(), json!({})).await;
    assert_eq!(
      (second.status, second.error_code()),
      (StatusCode::NOT_FOUND, "admin_join_request_not_found")
    );
    let late_decline = app.delete(&carol, &setup.request_path()).await;
    assert_eq!(late_decline.status, StatusCode::NO_CONTENT);
    assert!(setup.is_member(&app, &setup.bob_id).await, "the approval stands");
    app.stop().await;
  }

  /// Review test gap: only the channel's owners decide; a member or a workspace admin outside the
  /// channel can't approve.
  #[tokio::test]
  async fn only_an_owner_of_the_channel_approves() {
    let app = app!();
    let setup = Setup::new(&app, "owner_approval").await;
    let carol = member("carol");
    let dave = TestUser::new("dave", &["admin"]);
    user_id(&app, &dave).await;

    setup.ask(&app).await;
    for outsider in [&carol, &dave] {
      let refused = app.post(outsider, &setup.approve_path(), json!({})).await;
      assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
    }
    assert!(!setup.is_member(&app, &setup.bob_id).await);
    assert_eq!(listed(&app, &owner()).await.len(), 1, "still open");
    app.stop().await;
  }

  /// Review L7: somebody who is no admin any more (their last sign-in's role) isn't let in or
  /// listed.
  #[tokio::test]
  async fn a_requester_who_lost_the_admin_role_is_not_let_in() {
    let app = app!();
    let setup = Setup::new(&app, "owner_approval").await;
    let alice = owner();

    setup.ask(&app).await;
    teamcraft_db::testing::sqlx::query("UPDATE users SET oidc_role = 'member' WHERE username = 'bob'")
      .execute(app.sql_pool())
      .await
      .unwrap();
    assert!(listed(&app, &alice).await.is_empty());
    let refused = app.post(&alice, &setup.approve_path(), json!({})).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
    assert!(!setup.is_member(&app, &setup.bob_id).await);
    app.stop().await;
  }

  #[tokio::test]
  async fn an_archived_channel_takes_no_requests_and_no_approvals() {
    let app = app!();
    let setup = Setup::new(&app, "owner_approval").await;
    let alice = owner();

    setup.ask(&app).await;
    let archived = app
      .patch(
        &alice,
        &format!("/conversations/{}", setup.channel),
        json!({"is_archived": true}),
      )
      .await;
    assert_eq!(archived.status, StatusCode::OK, "{}", archived.body);
    assert!(listed(&app, &alice).await.is_empty());
    let approve = app.post(&alice, &setup.approve_path(), json!({})).await;
    assert_eq!(approve.status, StatusCode::CONFLICT, "{}", approve.body);
    assert_eq!(setup.ask(&app).await.status, StatusCode::CONFLICT);
    assert!(!setup.is_member(&app, &setup.bob_id).await);
    app.stop().await;
  }
}

mod asking {
  use super::*;

  /// Review test gap: the owners and the admin hear of a request, plain members don't.
  #[tokio::test]
  async fn asking_twice_keeps_one_request_and_tells_only_owners_and_admin_once() {
    let app = app!();
    let setup = Setup::new(&app, "owner_approval").await;
    let (alice, bob, carol) = (owner(), admin(), member("carol"));
    let mut owner_socket = socket(&app, &alice).await;
    let mut admin_socket = socket(&app, &bob).await;
    let mut member_socket = socket(&app, &carol).await;

    let first = setup.ask(&app).await;
    let second = setup.ask(&app).await;
    assert_eq!(second.status, StatusCode::CREATED, "{}", second.body);
    assert_eq!(first.body["requested_at"], second.body["requested_at"]);
    let seen = events(&mut owner_socket, WAIT).await;
    let requested = seen
      .iter()
      .filter(|event| update_of(std::slice::from_ref(event), AdminJoinRequestChange::Requested))
      .count();
    assert_eq!(requested, 1, "{seen:?}");
    assert!(update_of(
      &events(&mut admin_socket, WAIT).await,
      AdminJoinRequestChange::Requested
    ));
    let at_member = events(&mut member_socket, WAIT).await;
    assert!(
      !at_member
        .iter()
        .any(|event| matches!(event, GatewayEvent::AdminJoinRequestUpdate { .. })),
      "{at_member:?}"
    );
    assert!(listed(&app, &carol).await.is_empty());
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
}

mod closing {
  use super::*;

  #[tokio::test]
  async fn declining_closes_the_request_once_and_approving_it_later_fails() {
    let app = app!();
    let setup = Setup::new(&app, "owner_approval").await;
    let (alice, bob, carol) = (owner(), admin(), member("carol"));

    setup.ask(&app).await;
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
    let late = app.post(&alice, &setup.approve_path(), json!({})).await;
    assert_eq!(
      (late.status, late.error_code()),
      (StatusCode::NOT_FOUND, "admin_join_request_not_found")
    );
    assert!(!setup.is_member(&app, &setup.bob_id).await);
    app.stop().await;
  }

  /// Review L6: asking, withdrawing and asking again can't notify the owners over and over; the
  /// admin asks again once the request would have expired.
  #[tokio::test]
  async fn after_a_withdrawal_or_a_decline_the_admin_waits_until_the_request_would_expire() {
    let app = app!();
    let setup = Setup::new(&app, "owner_approval").await;
    let (alice, bob) = (owner(), admin());

    setup.ask(&app).await;
    let withdrawn = app.delete(&bob, &setup.request_path()).await;
    assert_eq!(withdrawn.status, StatusCode::NO_CONTENT, "{}", withdrawn.body);
    assert!(listed(&app, &alice).await.is_empty());
    let again = setup.ask(&app).await;
    assert_eq!(
      (again.status, again.error_code()),
      (StatusCode::CONFLICT, "admin_join_request_too_soon")
    );

    expire_all(&app).await;
    teamcraft_server::services::admin_join_requests::sweep_expired(&app.state)
      .await
      .unwrap();
    assert_eq!(setup.ask(&app).await.status, StatusCode::CREATED);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_policy_without_owner_approval_closes_open_requests() {
    let app = app!();
    let setup = Setup::new(&app, "owner_approval").await;
    let (alice, bob) = (owner(), admin());
    let mut socket = socket(&app, &bob).await;

    setup.ask(&app).await;
    publish_self_join(&app, 2, "visible").await;
    assert!(listed(&app, &alice).await.is_empty());
    assert!(update_of(
      &events(&mut socket, WAIT).await,
      AdminJoinRequestChange::Expired
    ));
    let audit = app
      .get(&alice, "/admin/audit?action=admin_join_request_expired")
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

    expire_all(&app).await;
    assert!(
      listed(&app, &alice).await.is_empty(),
      "expired requests are not listed"
    );
    let closed = teamcraft_server::services::admin_join_requests::sweep_expired(&app.state)
      .await
      .unwrap();
    assert_eq!(closed, 1);
    let audit = app
      .get(&alice, "/admin/audit?action=admin_join_request_expired")
      .await;
    assert_eq!(
      audit.body["entries"].as_array().unwrap().len(),
      1,
      "{}",
      audit.body
    );
    // Asking again opens a new request.
    let again = setup.ask(&app).await;
    assert_eq!(again.status, StatusCode::CREATED, "{}", again.body);
    assert_ne!(again.body["requested_at"], asked.body["requested_at"]);
    app.stop().await;
  }
}

/// A space with a private channel `leads` owned by Alice (owner, root of trust), Carol a member of
/// it, and Bob (admin) in the space but not in the channel; the policy as given.
struct Setup {
  channel: String,
  public: String,
  bob_id: String,
  carol_id: String,
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
      carol_id,
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

  async fn make_owner(&self, app: &TestApp, user: &str) {
    let reply = app
      .patch(
        &owner(),
        &format!("/conversations/{}/members/{user}", self.channel),
        json!({"role": "admin"}),
      )
      .await;
    assert!(reply.status.is_success(), "{}", reply.body);
  }

  async fn is_member(&self, app: &TestApp, user: &str) -> bool {
    app
      .get(&owner(), &format!("/conversations/{}/members", self.channel))
      .await
      .body["members"]
      .as_array()
      .unwrap()
      .iter()
      .any(|member| member["user"]["id"] == user)
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

async fn socket(app: &TestApp, user: &TestUser) -> common::ws::Socket {
  let device = register_device(app, user).await;
  connect(app, user, &device).await
}

/// Long after every request expired, as far as the database's clock is concerned.
async fn expire_all(app: &TestApp) {
  teamcraft_db::testing::sqlx::query(
    "UPDATE admin_join_requests SET requested_at = requested_at - interval '100 days',
                                    expires_at = expires_at - interval '100 days'",
  )
  .execute(app.sql_pool())
  .await
  .unwrap();
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
