//! The MLS delivery service against a real server and Postgres.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::time::Duration;

use common::*;
use futures_util::{SinkExt, StreamExt};
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_proto::{DeviceActivity, GatewayClientMessage, GatewayEvent, GatewayServerMessage};
use teamcraft_server::testing::{Reply, TestApp, TestUser};
use tokio_tungstenite::tungstenite::Message;

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

/// carol and dave in a private channel, each with a device.
struct Group {
  channel: String,
  carol: TestUser,
  dave: TestUser,
  carol_device: String,
  dave_device: String,
}

async fn group(app: &TestApp) -> Group {
  let (carol, dave) = (member("carol"), member("dave"));
  let channel = channel_with(app, &[&carol, &dave]).await;
  let (carol_device, dave_device) = (
    register_device(app, &carol).await,
    register_device(app, &dave).await,
  );
  Group {
    channel,
    carol,
    dave,
    carol_device,
    dave_device,
  }
}

fn b64(text: &str) -> String {
  use base64::Engine;
  base64::engine::general_purpose::STANDARD.encode(text)
}

async fn commit(
  app: &TestApp,
  user: &TestUser,
  channel: &str,
  device: &str,
  epoch: u64,
  welcomes: Value,
) -> Reply {
  app.post(
    user,
    &format!("/conversations/{channel}/mls/commits"),
    json!({"device_id": device, "epoch": epoch, "commit": b64(&format!("commit-{epoch}")), "welcomes": welcomes}),
  )
  .await
}

fn kinds(reply: &Reply) -> Vec<String> {
  reply.body["messages"]
    .as_array()
    .unwrap()
    .iter()
    .map(|m| m["kind"].as_str().unwrap().to_owned())
    .collect()
}

mod group_info {
  use super::*;

  #[tokio::test]
  async fn the_group_information_of_the_current_epoch_is_served_to_members() {
    let app = app!();
    let g = group(&app).await;
    let path = format!("/conversations/{}/mls/group-info", g.channel);
    assert_eq!(
      app.get(&g.carol, &path).await.error_code(),
      "mls_group_info_not_found"
    );

    // A commit carries the information for the epoch it starts.
    let first = app
      .post(
        &g.carol,
        &format!("/conversations/{}/mls/commits", g.channel),
        json!({"device_id": g.carol_device, "epoch": 0, "commit": b64("commit-0"), "group_info": b64("info-1")}),
      )
      .await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
    let served = app.get(&g.dave, &path).await;
    assert_eq!(served.status, StatusCode::OK, "{}", served.body);
    assert_eq!(served.body, json!({"epoch": 1, "group_info": b64("info-1")}));

    // A commit without one makes the stored one stale: it is not served any more.
    commit(&app, &g.dave, &g.channel, &g.dave_device, 1, json!([])).await;
    assert_eq!(
      app.get(&g.dave, &path).await.error_code(),
      "mls_group_info_not_found"
    );
    // A member publishes it for the current epoch; another epoch is a conflict.
    let stale = app
      .put(
        &g.dave,
        &path,
        json!({"device_id": g.dave_device, "epoch": 1, "group_info": b64("info-old")}),
      )
      .await;
    assert_eq!(stale.error_code(), "epoch_conflict");
    let put = app
      .put(
        &g.dave,
        &path,
        json!({"device_id": g.dave_device, "epoch": 2, "group_info": b64("info-2")}),
      )
      .await;
    assert_eq!(put.status, StatusCode::NO_CONTENT, "{}", put.body);
    assert_eq!(app.get(&g.carol, &path).await.body["epoch"], 2);

    // Too large to keep: the commit goes through without it.
    let big = "x".repeat(2 * 1024 * 1024);
    let third = app
      .post(
        &g.carol,
        &format!("/conversations/{}/mls/commits", g.channel),
        json!({"device_id": g.carol_device, "epoch": 2, "commit": b64("commit-2"), "group_info": b64(&big)}),
      )
      .await;
    assert_eq!(third.status, StatusCode::CREATED, "{}", third.body);
    assert_eq!(
      app.get(&g.carol, &path).await.error_code(),
      "mls_group_info_not_found"
    );

    // Not for outsiders.
    let erin = member("erin");
    register_device(&app, &erin).await;
    assert_ne!(app.get(&erin, &path).await.status, StatusCode::OK);
    app.stop().await;
  }
}

mod commits {
  use super::*;

  #[tokio::test]
  async fn a_commit_advances_the_epoch_and_the_chain_continues() {
    let app = app!();
    let g = group(&app).await;
    let first = commit(&app, &g.carol, &g.channel, &g.carol_device, 0, json!([])).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
    assert_eq!(first.body["new_epoch"], 1);
    assert_eq!(first.body["commit"]["kind"], "commit");
    assert_eq!(first.body["commit"]["epoch"], 0);

    let second = commit(&app, &g.dave, &g.channel, &g.dave_device, 1, json!([])).await;
    assert_eq!(second.body["new_epoch"], 2);
    let backlog = app
      .get(&g.carol, &format!("/conversations/{}/mls", g.channel))
      .await;
    assert_eq!(backlog.body["current_epoch"], 2);
    assert_eq!(kinds(&backlog), vec!["commit", "commit"]);
    app.stop().await;
  }

  #[tokio::test]
  async fn the_second_commit_for_an_epoch_is_rejected_with_the_current_epoch() {
    let app = app!();
    let g = group(&app).await;
    commit(&app, &g.carol, &g.channel, &g.carol_device, 0, json!([])).await;

    let late = commit(&app, &g.dave, &g.channel, &g.dave_device, 0, json!([])).await;
    assert_eq!(late.status, StatusCode::CONFLICT);
    assert_eq!(late.error_code(), "epoch_conflict");
    assert!(
      late.body["message"].as_str().unwrap().contains("epoch 1"),
      "{}",
      late.body
    );
    // The loser's commit was not stored.
    let backlog = app
      .get(&g.dave, &format!("/conversations/{}/mls", g.channel))
      .await;
    assert_eq!(kinds(&backlog), vec!["commit"]);
    // A commit for an epoch that does not exist yet is rejected too.
    assert_eq!(
      commit(&app, &g.dave, &g.channel, &g.dave_device, 5, json!([]))
        .await
        .error_code(),
      "epoch_conflict"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn of_many_concurrent_commits_for_one_epoch_exactly_one_wins() {
    let app = app!();
    let alice = owner();
    let mut users = Vec::new();
    for n in 0..12 {
      users.push(member(&format!("user{n}")));
    }
    let channel = channel_with(&app, &users.iter().collect::<Vec<_>>()).await;
    let mut devices = Vec::new();
    for user in &users {
      devices.push(register_device(&app, user).await);
    }
    let _ = alice;

    let attempts = users.iter().zip(&devices).map(|(user, device)| {
      let (app, channel) = (&app, channel.clone());
      async move { commit(app, user, &channel, device, 0, json!([])).await }
    });
    let replies = futures_util::future::join_all(attempts).await;

    let winners = replies.iter().filter(|r| r.status == StatusCode::CREATED).count();
    let losers = replies
      .iter()
      .filter(|r| r.status == StatusCode::CONFLICT && r.error_code() == "epoch_conflict")
      .count();
    assert_eq!((winners, losers), (1, 11), "unexpected outcomes");
    let backlog = app.get(&users[0], &format!("/conversations/{channel}/mls")).await;
    assert_eq!(backlog.body["current_epoch"], 1);
    assert_eq!(kinds(&backlog), vec!["commit"]);
    app.stop().await;
  }

  #[tokio::test]
  async fn only_members_with_their_own_device_commit() {
    let app = app!();
    let g = group(&app).await;
    let erin = member("erin");
    let erin_device = register_device(&app, &erin).await;
    assert_eq!(
      commit(&app, &erin, &g.channel, &erin_device, 0, json!([]))
        .await
        .status,
      StatusCode::NOT_FOUND
    );
    // dave's device can't be used by carol.
    let stolen = commit(&app, &g.carol, &g.channel, &g.dave_device, 0, json!([])).await;
    assert_eq!(stolen.error_code(), "device_not_found");
    assert_eq!(
      app
        .get(&g.carol, &format!("/conversations/{}/mls", g.channel))
        .await
        .body["current_epoch"],
      0
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn payloads_are_limited_and_welcomes_must_address_member_devices() {
    let Some(app) = TestApp::start_with(|s| s.limits.max_mls_payload_bytes = 16).await else {
      return;
    };
    let g = group(&app).await;
    let outsider = member("erin");
    let outsider_device = register_device(&app, &outsider).await;

    let big = app
      .post(
        &g.carol,
        &format!("/conversations/{}/mls/commits", g.channel),
        json!({"device_id": g.carol_device, "epoch": 0, "commit": b64(&"x".repeat(40))}),
      )
      .await;
    assert_eq!(big.status, StatusCode::PAYLOAD_TOO_LARGE);
    let stranger = commit(
      &app,
      &g.carol,
      &g.channel,
      &g.carol_device,
      0,
      json!([{"device_id": outsider_device, "welcome": b64("w")}]),
    )
    .await;
    assert_eq!(stranger.status, StatusCode::BAD_REQUEST);
    let twice = commit(
      &app,
      &g.carol,
      &g.channel,
      &g.carol_device,
      0,
      json!([{"device_id": g.dave_device, "welcome": b64("w")}, {"device_id": g.dave_device, "welcome": b64("w")}]),
    )
    .await;
    assert_eq!(twice.status, StatusCode::BAD_REQUEST);
    let empty = app
      .post(
        &g.carol,
        &format!("/conversations/{}/mls/commits", g.channel),
        json!({"device_id": g.carol_device, "epoch": 0, "commit": ""}),
      )
      .await;
    assert_eq!(empty.status, StatusCode::BAD_REQUEST);
    // None of the failed attempts moved the epoch.
    assert_eq!(
      commit(&app, &g.carol, &g.channel, &g.carol_device, 0, json!([]))
        .await
        .status,
      StatusCode::CREATED
    );
    app.stop().await;
  }
}

mod welcomes {
  use super::*;

  #[tokio::test]
  async fn a_welcome_is_stored_with_the_commit_and_only_its_recipient_reads_it() {
    let app = app!();
    let g = group(&app).await;
    let reply = commit(
      &app,
      &g.carol,
      &g.channel,
      &g.carol_device,
      0,
      json!([{"device_id": g.dave_device, "welcome": b64("welcome-for-dave")}]),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);

    // dave sees commit and welcome (the welcome carries the new epoch) ...
    let dave_view = app
      .get(&g.dave, &format!("/conversations/{}/mls", g.channel))
      .await;
    assert_eq!(kinds(&dave_view), vec!["commit", "welcome"]);
    assert_eq!(dave_view.body["messages"][1]["epoch"], 1);
    assert_eq!(
      dave_view.body["messages"][1]["recipient_device_id"],
      g.dave_device
    );
    assert_eq!(dave_view.body["messages"][1]["payload"], b64("welcome-for-dave"));
    // ... carol sees only the commit.
    let carol_view = app
      .get(&g.carol, &format!("/conversations/{}/mls", g.channel))
      .await;
    assert_eq!(kinds(&carol_view), vec!["commit"]);

    // A new device finds its welcomes without knowing the conversations.
    let waiting = app
      .get(&g.dave, &format!("/devices/{}/welcomes", g.dave_device))
      .await;
    assert_eq!(waiting.body["messages"].as_array().unwrap().len(), 1);
    assert_eq!(waiting.body["messages"][0]["conversation_id"], g.channel);
    assert_eq!(
      app
        .get(&g.carol, &format!("/devices/{}/welcomes", g.dave_device))
        .await
        .status,
      StatusCode::NOT_FOUND
    );
    app.stop().await;
  }
}

mod messages {
  use super::*;

  async fn post(
    app: &TestApp,
    user: &TestUser,
    channel: &str,
    device: &str,
    epoch: u64,
    kind: &str,
  ) -> Reply {
    app
      .post(
        user,
        &format!("/conversations/{channel}/mls/messages"),
        json!({"device_id": device, "epoch": epoch, "kind": kind, "payload": b64("p")}),
      )
      .await
  }

  #[tokio::test]
  async fn application_messages_must_match_the_current_epoch() {
    let app = app!();
    let g = group(&app).await;
    assert_eq!(
      post(&app, &g.carol, &g.channel, &g.carol_device, 0, "application")
        .await
        .status,
      StatusCode::CREATED
    );
    commit(&app, &g.carol, &g.channel, &g.carol_device, 0, json!([])).await;

    let stale = post(&app, &g.dave, &g.channel, &g.dave_device, 0, "application").await;
    assert_eq!(
      (stale.status, stale.error_code()),
      (StatusCode::CONFLICT, "epoch_conflict")
    );
    assert_eq!(
      post(&app, &g.dave, &g.channel, &g.dave_device, 1, "application")
        .await
        .status,
      StatusCode::CREATED
    );
    let invalid_kind = post(&app, &g.dave, &g.channel, &g.dave_device, 1, "commit").await;
    assert_eq!(invalid_kind.status, StatusCode::BAD_REQUEST);

    let backlog = app
      .get(&g.dave, &format!("/conversations/{}/mls", g.channel))
      .await;
    assert_eq!(kinds(&backlog), vec!["application", "commit", "application"]);
    app.stop().await;
  }

  #[tokio::test]
  async fn admins_send_external_proposals_without_being_members() {
    let app = app!();
    let g = group(&app).await;
    let (bob, erin) = (admin(), member("erin"));
    let bob_device = register_device(&app, &bob).await;
    let erin_device = register_device(&app, &erin).await;

    let proposal = post(&app, &bob, &g.channel, &bob_device, 0, "proposal").await;
    assert_eq!(proposal.status, StatusCode::CREATED, "{}", proposal.body);
    // Admins are not members: no group messages and no backlog.
    assert_eq!(
      post(&app, &bob, &g.channel, &bob_device, 0, "application")
        .await
        .status,
      StatusCode::FORBIDDEN
    );
    assert_eq!(
      app
        .get(&bob, &format!("/conversations/{}/mls", g.channel))
        .await
        .status,
      StatusCode::FORBIDDEN
    );
    // A user who can't even see the channel gets nothing.
    assert_eq!(
      post(&app, &erin, &g.channel, &erin_device, 0, "proposal")
        .await
        .status,
      StatusCode::NOT_FOUND
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn the_backlog_is_paged_by_the_last_processed_entry_and_hides_the_time_before_joining() {
    let app = app!();
    let g = group(&app).await;
    let mut ids = Vec::new();
    for _ in 0..4 {
      let sent = post(&app, &g.carol, &g.channel, &g.carol_device, 0, "application").await;
      ids.push(sent.id());
    }
    let page = app
      .get(&g.dave, &format!("/conversations/{}/mls?limit=3", g.channel))
      .await;
    assert_eq!(page.body["messages"].as_array().unwrap().len(), 3);
    assert_eq!(page.body["has_more"], true);
    let rest = app
      .get(
        &g.dave,
        &format!("/conversations/{}/mls?after={}", g.channel, ids[2]),
      )
      .await;
    assert_eq!(rest.body["messages"][0]["id"], ids[3]);
    assert_eq!(rest.body["has_more"], false);

    // erin joins later and does not get what predates her membership.
    tokio::time::sleep(Duration::from_millis(20)).await;
    let erin = member("erin");
    let erin_id = user_id(&app, &erin).await;
    let space = app.get(&owner(), "/spaces").await.body["spaces"][0]["id"]
      .as_str()
      .unwrap()
      .to_owned();
    add_space_member(&app, &owner(), &space, &erin_id, "member").await;
    add_channel_member(&app, &owner(), &g.channel, &erin_id).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let fresh = post(&app, &g.carol, &g.channel, &g.carol_device, 0, "application")
      .await
      .id();
    let erin_view = app.get(&erin, &format!("/conversations/{}/mls", g.channel)).await;
    assert_eq!(erin_view.body["messages"].as_array().unwrap().len(), 1);
    assert_eq!(erin_view.body["messages"][0]["id"], fresh);
    app.stop().await;
  }
}

mod delivery_over_the_gateway {
  use super::*;

  type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

  async fn connect(app: &TestApp, user: &TestUser, device: &str) -> Socket {
    let (mut socket, _) = tokio_tungstenite::connect_async(&app.ws_url).await.unwrap();
    let _hello = socket.next().await;
    let identify = GatewayClientMessage::Identify {
      token: app.token(user),
      device_id: serde_json::from_value(json!(device)).unwrap(),
      activity: DeviceActivity::Active,
    };
    socket
      .send(Message::Text(serde_json::to_string(&identify).unwrap().into()))
      .await
      .unwrap();
    let ready = socket.next().await.unwrap().unwrap();
    assert!(ready.to_text().unwrap().contains("\"ready\""));
    socket
  }

  /// MLS events received within a short while, as (kind, recipient device).
  async fn mls_events(socket: &mut Socket) -> Vec<(String, Option<String>)> {
    let mut found = Vec::new();
    while let Ok(Some(Ok(Message::Text(text)))) =
      tokio::time::timeout(Duration::from_millis(500), socket.next()).await
    {
      if let Ok(GatewayServerMessage::Event {
        event: GatewayEvent::MlsMessage { message },
        ..
      }) = serde_json::from_str(text.as_str())
      {
        found.push((
          message.kind.to_string(),
          message.maybe_recipient_device_id.map(|d| d.to_string()),
        ));
      }
    }
    found
  }

  #[tokio::test]
  async fn commits_reach_all_members_and_welcomes_only_their_recipient() {
    let app = app!();
    let g = group(&app).await;
    let erin = member("erin");
    let space = app.get(&owner(), "/spaces").await.body["spaces"][0]["id"]
      .as_str()
      .unwrap()
      .to_owned();
    let erin_id = user_id(&app, &erin).await;
    add_space_member(&app, &owner(), &space, &erin_id, "member").await;
    add_channel_member(&app, &owner(), &g.channel, &erin_id).await;
    let erin_device = register_device(&app, &erin).await;

    let mut carol = connect(&app, &g.carol, &g.carol_device).await;
    let mut dave = connect(&app, &g.dave, &g.dave_device).await;
    let mut erin_socket = connect(&app, &erin, &erin_device).await;

    let reply = commit(
      &app,
      &g.carol,
      &g.channel,
      &g.carol_device,
      0,
      json!([{"device_id": g.dave_device, "welcome": b64("w")}]),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);

    assert_eq!(mls_events(&mut carol).await, vec![("commit".to_owned(), None)]);
    let dave_events = mls_events(&mut dave).await;
    assert!(
      dave_events.contains(&("commit".to_owned(), None)),
      "{dave_events:?}"
    );
    assert!(
      dave_events.contains(&("welcome".to_owned(), Some(g.dave_device.clone()))),
      "{dave_events:?}"
    );
    // erin is a member and gets the commit, but not dave's welcome.
    assert_eq!(
      mls_events(&mut erin_socket).await,
      vec![("commit".to_owned(), None)]
    );
    app.stop().await;
  }
}
