//! Deleting an account (admin.md §3.2.1, issue #630) against a real server and Postgres.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::time::Duration;

use common::*;
use futures_util::{SinkExt, StreamExt};
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_proto::{DeviceActivity, GatewayClientMessage, GatewayEvent, GatewayServerMessage};
use teamcraft_server::testing::{TestApp, TestUser};
use tokio_tungstenite::tungstenite::Message;

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

mod permissions {
  use super::*;

  #[tokio::test]
  async fn only_admins_delete_and_never_themselves() {
    let app = app!();
    let (bob, carol, dave, erin) = (admin(), member("carol"), member("dave"), guest("erin"));
    let bob_id = user_id(&app, &bob).await;
    let carol_id = user_id(&app, &carol).await;
    user_id(&app, &erin).await;

    for (caller, who) in [(&dave, "a member"), (&erin, "a guest")] {
      let refused = app.delete(caller, &format!("/admin/users/{carol_id}")).await;
      assert_eq!(refused.status, StatusCode::FORBIDDEN, "{who}: {}", refused.body);
    }
    let own = app.delete(&bob, &format!("/admin/users/{bob_id}")).await;
    assert_eq!(
      (own.status, own.error_code()),
      (StatusCode::CONFLICT, "cannot_delete_self")
    );
    let unknown = app.delete(&bob, "/admin/users/123456789").await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);

    let deleted = app.delete(&bob, &format!("/admin/users/{carol_id}")).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.body);
    app.stop().await;
  }

  #[tokio::test]
  async fn only_owners_delete_admins_and_owners() {
    let app = app!();
    let (alice, bob) = (owner(), admin());
    let other_admin = TestUser::new("frank", &["admin"]);
    let other_owner = TestUser::new("grace", &["owner"]);
    let alice_id = user_id(&app, &alice).await;
    let frank_id = user_id(&app, &other_admin).await;
    let grace_id = user_id(&app, &other_owner).await;
    user_id(&app, &bob).await;

    let by_admin = app.delete(&bob, &format!("/admin/users/{frank_id}")).await;
    assert_eq!(by_admin.status, StatusCode::FORBIDDEN, "{}", by_admin.body);
    let by_admin = app.delete(&bob, &format!("/admin/users/{alice_id}")).await;
    assert_eq!(by_admin.status, StatusCode::FORBIDDEN, "{}", by_admin.body);
    let by_owner = app.delete(&alice, &format!("/admin/users/{frank_id}")).await;
    assert_eq!(by_owner.status, StatusCode::NO_CONTENT, "{}", by_owner.body);

    // Owners delete owners; the last active owner and admin stay (unit-tested in
    // `services::admin`, since a caller who may delete them always counts as one).
    let by_owner = app.delete(&alice, &format!("/admin/users/{grace_id}")).await;
    assert_eq!(by_owner.status, StatusCode::NO_CONTENT, "{}", by_owner.body);
    app.stop().await;
  }
}

mod effects {
  use super::*;

  #[tokio::test]
  async fn a_deleted_account_becomes_a_tombstone_without_key_material_or_memberships() {
    let app = app!();
    let (bob, carol, dave) = (admin(), member("carol"), member("dave"));
    let carol_id = user_id(&app, &carol).await;
    let channel = channel_with(&app, &[&carol, &dave]).await;
    let carol_device = register_device(&app, &carol).await;
    enroll_identity(&app, &carol).await;
    let dave_device = register_device(&app, &dave).await;
    let message = send(&app, &carol, &channel, &carol_device, 1).await;
    let mut dave_socket = connect(&app, &dave, &dave_device).await;

    let deleted = app.delete(&bob, &format!("/admin/users/{carol_id}")).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.body);

    let (seen, _) = events(&mut dave_socket).await;
    for expected in [
      format!("membership_remove {carol_id}"),
      format!("device_remove {carol_id} {carol_device}"),
      format!("profile_update {carol_id}"),
    ] {
      assert!(seen.contains(&expected), "{expected} missing: {seen:?}");
    }
    // The channel and her message stay; she is no longer a member.
    let members = app.get(&dave, &format!("/conversations/{channel}/members")).await;
    let members = members.body["members"].as_array().unwrap().clone();
    assert!(members.iter().all(|m| m["user"]["id"] != carol_id.as_str()));
    let history = app
      .get(&dave, &format!("/conversations/{channel}/messages"))
      .await;
    assert!(
      history.body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["id"] == message.as_str()),
      "{}",
      history.body
    );
    // The tombstone: deleted, deactivated, no username.
    let profile = app.get(&dave, &format!("/users/{carol_id}")).await;
    assert_eq!(profile.status, StatusCode::OK, "{}", profile.body);
    assert_eq!(profile.body["is_deleted"], true);
    assert_eq!(profile.body["is_deactivated"], true);
    assert!(profile.body.get("username").is_none(), "{}", profile.body);
    assert_eq!(profile.body["display_name"], "Deleted user");
    // Key material and devices are gone.
    assert_eq!(
      app
        .get(&dave, &format!("/users/{carol_id}/identity-key"))
        .await
        .status,
      StatusCode::NOT_FOUND
    );
    let devices = app.get(&bob, &format!("/admin/users/{carol_id}/devices")).await;
    assert!(
      devices.body["devices"]
        .as_array()
        .unwrap()
        .iter()
        .all(|d| d["is_revoked"] == true)
    );
    let key_bags: i64 =
      teamcraft_db::testing::sqlx::query_scalar("SELECT count(*) FROM key_bags WHERE user_id = $1")
        .bind(carol_id.parse::<i64>().unwrap())
        .fetch_one(app.sql_pool())
        .await
        .unwrap();
    assert_eq!(key_bags, 0);
    // Reactivating a deleted account is not possible.
    assert_eq!(
      app
        .post(&bob, &format!("/admin/users/{carol_id}/reactivate"), json!({}))
        .await
        .status,
      StatusCode::NOT_FOUND
    );
    let listed = app.get(&bob, "/admin/users?status=all").await;
    let entry = listed.body["users"]
      .as_array()
      .unwrap()
      .iter()
      .find(|u| u["id"] == carol_id.as_str())
      .cloned()
      .unwrap();
    assert_eq!(entry["is_deleted"], true);
    let audit = app.get(&bob, "/admin/audit").await;
    assert!(audit.body.to_string().contains("user_deleted"), "{}", audit.body);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_retry_after_a_lost_answer_succeeds() {
    let app = app!();
    let (bob, carol) = (admin(), member("carol"));
    let carol_id = user_id(&app, &carol).await;
    channel_with(&app, &[&carol]).await;
    register_device(&app, &carol).await;
    for _ in 0..2 {
      let deleted = app.delete(&bob, &format!("/admin/users/{carol_id}")).await;
      assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.body);
    }
    let audit = app.get(&bob, "/admin/audit").await;
    assert_eq!(
      audit.body.to_string().matches("user_deleted").count(),
      1,
      "recorded once: {}",
      audit.body
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn only_a_sign_in_after_the_deletion_creates_a_new_account() {
    let app = app!();
    let (bob, carol) = (admin(), member("carol"));
    let signed_in = unix_now() - 60;
    let old_token = app.token_issued_at(&carol, signed_in);
    let (status, body) = get_with(&app, &old_token, "/me").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let old_id = body["id"].as_str().unwrap().to_owned();
    let device = register_device(&app, &carol).await;
    let deleted = app.delete(&bob, &format!("/admin/users/{old_id}")).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.body);

    // A token from the old sign-in, also one refreshed since (same `auth_time`), opens nothing.
    let (status, body) = get_with(&app, &old_token, "/me").await;
    assert_eq!(
      (status, body["error_code"].as_str()),
      (StatusCode::UNAUTHORIZED, Some("account_deleted"))
    );
    let refreshed = app.token_with(&carol, unix_now() + 5, &json!({"auth_time": signed_in}));
    let (status, body) = get_with(&app, &refreshed, "/me").await;
    assert_eq!(
      (status, body["error_code"].as_str()),
      (StatusCode::UNAUTHORIZED, Some("account_deleted"))
    );
    // Same on the gateway.
    let refusal = identify_with(&app, &old_token, &device).await;
    assert!(refusal.contains("account_deleted"), "{refusal}");

    // A fresh sign-in creates a new account.
    let fresh = app.token_issued_at(&carol, unix_now() + 5);
    let (status, body) = get_with(&app, &fresh, "/me").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_ne!(body["id"].as_str().unwrap(), old_id, "the identity was released");
    assert_eq!(body["username"], "carol");
    let old = app.get(&bob, &format!("/users/{old_id}")).await;
    assert_eq!(old.body["is_deleted"], true);
    app.stop().await;
  }
}

mod direct_messages {
  use super::*;

  #[tokio::test]
  async fn one_to_one_dms_are_deleted_and_the_other_side_is_told() {
    let app = app!();
    let (bob, carol, dave) = (admin(), member("carol"), member("dave"));
    let carol_id = user_id(&app, &carol).await;
    let dave_id = user_id(&app, &dave).await;
    let carol_device = register_device(&app, &carol).await;
    let dave_device = register_device(&app, &dave).await;
    let dm = app.post(&dave, "/dms", json!({"user_id": carol_id})).await;
    assert!(dm.status.is_success(), "{}", dm.body);
    let dm = dm.id();
    send(&app, &carol, &dm, &carol_device, 1).await;
    let mut dave_socket = connect(&app, &dave, &dave_device).await;

    let deleted = app.delete(&bob, &format!("/admin/users/{carol_id}")).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.body);

    let (seen, _) = events(&mut dave_socket).await;
    assert!(seen.contains(&format!("conversation_delete {dm}")), "{seen:?}");
    assert!(
      seen.contains(&format!("membership_remove {dave_id}")),
      "older clients drop it as a removal: {seen:?}"
    );
    assert_eq!(
      app.get(&dave, &format!("/conversations/{dm}")).await.status,
      StatusCode::NOT_FOUND
    );
    // A device that was offline learns it on its next sync.
    let listing = app.get(&dave, "/conversations").await;
    assert_eq!(
      listing.body["deleted_conversation_ids"],
      json!([dm]),
      "{}",
      listing.body
    );
    let messages: i64 =
      teamcraft_db::testing::sqlx::query_scalar("SELECT count(*) FROM messages WHERE conversation_id = $1")
        .bind(dm.parse::<i64>().unwrap())
        .fetch_one(app.sql_pool())
        .await
        .unwrap();
    assert_eq!(messages, 0);
    app.stop().await;
  }

  /// Issue #1066: a removal from all chats before the deletion takes the membership, yet the DM is
  /// still the one with them and goes.
  #[tokio::test]
  async fn a_dm_goes_also_after_a_removal_from_all_chats() {
    let app = app!();
    let (bob, carol, dave) = (admin(), member("carol"), member("dave"));
    let carol_id = user_id(&app, &carol).await;
    let dave_id = user_id(&app, &dave).await;
    let dave_device = register_device(&app, &dave).await;
    let dm = app.post(&dave, "/dms", json!({"user_id": carol_id})).await;
    assert!(dm.status.is_success(), "{}", dm.body);
    let dm = dm.id();
    for step in ["deactivate", "remove-from-chats"] {
      let done = app
        .post(&bob, &format!("/admin/users/{carol_id}/{step}"), json!({}))
        .await;
      assert!(done.status.is_success(), "{step}: {}", done.body);
    }
    let mut dave_socket = connect(&app, &dave, &dave_device).await;

    let deleted = app.delete(&bob, &format!("/admin/users/{carol_id}")).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.body);

    let (seen, _) = events(&mut dave_socket).await;
    assert!(seen.contains(&format!("conversation_delete {dm}")), "{seen:?}");
    assert!(seen.contains(&format!("membership_remove {dave_id}")), "{seen:?}");
    assert_eq!(
      app.get(&dave, &format!("/conversations/{dm}")).await.status,
      StatusCode::NOT_FOUND
    );
    let listing = app.get(&dave, "/conversations").await;
    assert_eq!(
      listing.body["deleted_conversation_ids"],
      json!([dm]),
      "{}",
      listing.body
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn group_dms_stay_unless_only_one_person_would_remain() {
    let app = app!();
    let (bob, carol, dave, erin) = (admin(), member("carol"), member("dave"), member("erin"));
    let carol_id = user_id(&app, &carol).await;
    let dave_id = user_id(&app, &dave).await;
    let erin_id = user_id(&app, &erin).await;
    let frank = member("frank");
    let frank_id = user_id(&app, &frank).await;
    let small = app
      .post(&carol, "/group-dms", json!({"user_ids": [dave_id, erin_id]}))
      .await;
    assert!(small.status.is_success(), "{}", small.body);
    let small = small.id();
    let large = app
      .post(
        &carol,
        "/group-dms",
        json!({"user_ids": [dave_id, erin_id, frank_id]}),
      )
      .await
      .id();
    // Erin leaves the small one first, so only Dave would remain with Carol gone.
    teamcraft_db::testing::sqlx::query("DELETE FROM memberships WHERE conversation_id = $1 AND user_id = $2")
      .bind(small.parse::<i64>().unwrap())
      .bind(erin_id.parse::<i64>().unwrap())
      .execute(app.sql_pool())
      .await
      .unwrap();

    let deleted = app.delete(&bob, &format!("/admin/users/{carol_id}")).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.body);

    assert_eq!(
      app.get(&dave, &format!("/conversations/{small}")).await.status,
      StatusCode::NOT_FOUND
    );
    let kept = app.get(&dave, &format!("/conversations/{large}/members")).await;
    assert_eq!(kept.status, StatusCode::OK, "{}", kept.body);
    assert_eq!(kept.body["members"].as_array().unwrap().len(), 3);
    app.stop().await;
  }
}

fn unix_now() -> i64 {
  i64::try_from(now_secs()).unwrap()
}

/// A GET with this exact token (status and JSON body).
async fn get_with(app: &TestApp, token: &str, path: &str) -> (StatusCode, serde_json::Value) {
  let response = app
    .http
    .get(format!("{}/api/v1{path}", app.base_url))
    .bearer_auth(token)
    .send()
    .await
    .unwrap();
  let status = response.status();
  (status, response.json().await.unwrap_or(serde_json::Value::Null))
}

/// What the gateway answers an identify with this token: the text frames until it goes quiet or
/// closes.
async fn identify_with(app: &TestApp, token: &str, device: &str) -> String {
  let (mut socket, _) = tokio_tungstenite::connect_async(&app.ws_url).await.unwrap();
  socket.next().await;
  let identify = GatewayClientMessage::Identify {
    token: token.to_owned(),
    device_id: serde_json::from_value(json!(device)).unwrap(),
    activity: DeviceActivity::Active,
  };
  socket
    .send(Message::Text(serde_json::to_string(&identify).unwrap().into()))
    .await
    .unwrap();
  let mut seen = String::new();
  while let Ok(Some(Ok(frame))) = tokio::time::timeout(Duration::from_millis(700), socket.next()).await {
    match frame {
      Message::Text(text) => seen.push_str(text.as_str()),
      Message::Close(maybe_frame) => {
        if let Some(frame) = maybe_frame {
          seen.push_str(frame.reason.as_str());
        }
        break;
      },
      _ => {},
    }
  }
  seen
}

type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(app: &TestApp, user: &TestUser, device: &str) -> Socket {
  let (mut socket, _) = tokio_tungstenite::connect_async(&app.ws_url).await.unwrap();
  socket.next().await;
  let identify = GatewayClientMessage::Identify {
    token: app.token(user),
    device_id: serde_json::from_value(json!(device)).unwrap(),
    activity: DeviceActivity::Active,
  };
  socket
    .send(Message::Text(serde_json::to_string(&identify).unwrap().into()))
    .await
    .unwrap();
  socket.next().await;
  socket
}

/// Events seen within a short while, as compact text, until the connection closes.
async fn events(socket: &mut Socket) -> (Vec<String>, bool) {
  let mut seen = Vec::new();
  loop {
    match tokio::time::timeout(Duration::from_millis(700), socket.next()).await {
      Err(_) => return (seen, false),
      Ok(None | Some(Err(_)) | Some(Ok(Message::Close(_)))) => return (seen, true),
      Ok(Some(Ok(Message::Text(text)))) => {
        if let Ok(GatewayServerMessage::Event { event, .. }) = serde_json::from_str(text.as_str()) {
          match event {
            GatewayEvent::MembershipRemove { user_id, .. } => {
              seen.push(format!("membership_remove {user_id}"))
            },
            GatewayEvent::ConversationDelete { conversation_id } => {
              seen.push(format!("conversation_delete {conversation_id}"))
            },
            GatewayEvent::DeviceRemove { user_id, device_id } => {
              seen.push(format!("device_remove {user_id} {device_id}"))
            },
            GatewayEvent::ProfileUpdate { user_id } => seen.push(format!("profile_update {user_id}")),
            _ => {},
          }
        }
      },
      Ok(Some(Ok(_))) => {},
    }
  }
}
