//! REST API against a real server and Postgres (`--features integration`, needs `DATABASE_URL`).
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use reqwest::{Method, StatusCode};
use serde_json::json;
use teamcraft_server::testing::TestApp;

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

mod authentication_and_devices {
  use super::*;

  #[tokio::test]
  async fn requests_without_a_token_get_an_error_body() {
    let app = app!();
    let reply = app.call(Method::GET, None, "/spaces", None).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(reply.error_code(), "unauthorized");
    app.stop().await;
  }

  #[tokio::test]
  async fn users_without_a_teamcraft_role_are_rejected() {
    let app = app!();
    let nobody = teamcraft_server::testing::TestUser::new("frank", &[]);
    let reply = app.get(&nobody, "/me").await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(reply.error_code(), "no_teamcraft_role");
    app.stop().await;
  }

  #[tokio::test]
  async fn malformed_bodies_and_ids_get_an_error_body_too() {
    let app = app!();
    let alice = owner();
    let bad_json = app
      .call(Method::POST, Some(&alice), "/spaces", Some(json!({"nope": 1})))
      .await;
    assert_eq!(bad_json.status, StatusCode::BAD_REQUEST);
    assert_eq!(bad_json.error_code(), "invalid_request");
    let bad_id = app.get(&alice, "/spaces/not-a-number").await;
    assert_eq!(bad_id.status, StatusCode::BAD_REQUEST);
    assert_eq!(bad_id.error_code(), "invalid_request");
    app.stop().await;
  }

  #[tokio::test]
  async fn devices_can_be_registered_listed_and_revoked() {
    let app = app!();
    let carol = member("carol");
    let device = register_device(&app, &carol).await;

    let listed = app.get(&carol, "/devices").await;
    assert_eq!(listed.body["devices"].as_array().unwrap().len(), 1);

    // Somebody else can't revoke it.
    let dave = member("dave");
    assert_eq!(
      app.delete(&dave, &format!("/devices/{device}")).await.status,
      StatusCode::NOT_FOUND
    );
    assert_eq!(
      app.delete(&carol, &format!("/devices/{device}")).await.status,
      StatusCode::NO_CONTENT
    );
    assert_eq!(
      app.get(&carol, "/devices").await.body["devices"]
        .as_array()
        .unwrap()
        .len(),
      0
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn device_keys_are_validated() {
    let app = app!();
    let carol = member("carol");
    let empty_key = app
      .post(
        &carol,
        "/devices",
        json!({"name": "x", "public_key": "", "cross_signature": "c2ln"}),
      )
      .await;
    assert_eq!(empty_key.status, StatusCode::BAD_REQUEST);
    let not_base64 = app
      .post(
        &carol,
        "/devices",
        json!({"name": "x", "public_key": "%%%", "cross_signature": "c2ln"}),
      )
      .await;
    assert_eq!(not_base64.status, StatusCode::BAD_REQUEST);
    app.stop().await;
  }

  #[tokio::test]
  async fn openapi_document_lists_the_endpoints() {
    let app = app!();
    let doc = app
      .http
      .get(format!("{}/api/v1/openapi.json", app.base_url))
      .send()
      .await
      .unwrap();
    let doc: serde_json::Value = doc.json().await.unwrap();
    for path in [
      "/api/v1/me",
      "/api/v1/spaces",
      "/api/v1/conversations/{conversation_id}/messages",
      "/api/v1/dms",
    ] {
      assert!(doc["paths"][path].is_object(), "missing {path}");
    }
    assert!(doc["components"]["securitySchemes"]["bearer"].is_object());
    app.stop().await;
  }
}

mod spaces {
  use super::*;

  #[tokio::test]
  async fn only_admins_create_spaces() {
    let app = app!();
    let carol = member("carol");
    let denied = app
      .post(&carol, "/spaces", json!({"name": "X", "join_policy": "open"}))
      .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    assert_eq!(denied.error_code(), "forbidden");
    create_space(&app, &admin(), "Project A", "open").await;
    app.stop().await;
  }

  #[tokio::test]
  async fn open_spaces_can_be_joined_invite_only_spaces_are_hidden() {
    let app = app!();
    let (alice, carol) = (owner(), member("carol"));
    let open = create_space(&app, &alice, "Open", "open").await;
    let secret = create_space(&app, &alice, "Secret", "invite_only").await;

    let listed = app.get(&carol, "/spaces").await;
    let names: Vec<_> = listed.body["spaces"]
      .as_array()
      .unwrap()
      .iter()
      .map(|s| s["name"].clone())
      .collect();
    assert_eq!(names, vec![json!("Open")]);
    assert_eq!(
      app.get(&carol, &format!("/spaces/{secret}")).await.error_code(),
      "space_not_found"
    );
    assert_eq!(
      app
        .post(&carol, &format!("/spaces/{secret}/join"), json!({}))
        .await
        .status,
      StatusCode::NOT_FOUND
    );

    let joined = app.post(&carol, &format!("/spaces/{open}/join"), json!({})).await;
    assert_eq!(joined.status, StatusCode::OK, "{}", joined.body);
    assert_eq!(joined.body["my_role"], "member");
    app.stop().await;
  }

  #[tokio::test]
  async fn admins_see_every_space() {
    let app = app!();
    let (alice, bob) = (owner(), admin());
    create_space(&app, &alice, "Hidden", "invite_only").await;
    let listed = app.get(&bob, "/spaces").await;
    assert_eq!(listed.body["spaces"].as_array().unwrap().len(), 1);
    app.stop().await;
  }

  #[tokio::test]
  async fn space_admins_manage_members_but_keep_one_admin() {
    let app = app!();
    let (alice, carol, dave) = (owner(), member("carol"), member("dave"));
    let (alice_id, carol_id, dave_id) = (
      user_id(&app, &alice).await,
      user_id(&app, &carol).await,
      user_id(&app, &dave).await,
    );
    let space = create_space(&app, &alice, "Team", "invite_only").await;

    // Plain members can't add anybody.
    add_space_member(&app, &alice, &space, &carol_id, "member").await;
    let denied = app
      .post(
        &carol,
        &format!("/spaces/{space}/members"),
        json!({"user_id": dave_id, "role": "member"}),
      )
      .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);

    // Promote carol, then alice may step down; the last admin may not.
    let promoted = app
      .patch(
        &alice,
        &format!("/spaces/{space}/members/{carol_id}"),
        json!({"role": "space_admin"}),
      )
      .await;
    assert_eq!(promoted.status, StatusCode::OK, "{}", promoted.body);
    let step_down = app
      .patch(
        &alice,
        &format!("/spaces/{space}/members/{alice_id}"),
        json!({"role": "member"}),
      )
      .await;
    assert_eq!(step_down.status, StatusCode::OK, "{}", step_down.body);
    let last = app
      .delete(&carol, &format!("/spaces/{space}/members/{carol_id}"))
      .await;
    assert_eq!(last.status, StatusCode::CONFLICT);
    assert_eq!(last.error_code(), "conflict");
    app.stop().await;
  }

  #[tokio::test]
  async fn leaving_a_space_removes_the_channel_memberships() {
    let app = app!();
    let (alice, carol) = (owner(), member("carol"));
    let carol_id = user_id(&app, &carol).await;
    let space = create_space(&app, &alice, "Team", "open").await;
    let channel = create_channel(&app, &alice, &space, "general", "public").await;
    app
      .post(&carol, &format!("/spaces/{space}/join"), json!({}))
      .await;
    assert_eq!(
      app
        .post(&carol, &format!("/conversations/{channel}/join"), json!({}))
        .await
        .status,
      StatusCode::NO_CONTENT
    );

    assert_eq!(
      app
        .delete(&carol, &format!("/spaces/{space}/members/{carol_id}"))
        .await
        .status,
      StatusCode::NO_CONTENT
    );
    assert_eq!(
      app.get(&carol, "/conversations").await.body["conversations"]
        .as_array()
        .unwrap()
        .len(),
      0
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn categories_are_managed_by_space_admins_and_assigned_to_channels() {
    let app = app!();
    let (alice, carol) = (owner(), member("carol"));
    let carol_id = user_id(&app, &carol).await;
    let space = create_space(&app, &alice, "Team", "invite_only").await;
    add_space_member(&app, &alice, &space, &carol_id, "member").await;

    let denied = app
      .post(
        &carol,
        &format!("/spaces/{space}/categories"),
        json!({"name": "Dev"}),
      )
      .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    let category = app
      .post(
        &alice,
        &format!("/spaces/{space}/categories"),
        json!({"name": "Dev"}),
      )
      .await;
    assert_eq!(category.status, StatusCode::CREATED);
    let category_id = category.id();

    let channel = app
      .post(
        &alice,
        &format!("/spaces/{space}/channels"),
        json!({"name": "backend", "visibility": "public", "category_id": category_id}),
      )
      .await;
    assert_eq!(channel.status, StatusCode::CREATED, "{}", channel.body);
    assert_eq!(channel.body["category_id"], category_id);

    let listed = app.get(&carol, &format!("/spaces/{space}/categories")).await;
    assert_eq!(listed.body["categories"][0]["name"], "Dev");
    let renamed = app
      .patch(
        &alice,
        &format!("/spaces/{space}/categories/{category_id}"),
        json!({"name": "Engineering"}),
      )
      .await;
    assert_eq!(renamed.body["name"], "Engineering");

    // A category of another space can't be used.
    let other = create_space(&app, &alice, "Other", "invite_only").await;
    let wrong = app
      .post(
        &alice,
        &format!("/spaces/{other}/channels"),
        json!({"name": "x", "visibility": "public", "category_id": category_id}),
      )
      .await;
    assert_eq!(wrong.error_code(), "category_not_found");

    assert_eq!(
      app
        .delete(&alice, &format!("/spaces/{space}/categories/{category_id}"))
        .await
        .status,
      StatusCode::NO_CONTENT
    );
    let channel_id = channel.id();
    let after = app.get(&alice, &format!("/conversations/{channel_id}")).await;
    assert!(after.body.get("category_id").is_none());
    app.stop().await;
  }

  #[tokio::test]
  async fn space_properties_can_be_changed_and_archived() {
    let app = app!();
    let (alice, carol) = (owner(), member("carol"));
    let space = create_space(&app, &alice, "Team", "open").await;
    let updated = app
      .patch(
        &alice,
        &format!("/spaces/{space}"),
        json!({"name": "Team 2", "color": "#ff0000", "description": "Hi"}),
      )
      .await;
    assert_eq!(updated.body["name"], "Team 2");
    assert_eq!(updated.body["color"], "#ff0000");
    let cleared = app
      .patch(&alice, &format!("/spaces/{space}"), json!({"color": ""}))
      .await;
    assert!(cleared.body.get("color").is_none());

    assert_eq!(
      app
        .patch(&carol, &format!("/spaces/{space}"), json!({"name": "Mine"}))
        .await
        .status,
      StatusCode::FORBIDDEN
    );
    app
      .patch(&alice, &format!("/spaces/{space}"), json!({"is_archived": true}))
      .await;
    let join = app
      .post(&carol, &format!("/spaces/{space}/join"), json!({}))
      .await;
    assert_eq!(
      join.status,
      StatusCode::NOT_FOUND,
      "archived spaces disappear for non-members"
    );
    app.stop().await;
  }
}
