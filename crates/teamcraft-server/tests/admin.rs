//! Administration against a real server and Postgres.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::*;
use futures_util::{SinkExt, StreamExt};
use reqwest::StatusCode;
use serde_json::{Value, json};
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

fn ids(reply: &teamcraft_server::testing::Reply, key: &str) -> Vec<String> {
  reply.body[key]
    .as_array()
    .unwrap()
    .iter()
    .map(|item| item["id"].as_str().unwrap().to_owned())
    .collect()
}

mod overview {
  use super::*;

  #[tokio::test]
  async fn only_admins_use_the_admin_api() {
    let app = app!();
    let carol = member("carol");
    for path in [
      "/admin/spaces",
      "/admin/conversations",
      "/admin/users",
      "/admin/audit",
    ] {
      let reply = app.get(&carol, path).await;
      assert_eq!(
        (reply.status, reply.error_code()),
        (StatusCode::FORBIDDEN, "forbidden"),
        "{path}"
      );
    }
    let guest = guest("ext");
    assert_eq!(
      app.get(&guest, "/admin/users").await.status,
      StatusCode::FORBIDDEN
    );
    assert_eq!(app.get(&admin(), "/admin/spaces").await.status, StatusCode::OK);
    app.stop().await;
  }

  #[tokio::test]
  async fn admins_see_all_spaces_and_channels_with_their_members_but_no_messages() {
    let app = app!();
    let (alice, bob, carol) = (owner(), admin(), member("carol"));
    let device = register_device(&app, &alice).await;
    let carol_id = user_id(&app, &carol).await;
    let space = create_space(&app, &alice, "Secret", "invite_only").await;
    add_space_member(&app, &alice, &space, &carol_id, "member").await;
    let private = create_channel(&app, &alice, &space, "leads", "private").await;
    let public = create_channel(&app, &alice, &space, "general", "public").await;
    add_channel_member(&app, &alice, &private, &carol_id).await;
    send(&app, &alice, &private, &device, 1).await;
    app
      .post(&carol, "/dms", json!({"user_id": user_id(&app, &alice).await}))
      .await;

    let spaces = app.get(&bob, "/admin/spaces").await;
    assert_eq!(spaces.body["spaces"][0]["member_count"], 2);
    assert_eq!(spaces.body["spaces"][0]["channel_count"], 2);

    let all = app.get(&bob, "/admin/conversations").await;
    assert_eq!(
      ids(&all, "conversations"),
      vec![private.clone(), public.clone()],
      "channels only, no DMs"
    );
    let first = &all.body["conversations"][0];
    assert_eq!(
      (first["member_count"].clone(), first["message_count"].clone()),
      (json!(2), json!(1))
    );
    assert!(first.get("last_activity_at").is_some());

    let members = app
      .get(&bob, &format!("/admin/conversations/{private}/members"))
      .await;
    assert_eq!(members.body["members"].as_array().unwrap().len(), 2);
    let space_members = app.get(&bob, &format!("/admin/spaces/{space}/members")).await;
    assert_eq!(space_members.status, StatusCode::OK);
    // Metadata yes, content no.
    assert_eq!(
      app
        .get(&bob, &format!("/conversations/{private}/messages"))
        .await
        .status,
      StatusCode::FORBIDDEN
    );
    let carol_conversations = app.get(&carol, "/conversations").await;
    let dm = carol_conversations.body["conversations"]
      .as_array()
      .unwrap()
      .iter()
      .find(|c| c["kind"] == "dm")
      .map(|c| c["id"].as_str().unwrap().to_owned())
      .unwrap();
    assert_eq!(
      app
        .get(&bob, &format!("/admin/conversations/{dm}/members"))
        .await
        .status,
      StatusCode::NOT_FOUND
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn conversations_can_be_filtered() {
    let app = app!();
    let (alice, bob) = (owner(), admin());
    let (ext, carol) = (guest("ext"), member("carol"));
    let (ext_id, carol_id) = (user_id(&app, &ext).await, user_id(&app, &carol).await);
    let a = create_space(&app, &alice, "A", "invite_only").await;
    let b = create_space(&app, &alice, "B", "invite_only").await;
    let with_guest = create_channel(&app, &alice, &a, "client", "public").await;
    let private = create_channel(&app, &alice, &a, "leads", "private").await;
    let other = create_channel(&app, &alice, &b, "other", "public").await;
    add_space_member(&app, &alice, &a, &ext_id, "guest").await;
    add_space_member(&app, &alice, &a, &carol_id, "member").await;
    add_channel_member(&app, &alice, &with_guest, &ext_id).await;
    add_channel_member(&app, &alice, &with_guest, &carol_id).await;

    let list = |query: String| {
      let (app, bob) = (&app, &bob);
      async move {
        ids(
          &app.get(bob, &format!("/admin/conversations?{query}")).await,
          "conversations",
        )
      }
    };
    assert_eq!(list(format!("space_id={b}")).await, vec![other.clone()]);
    assert_eq!(list("visibility=private".into()).await, vec![private.clone()]);
    assert_eq!(list("has_guests=true".into()).await, vec![with_guest.clone()]);
    assert_eq!(
      list("has_guests=false".into()).await,
      vec![private.clone(), other.clone()]
    );
    assert_eq!(list("min_members=3".into()).await, vec![with_guest.clone()]);
    assert_eq!(
      list("max_members=1".into()).await,
      vec![private.clone(), other.clone()]
    );
    assert!(
      list("inactive_days=7".into()).await.is_empty(),
      "everything is fresh"
    );
    let paged = app.get(&bob, "/admin/conversations?limit=2").await;
    assert_eq!(paged.body["has_more"], true);
    let rest = list(format!(
      "after={}",
      paged.body["conversations"][1]["id"].as_str().unwrap()
    ))
    .await;
    assert_eq!(rest.len(), 1);
    app.stop().await;
  }

  #[tokio::test]
  async fn users_are_listed_searched_and_filtered_by_status() {
    let app = app!();
    let (alice, bob, carol) = (owner(), admin(), member("carol"));
    let carol_id = user_id(&app, &carol).await;
    user_id(&app, &alice).await;
    register_device(&app, &carol).await;
    app
      .post(&bob, &format!("/admin/users/{carol_id}/deactivate"), json!({}))
      .await;

    let carol_row = app.get(&bob, "/admin/users?query=caro").await;
    assert_eq!(carol_row.body["users"].as_array().unwrap().len(), 1);
    let row = &carol_row.body["users"][0];
    assert_eq!(
      (row["is_deactivated"].clone(), row["deactivation_reason"].clone()),
      (json!(true), json!("admin"))
    );
    assert_eq!(
      app.get(&bob, "/admin/users?status=active").await.body["users"]
        .as_array()
        .unwrap()
        .len(),
      2
    );
    assert_eq!(
      app.get(&bob, "/admin/users?status=deactivated").await.body["users"]
        .as_array()
        .unwrap()
        .len(),
      1
    );

    let devices = app.get(&bob, &format!("/admin/users/{carol_id}/devices")).await;
    assert_eq!(devices.body["devices"][0]["is_revoked"], true);
    assert_eq!(
      app.get(&bob, "/admin/users/424242/devices").await.status,
      StatusCode::NOT_FOUND
    );
    app.stop().await;
  }
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
            GatewayEvent::DeviceRemove { user_id, device_id } => {
              seen.push(format!("device_remove {user_id} {device_id}"))
            },
            GatewayEvent::SpaceMemberRemove { user_id, .. } => {
              seen.push(format!("space_member_remove {user_id}"))
            },
            _ => {},
          }
        }
      },
      Ok(Some(Ok(_))) => {},
    }
  }
}

mod deactivation {
  use super::*;

  #[tokio::test]
  async fn a_deactivated_user_is_cut_off_everywhere_and_the_others_are_told() {
    let app = app!();
    let (alice, bob, carol, dave) = (owner(), admin(), member("carol"), member("dave"));
    let (carol_id, dave_id) = (user_id(&app, &carol).await, user_id(&app, &dave).await);
    let channel = channel_with(&app, &[&carol, &dave]).await;
    let (carol_device, dave_device) = (
      register_device(&app, &carol).await,
      register_device(&app, &dave).await,
    );
    let mut carol_socket = connect(&app, &carol, &carol_device).await;
    let mut dave_socket = connect(&app, &dave, &dave_device).await;
    let _ = (&alice, &dave_id);

    let done = app
      .post(
        &bob,
        &format!("/admin/users/{carol_id}/deactivate"),
        json!({"note": "left the company"}),
      )
      .await;
    assert_eq!(done.status, StatusCode::NO_CONTENT, "{}", done.body);

    // Her live connection ends, she can't come back and she is out of the channel.
    let (_, closed) = events(&mut carol_socket).await;
    assert!(closed, "the deactivated user's gateway connection stayed open");
    let login = app.get(&carol, "/me").await;
    assert_eq!(
      (login.status, login.error_code()),
      (StatusCode::FORBIDDEN, "forbidden")
    );
    let members = app.get(&dave, &format!("/conversations/{channel}/members")).await;
    assert_eq!(
      members.body["members"].as_array().unwrap().len(),
      2,
      "alice (creator) and dave remain"
    );

    // Dave's client learns what to commit: she left the group and her device is gone.
    let (seen, _) = events(&mut dave_socket).await;
    assert!(
      seen.contains(&format!("membership_remove {carol_id}")),
      "{seen:?}"
    );
    assert!(
      seen.contains(&format!("device_remove {carol_id} {carol_device}")),
      "{seen:?}"
    );
    assert!(
      seen.iter().any(|e| e.starts_with("space_member_remove")),
      "{seen:?}"
    );

    // Idempotent; and she can't get MLS key packages claimed any more.
    assert_eq!(
      app
        .post(&bob, &format!("/admin/users/{carol_id}/deactivate"), json!({}))
        .await
        .status,
      StatusCode::NO_CONTENT
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn admins_cannot_deactivate_themselves_and_reactivation_restores_login_only() {
    let app = app!();
    let (bob, carol) = (admin(), member("carol"));
    let bob_id = user_id(&app, &bob).await;
    let carol_id = user_id(&app, &carol).await;
    let channel = channel_with(&app, &[&carol]).await;

    let own = app
      .post(&bob, &format!("/admin/users/{bob_id}/deactivate"), json!({}))
      .await;
    assert_eq!(own.status, StatusCode::BAD_REQUEST);
    app
      .post(&bob, &format!("/admin/users/{carol_id}/deactivate"), json!({}))
      .await;
    assert_eq!(
      app
        .post(&bob, &format!("/admin/users/{carol_id}/reactivate"), json!({}))
        .await
        .status,
      StatusCode::NO_CONTENT
    );
    assert_eq!(app.get(&carol, "/me").await.status, StatusCode::OK);
    assert_eq!(
      app.get(&carol, &format!("/conversations/{channel}")).await.status,
      StatusCode::NOT_FOUND,
      "memberships stay removed"
    );
    assert_eq!(
      app
        .post(&bob, &format!("/admin/users/{carol_id}/reactivate"), json!({}))
        .await
        .status,
      StatusCode::NOT_FOUND
    );
    assert_eq!(
      app
        .post(
          &member("dave"),
          &format!("/admin/users/{carol_id}/deactivate"),
          json!({})
        )
        .await
        .status,
      StatusCode::FORBIDDEN
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn removing_a_device_ends_its_session_and_is_announced() {
    let app = app!();
    let (bob, carol, dave) = (admin(), member("carol"), member("dave"));
    let carol_id = user_id(&app, &carol).await;
    channel_with(&app, &[&carol, &dave]).await;
    let (lost, kept) = (
      register_device(&app, &carol).await,
      register_device(&app, &carol).await,
    );
    let dave_device = register_device(&app, &dave).await;
    let mut lost_socket = connect(&app, &carol, &lost).await;
    let mut kept_socket = connect(&app, &carol, &kept).await;
    let mut dave_socket = connect(&app, &dave, &dave_device).await;

    let reply = app
      .delete(&bob, &format!("/admin/users/{carol_id}/devices/{lost}"))
      .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
    assert!(
      events(&mut lost_socket).await.1,
      "the removed device's connection stays open"
    );
    let (kept_events, kept_closed) = events(&mut kept_socket).await;
    assert!(
      !kept_closed && kept_events.iter().any(|e| e.contains(&lost)),
      "{kept_events:?}"
    );
    assert!(
      events(&mut dave_socket)
        .await
        .0
        .contains(&format!("device_remove {carol_id} {lost}"))
    );
    // The account itself keeps working with the other device.
    assert_eq!(app.get(&carol, "/me").await.status, StatusCode::OK);
    assert_eq!(
      app
        .delete(&bob, &format!("/admin/users/{carol_id}/devices/{lost}"))
        .await
        .error_code(),
      "device_not_found"
    );
    app.stop().await;
  }
}

mod audit {
  use super::*;

  fn actions(reply: &teamcraft_server::testing::Reply) -> Vec<String> {
    reply.body["entries"]
      .as_array()
      .unwrap()
      .iter()
      .map(|e| e["action"].as_str().unwrap().to_owned())
      .collect()
  }

  #[tokio::test]
  async fn admin_actions_device_changes_and_policy_changes_are_logged() {
    let app = app!();
    let (alice, bob, carol) = (owner(), admin(), member("carol"));
    let (bob_id, carol_id) = (user_id(&app, &bob).await, user_id(&app, &carol).await);
    let space = create_space(&app, &alice, "Team", "open").await;
    let device = register_device(&app, &carol).await;
    app
      .patch(&alice, &format!("/spaces/{space}"), json!({"is_archived": false}))
      .await;
    app.delete(&carol, &format!("/devices/{device}")).await;
    app
      .post(
        &bob,
        &format!("/admin/users/{carol_id}/deactivate"),
        json!({"note": "test"}),
      )
      .await;

    let log = app.get(&bob, "/admin/audit").await;
    assert_eq!(
      actions(&log),
      vec![
        "user_deactivated",
        "device_revoked",
        "space_updated",
        "space_created"
      ]
    );
    let deactivated = &log.body["entries"][0];
    assert_eq!(deactivated["actor_user_id"], bob_id);
    assert_eq!(deactivated["target_kind"], "user");
    assert_eq!(deactivated["target_id"], carol_id);
    let detail: Value = serde_json::from_str(deactivated["detail"].as_str().unwrap()).unwrap();
    assert_eq!(
      (detail["reason"].clone(), detail["note"].clone()),
      (json!("admin"), json!("test"))
    );
    let revoked: Value = serde_json::from_str(log.body["entries"][1]["detail"].as_str().unwrap()).unwrap();
    assert_eq!(
      (revoked["by"].clone(), revoked["device_id"].clone()),
      (json!("self"), json!(device))
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn the_log_can_be_filtered_and_paged() {
    let app = app!();
    let (alice, bob) = (owner(), admin());
    for name in ["One", "Two", "Three"] {
      create_space(&app, &alice, name, "open").await;
    }
    let bob_id = user_id(&app, &bob).await;
    let alice_id = user_id(&app, &alice).await;

    let by_action = app.get(&bob, "/admin/audit?action=space_created").await;
    assert_eq!(by_action.body["entries"].as_array().unwrap().len(), 3);
    let by_actor = app.get(&bob, &format!("/admin/audit?actor={bob_id}")).await;
    assert!(by_actor.body["entries"].as_array().unwrap().is_empty());
    let by_alice = app
      .get(&bob, &format!("/admin/audit?actor={alice_id}&target_kind=space"))
      .await;
    assert_eq!(by_alice.body["entries"].as_array().unwrap().len(), 3);

    let page = app.get(&bob, "/admin/audit?limit=2").await;
    assert_eq!(page.body["has_more"], true);
    let before = page.body["entries"][1]["id"].as_str().unwrap();
    let rest = app
      .get(&bob, &format!("/admin/audit?limit=2&before={before}"))
      .await;
    assert_eq!(rest.body["entries"].as_array().unwrap().len(), 1);
    assert_eq!(rest.body["has_more"], false);

    let future = app.get(&bob, "/admin/audit?since=4102444800000").await;
    assert!(future.body["entries"].as_array().unwrap().is_empty());
    let past = app.get(&bob, "/admin/audit?until=1000").await;
    assert!(past.body["entries"].as_array().unwrap().is_empty());
    assert_eq!(
      app.get(&bob, "/admin/audit?target_id=abc").await.status,
      StatusCode::BAD_REQUEST
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn users_see_what_happened_to_their_own_account_and_only_that() {
    let app = app!();
    let (bob, carol, dave) = (admin(), member("carol"), member("dave"));
    let carol_id = user_id(&app, &carol).await;
    user_id(&app, &dave).await;
    let (carol_device, dave_device) = (
      register_device(&app, &carol).await,
      register_device(&app, &dave).await,
    );
    app
      .delete(&bob, &format!("/admin/users/{carol_id}/devices/{carol_device}"))
      .await;
    app.delete(&dave, &format!("/devices/{dave_device}")).await;

    let mine = app.get(&carol, "/me/security-log").await;
    assert_eq!(actions(&mine), vec!["device_revoked"]);
    let detail: Value = serde_json::from_str(mine.body["entries"][0]["detail"].as_str().unwrap()).unwrap();
    assert_eq!(detail["by"], "admin");
    // Not the full log, even when asking for it.
    assert_eq!(
      app.get(&carol, "/admin/audit").await.status,
      StatusCode::FORBIDDEN
    );
    let forged = app
      .get(
        &carol,
        &format!("/me/security-log?actor={}", user_id(&app, &dave).await),
      )
      .await;
    assert_eq!(
      actions(&forged),
      vec!["device_revoked"],
      "the filter can't widen the view"
    );
    app.stop().await;
  }
}
