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

/// e2ee.md §4.6, removal order: the remover commits the MLS removal first, then drops the server's
/// membership. What the removed member can still get from the server afterwards.
mod removal {
  use super::*;

  #[tokio::test]
  async fn a_removed_member_gets_nothing_of_the_next_epoch() {
    let app = app!();
    let g = group(&app).await;
    let dave_id = user_id(&app, &g.dave).await;
    let alice = owner();

    // Carol commits Dave's removal (no welcome for him), then the owner drops his membership.
    let removal = commit(&app, &g.carol, &g.channel, &g.carol_device, 0, json!([])).await;
    assert_eq!(removal.status, StatusCode::CREATED, "{}", removal.body);
    let removed = app
      .delete(&alice, &format!("/conversations/{}/members/{dave_id}", g.channel))
      .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT, "{}", removed.body);

    // Nothing of the queue, the group information or the next epoch reaches him.
    for path in ["mls", "mls/group-info"] {
      let reply = app
        .get(&g.dave, &format!("/conversations/{}/{path}", g.channel))
        .await;
      assert!(
        matches!(reply.status, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND),
        "{path}: {}",
        reply.status
      );
    }
    let next = commit(
      &app,
      &g.carol,
      &g.channel,
      &g.carol_device,
      1,
      json!([{"device_id": g.dave_device, "welcome": b64("welcome-for-dave")}]),
    )
    .await;
    assert_eq!(next.status, StatusCode::BAD_REQUEST, "{}", next.body);
    let waiting = app
      .get(&g.dave, &format!("/devices/{}/welcomes", g.dave_device))
      .await;
    assert_eq!(waiting.body["messages"], json!([]));
    // Nor can he commit or post into the group any more.
    let own = commit(&app, &g.dave, &g.channel, &g.dave_device, 1, json!([])).await;
    assert!(
      matches!(own.status, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND),
      "{}",
      own.status
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
      maybe_dpop_proof: None,
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

/// A strict channel's admin continues its group from the last good epoch (e2ee.md §4.6.2, D4).
/// A strict channel's hint (e2ee.md §4.6.2, issue #1924): told with a roles commit, shown with the
/// conversation and to a workspace admin outside the group, never decided on; it keeps the channel
/// private.
mod strict_hint {
  use super::*;

  pub(super) fn hint(is_strict: bool, version: u64) -> Value {
    use base64::Engine;
    let hash = base64::engine::general_purpose::STANDARD.encode([7u8; 32]);
    json!({"is_strict": is_strict, "version": version, "hash": hash})
  }

  /// The chain's links of an answer, as (version, change).
  pub(super) fn links(reply: &Reply) -> Vec<(u64, String)> {
    reply.body["changes"]
      .as_array()
      .map(|changes| {
        changes
          .iter()
          .map(|change| {
            (
              change["version"].as_u64().unwrap(),
              change["change"].as_str().unwrap().to_owned(),
            )
          })
          .collect()
      })
      .unwrap_or_default()
  }

  /// A hint with the signed change, as an app of #2251 sends it (the bytes are opaque here).
  pub(super) fn chained_hint(version: u64) -> Value {
    let mut hint = hint(true, version);
    hint["change"] = json!(b64(&format!("change-{version}")));
    hint
  }

  pub(super) async fn roles_commit(app: &TestApp, g: &Group, epoch: u64, strict: Value) -> Reply {
    app
      .post(
        &g.carol,
        &format!("/conversations/{}/mls/commits", g.channel),
        json!({"device_id": g.carol_device, "epoch": epoch, "commit": b64(&format!("commit-{epoch}")), "strict": strict}),
      )
      .await
  }

  #[tokio::test]
  async fn a_roles_commit_tells_the_server_and_admins_outside_read_it() {
    let app = app!();
    let g = group(&app).await;
    let before = app
      .get(&g.carol, &format!("/conversations/{}/mls/strict", g.channel))
      .await;
    assert_eq!(before.status, StatusCode::OK, "{}", before.body);
    assert_eq!(before.body, json!({"epoch": 0}));

    let reply = roles_commit(&app, &g, 0, hint(true, 1)).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);

    let conversation = app.get(&g.dave, &format!("/conversations/{}", g.channel)).await;
    assert_eq!(
      conversation.body["strict"],
      hint(true, 1),
      "{}",
      conversation.body
    );
    // A workspace admin with no device in the group reads the epoch and the hint.
    let outside = app
      .get(&admin(), &format!("/conversations/{}/mls/strict", g.channel))
      .await;
    assert_eq!(outside.status, StatusCode::OK, "{}", outside.body);
    assert_eq!(outside.body, json!({"epoch": 1, "strict": hint(true, 1)}));
    // A commit without a hint leaves it as it was.
    commit(&app, &g.carol, &g.channel, &g.carol_device, 1, json!([])).await;
    let conversation = app.get(&g.dave, &format!("/conversations/{}", g.channel)).await;
    assert_eq!(conversation.body["strict"], hint(true, 1));
    app.stop().await;
  }

  #[tokio::test]
  async fn a_strict_channel_is_made_public_only_after_strict_is_off() {
    let app = app!();
    let g = group(&app).await;
    roles_commit(&app, &g, 0, hint(true, 1)).await;
    let path = format!("/conversations/{}", g.channel);

    let refused = app.patch(&owner(), &path, json!({"visibility": "public"})).await;
    assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.body);
    let refused = app
      .put(
        &owner(),
        &format!("/conversations/{}/visibility", g.channel),
        json!({"visibility": "public"}),
      )
      .await;
    assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.body);

    roles_commit(&app, &g, 1, hint(false, 2)).await;
    let made_public = app.patch(&owner(), &path, json!({"visibility": "public"})).await;
    assert_eq!(made_public.status, StatusCode::OK, "{}", made_public.body);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_malformed_hint_is_refused_with_its_commit() {
    let app = app!();
    let g = group(&app).await;
    let reply = roles_commit(
      &app,
      &g,
      0,
      json!({"is_strict": true, "version": 1, "hash": b64("short")}),
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    let backlog = app
      .get(&g.carol, &format!("/conversations/{}/mls", g.channel))
      .await;
    assert_eq!(backlog.body["current_epoch"], 0);
    app.stop().await;
  }

  /// The signed roles change travels with the commit's hint and stays in the channel's chain
  /// (e2ee.md §4.6.2, "Falling out in a strict channel"): a device that fell out reads the changes
  /// after the version it knew, ascending; the hint itself never shows it.
  #[tokio::test]
  async fn a_roles_commit_keeps_its_signed_change_for_the_chain() {
    let app = app!();
    let g = group(&app).await;
    for epoch in 0..3 {
      let version = epoch + 1;
      let reply = roles_commit(&app, &g, epoch, chained_hint(version)).await;
      assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    }
    // A commit without a hint adds no link.
    commit(&app, &g.carol, &g.channel, &g.carol_device, 3, json!([])).await;
    let path = format!("/conversations/{}/mls/strict", g.channel);

    let bare = app.get(&g.dave, &path).await;
    assert_eq!(bare.body, json!({"epoch": 4, "strict": hint(true, 3)}));
    let all = app.get(&g.dave, &format!("{path}?after=0")).await;
    assert_eq!(all.status, StatusCode::OK, "{}", all.body);
    assert_eq!(
      links(&all),
      vec![(1, b64("change-1")), (2, b64("change-2")), (3, b64("change-3")),]
    );
    assert_eq!(all.body.get("has_more"), None);
    let after_known = app.get(&g.dave, &format!("{path}?after=2")).await;
    assert_eq!(links(&after_known), vec![(3, b64("change-3"))]);
    let nothing_newer = app.get(&g.dave, &format!("{path}?after=3")).await;
    assert_eq!(nothing_newer.body, json!({"epoch": 4, "strict": hint(true, 3)}));
    let conversation = app.get(&g.dave, &format!("/conversations/{}", g.channel)).await;
    assert_eq!(conversation.body["strict"], hint(true, 3));
    app.stop().await;
  }

  /// At most 64 changes per answer; `has_more` says to ask again after the last.
  #[tokio::test]
  async fn the_chain_is_read_in_pages() {
    let app = app!();
    let g = group(&app).await;
    for epoch in 0..66 {
      let reply = roles_commit(&app, &g, epoch, chained_hint(epoch + 1)).await;
      assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    }
    let path = format!("/conversations/{}/mls/strict", g.channel);

    let first = app.get(&g.carol, &format!("{path}?after=0")).await;
    let versions =
      |reply: &Reply| -> Vec<u64> { links(reply).into_iter().map(|(version, _)| version).collect() };
    assert_eq!(versions(&first), (1..=64).collect::<Vec<_>>());
    assert_eq!(first.body["has_more"], true);
    let last = &first.body["changes"][63]["commit"];
    let second = app
      .get(
        &g.carol,
        &format!("{path}?after=64&after_commit={}", last.as_str().unwrap()),
      )
      .await;
    assert_eq!(versions(&second), vec![65, 66]);
    assert_eq!(second.body.get("has_more"), None);
    app.stop().await;
  }

  /// A member's commit plants junk under the next version first: the admin's real change of that
  /// version is still taken, and both stay, in commit order, for the device to judge. Nothing a
  /// member stores blocks a roles change.
  #[tokio::test]
  async fn a_planted_change_blocks_no_roles_change() {
    let app = app!();
    let g = group(&app).await;
    roles_commit(&app, &g, 0, chained_hint(1)).await;
    let mut junk = chained_hint(2);
    junk["change"] = json!(b64("junk-2"));
    let planted = app
      .post(
        &g.dave,
        &format!("/conversations/{}/mls/commits", g.channel),
        json!({"device_id": g.dave_device, "epoch": 1, "commit": b64("commit-1"), "strict": junk}),
      )
      .await;
    assert_eq!(planted.status, StatusCode::CREATED, "{}", planted.body);

    let real = roles_commit(&app, &g, 2, chained_hint(2)).await;

    assert_eq!(real.status, StatusCode::CREATED, "{}", real.body);
    let chain = app
      .get(
        &g.carol,
        &format!("/conversations/{}/mls/strict?after=1", g.channel),
      )
      .await;
    assert_eq!(links(&chain), vec![(2, b64("junk-2")), (2, b64("change-2"))]);
    // The next page starts after the junk's commit, within its version.
    let junk_commit = chain.body["changes"][0]["commit"].as_str().unwrap().to_owned();
    let rest = app
      .get(
        &g.carol,
        &format!(
          "/conversations/{}/mls/strict?after=2&after_commit={junk_commit}",
          g.channel
        ),
      )
      .await;
    assert_eq!(links(&rest), vec![(2, b64("change-2"))]);
    // A hint without the change (an older app) is stored as before.
    let older = roles_commit(&app, &g, 3, hint(true, 3)).await;
    assert_eq!(older.status, StatusCode::CREATED, "{}", older.body);
    app.stop().await;
  }

  #[tokio::test]
  async fn an_empty_or_oversized_change_is_refused_with_its_commit() {
    let app = app!();
    let g = group(&app).await;
    let mut empty = hint(true, 1);
    empty["change"] = json!("");
    let reply = roles_commit(&app, &g, 0, empty).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    let mut oversized = hint(true, 1);
    oversized["change"] = json!(b64(&"x".repeat(8 * 1024 + 1)));
    let reply = roles_commit(&app, &g, 0, oversized).await;
    assert_eq!(reply.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", reply.body);
    let mut largest = hint(true, 1);
    largest["change"] = json!(b64(&"x".repeat(8 * 1024)));
    let reply = roles_commit(&app, &g, 0, largest).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    app.stop().await;
  }

  /// Decision 3 of #2251: a device that fell out (no group, a rejoin request in the queue) reads
  /// the chain, as its user is still a member. The request is a proposal, which only members and
  /// holders of `manage_members` post.
  #[tokio::test]
  async fn a_member_whose_device_fell_out_reads_the_chain() {
    let app = app!();
    let g = group(&app).await;
    roles_commit(&app, &g, 0, chained_hint(1)).await;
    let rejoin = app
      .post(
        &g.dave,
        &format!("/conversations/{}/mls/messages", g.channel),
        json!({"device_id": g.dave_device, "epoch": 1, "kind": "proposal", "payload": b64("rejoin")}),
      )
      .await;
    assert_eq!(rejoin.status, StatusCode::CREATED, "{}", rejoin.body);

    let reply = app
      .get(
        &g.dave,
        &format!("/conversations/{}/mls/strict?after=0", g.channel),
      )
      .await;

    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(links(&reply), vec![(1, b64("change-1"))]);
    // Somebody who isn't a member can't post one, and reads nothing.
    let erin = member("erin");
    let erin_device = register_device(&app, &erin).await;
    let refused = app
      .post(
        &erin,
        &format!("/conversations/{}/mls/messages", g.channel),
        json!({"device_id": erin_device, "epoch": 1, "kind": "proposal", "payload": b64("rejoin")}),
      )
      .await;
    assert!(refused.status.is_client_error(), "{}", refused.body);
    let outside = app
      .get(&erin, &format!("/conversations/{}/mls/strict?after=0", g.channel))
      .await;
    assert!(outside.status.is_client_error(), "{}", outside.body);
    app.stop().await;
  }

  /// A member of the workspace with no device in the group and no admin role reads nothing.
  #[tokio::test]
  async fn somebody_outside_without_manage_members_reads_nothing() {
    let app = app!();
    let g = group(&app).await;
    let erin = member("erin");
    user_id(&app, &erin).await;
    let reply = app
      .get(&erin, &format!("/conversations/{}/mls/strict", g.channel))
      .await;
    assert!(reply.status.is_client_error(), "{}", reply.body);
    app.stop().await;
  }
}

mod rewind {
  use super::*;

  /// A server with strict channels on (`[policy] strict_channels`).
  macro_rules! strict_app {
    () => {
      match TestApp::start_with(|settings| settings.policy.strict_channels = true).await {
        Some(app) => app,
        None => return,
      }
    };
  }

  async fn rewind(app: &TestApp, user: &TestUser, channel: &str, device: &str, epoch: u64) -> Reply {
    app
      .post(
        user,
        &format!("/conversations/{channel}/mls/rewind"),
        json!({"device_id": device, "epoch": epoch, "commit": b64(&format!("rewind-{epoch}")), "group_info": b64("info-rewound")}),
      )
      .await
  }

  #[tokio::test]
  async fn an_admin_drops_what_was_built_on_a_refused_commit_and_continues() {
    let app = strict_app!();
    let g = group(&app).await;
    let alice = owner();
    let alice_device = register_device(&app, &alice).await;
    commit(&app, &g.carol, &g.channel, &g.carol_device, 0, json!([])).await;
    // The refused commit of epoch 1, a welcome it made and an entry built on it.
    let refused = commit(
      &app,
      &g.dave,
      &g.channel,
      &g.dave_device,
      1,
      json!([{"device_id": g.carol_device, "welcome": b64("welcome-2")}]),
    )
    .await;
    assert_eq!(refused.status, StatusCode::CREATED, "{}", refused.body);
    commit(&app, &g.dave, &g.channel, &g.dave_device, 2, json!([])).await;

    let reply = rewind(&app, &alice, &g.channel, &alice_device, 1).await;

    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert_eq!(reply.body["new_epoch"], 2);
    assert_eq!(reply.body["commit"]["epoch"], 1);
    let backlog = app
      .get(&g.carol, &format!("/conversations/{}/mls", g.channel))
      .await;
    assert_eq!(backlog.body["current_epoch"], 2);
    let payloads: Vec<String> = backlog.body["messages"]
      .as_array()
      .unwrap()
      .iter()
      .filter(|m| m["kind"] == "commit")
      .map(|m| m["payload"].as_str().unwrap().to_owned())
      .collect();
    assert_eq!(payloads, vec![b64("commit-0"), b64("rewind-1")]);
    // The refused commit's welcome went with it.
    let welcomes = app
      .get(&g.carol, &format!("/devices/{}/welcomes", g.carol_device))
      .await;
    assert_eq!(welcomes.body["messages"], json!([]), "{}", welcomes.body);
    // The group information is the rewind's.
    let info = app
      .get(&g.carol, &format!("/conversations/{}/mls/group-info", g.channel))
      .await;
    assert_eq!(info.body, json!({"epoch": 2, "group_info": b64("info-rewound")}));
    // The chain goes on from there.
    let next = commit(&app, &g.carol, &g.channel, &g.carol_device, 2, json!([])).await;
    assert_eq!(next.status, StatusCode::CREATED, "{}", next.body);
    // An audit row names who did it.
    let audit = app.get(&admin(), "/admin/audit?limit=50").await;
    let entries = audit.body["entries"].as_array().unwrap();
    assert!(
      entries.iter().any(|entry| entry["action"] == "mls_group_rewound"),
      "{}",
      audit.body
    );
    app.stop().await;
  }

  /// Two admins continue from the same stop at once (e2ee.md §4.6.2, D4): each names the refused
  /// commit it replaces, so the second finds the first one's continuation there and gets an epoch
  /// conflict instead of dropping it.
  #[tokio::test]
  async fn a_second_rewind_does_not_drop_the_first_admins_continuation() {
    let app = strict_app!();
    let g = group(&app).await;
    let alice = owner();
    let alice_device = register_device(&app, &alice).await;
    commit(&app, &g.carol, &g.channel, &g.carol_device, 0, json!([])).await;
    let refused = commit(&app, &g.dave, &g.channel, &g.dave_device, 1, json!([])).await;
    let refused_id = refused.body["commit"]["id"].clone();
    let continue_from = |payload: &str| json!({"device_id": alice_device, "epoch": 1, "commit": b64(payload), "replaces": refused_id});
    let path = format!("/conversations/{}/mls/rewind", g.channel);

    let first = app.post(&alice, &path, continue_from("first")).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
    let second = app.post(&alice, &path, continue_from("second")).await;

    assert_eq!(second.status, StatusCode::CONFLICT, "{}", second.body);
    assert_eq!(second.error_code(), "epoch_conflict");
    let backlog = app
      .get(&g.carol, &format!("/conversations/{}/mls", g.channel))
      .await;
    let payloads: Vec<String> = backlog.body["messages"]
      .as_array()
      .unwrap()
      .iter()
      .filter(|m| m["kind"] == "commit")
      .map(|m| m["payload"].as_str().unwrap().to_owned())
      .collect();
    assert_eq!(payloads, vec![b64("commit-0"), b64("first")]);
    app.stop().await;
  }

  /// A rewind drops the commits it rewinds and the roles changes they carried (e2ee.md §4.6.2):
  /// the chain never holds a change the group no longer has, and the version is free again.
  #[tokio::test]
  async fn a_rewind_drops_the_roles_changes_of_the_commits_it_drops() {
    use super::strict_hint::{chained_hint, links, roles_commit};
    let app = strict_app!();
    let g = group(&app).await;
    let alice = owner();
    let alice_device = register_device(&app, &alice).await;
    roles_commit(&app, &g, 0, chained_hint(1)).await;
    // The refused commit of epoch 1 carried version 2, and one built on it version 3.
    roles_commit(&app, &g, 1, chained_hint(2)).await;
    roles_commit(&app, &g, 2, chained_hint(3)).await;
    let path = format!("/conversations/{}/mls/strict?after=0", g.channel);
    assert_eq!(links(&app.get(&g.carol, &path).await).len(), 3);

    let reply = rewind(&app, &alice, &g.channel, &alice_device, 1).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);

    let chain = app.get(&g.carol, &path).await;
    assert_eq!(links(&chain), vec![(1, b64("change-1"))]);
    let mut other = chained_hint(2);
    other["change"] = json!(b64("another-change-2"));
    let next = roles_commit(&app, &g, 2, other).await;
    assert_eq!(next.status, StatusCode::CREATED, "{}", next.body);
    let chain = app.get(&g.carol, &path).await;
    assert_eq!(
      links(&chain),
      vec![(1, b64("change-1")), (2, b64("another-change-2"))]
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn a_plain_member_may_not_rewind() {
    let app = strict_app!();
    let g = group(&app).await;
    commit(&app, &g.carol, &g.channel, &g.carol_device, 0, json!([])).await;
    commit(&app, &g.dave, &g.channel, &g.dave_device, 1, json!([])).await;

    let reply = rewind(&app, &g.carol, &g.channel, &g.carol_device, 1).await;

    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.body);
    let backlog = app
      .get(&g.carol, &format!("/conversations/{}/mls", g.channel))
      .await;
    assert_eq!(backlog.body["current_epoch"], 2);
    assert_eq!(kinds(&backlog), vec!["commit", "commit"]);
    app.stop().await;
  }

  #[tokio::test]
  async fn an_epoch_the_group_has_not_reached_is_a_conflict() {
    let app = strict_app!();
    let g = group(&app).await;
    let alice = owner();
    let alice_device = register_device(&app, &alice).await;
    commit(&app, &g.carol, &g.channel, &g.carol_device, 0, json!([])).await;

    let ahead = rewind(&app, &alice, &g.channel, &alice_device, 3).await;
    assert_eq!(ahead.status, StatusCode::CONFLICT, "{}", ahead.body);
    assert_eq!(ahead.error_code(), "epoch_conflict");
    // The current epoch itself takes it like an ordinary commit.
    let current = rewind(&app, &alice, &g.channel, &alice_device, 1).await;
    assert_eq!(current.status, StatusCode::CREATED, "{}", current.body);
    assert_eq!(current.body["new_epoch"], 2);
    app.stop().await;
  }

  /// Off (the default), the server rewinds no group: it can't tell a strict one from another.
  #[tokio::test]
  async fn with_strict_channels_off_no_group_is_rewound() {
    let app = app!();
    let g = group(&app).await;
    let alice = owner();
    let alice_device = register_device(&app, &alice).await;
    commit(&app, &g.carol, &g.channel, &g.carol_device, 0, json!([])).await;
    commit(&app, &g.dave, &g.channel, &g.dave_device, 1, json!([])).await;

    let reply = rewind(&app, &alice, &g.channel, &alice_device, 1).await;

    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.body);
    let backlog = app
      .get(&g.carol, &format!("/conversations/{}/mls", g.channel))
      .await;
    assert_eq!(backlog.body["current_epoch"], 2);
    assert_eq!(kinds(&backlog), vec!["commit", "commit"]);
    app.stop().await;
  }

  /// At most `MAX_REWIND_EPOCHS` (8) back: further would take back real changes.
  #[tokio::test]
  async fn a_rewind_goes_back_a_few_epochs_at_most() {
    let app = strict_app!();
    let g = group(&app).await;
    let alice = owner();
    let alice_device = register_device(&app, &alice).await;
    for epoch in 0..10 {
      commit(&app, &g.carol, &g.channel, &g.carol_device, epoch, json!([])).await;
    }

    let too_far = rewind(&app, &alice, &g.channel, &alice_device, 1).await;
    assert_eq!(too_far.status, StatusCode::BAD_REQUEST, "{}", too_far.body);
    let backlog = app
      .get(&g.carol, &format!("/conversations/{}/mls?limit=50", g.channel))
      .await;
    assert_eq!(backlog.body["current_epoch"], 10, "nothing was dropped");
    assert_eq!(kinds(&backlog).len(), 10);

    let within = rewind(&app, &alice, &g.channel, &alice_device, 2).await;
    assert_eq!(within.status, StatusCode::CREATED, "{}", within.body);
    assert_eq!(within.body["new_epoch"], 3);
    app.stop().await;
  }
}
