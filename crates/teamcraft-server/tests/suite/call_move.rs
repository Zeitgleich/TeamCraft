//! A DM call that grows moves into a meeting (`--features integration`, needs `DATABASE_URL`).
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::path::PathBuf;
use std::time::Duration;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_proto::GatewayEvent;
use teamcraft_server::config::{CallsConfig, MailTexts, MeetingsConfig};
use teamcraft_server::testing::{TestApp, TestUser};

/// Throwaway Ed25519 key used only by these tests. It protects nothing.
const SIGNING_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----
MC4CAQAwBQYDK2VwBCIEIAxxS2yuWa9SUUAfZhI0772UQ48VXZLz45+L2A5IfcWA
-----END PRIVATE KEY-----
";
const SFU_TOKEN: &str = "sfu-registration-token-for-tests";
const QUIET: Duration = Duration::from_millis(400);

#[tokio::test]
async fn the_call_moves_and_only_the_added_person_rings() {
  let Some(scene) = dm_call().await else { return };
  let Scene {
    app,
    alice,
    bob,
    carol,
    ids: [alice_id, bob_id, carol_id],
    devices: [alice_device, bob_device, carol_device],
    dm,
  } = &scene;
  let mut bob_socket = ws::connect(app, bob, bob_device).await;
  let mut carol_socket = ws::connect(app, carol, carol_device).await;
  ws::events(&mut bob_socket, QUIET).await;
  ws::events(&mut carol_socket, QUIET).await;

  let meeting = app
    .post(
      alice,
      "/meetings/instant",
      json!({"invitees": [bob_id, carol_id]}),
    )
    .await;
  assert_eq!(meeting.status, StatusCode::CREATED, "{}", meeting.body);
  let room = meeting.body["conversation_id"].as_str().unwrap().to_owned();
  let members = app.get(bob, &format!("/conversations/{room}/members")).await;
  assert_eq!(members.status, StatusCode::OK, "{}", members.body);

  let moved = app
    .post(
      alice,
      &format!("/conversations/{dm}/call/move"),
      json!({"to_conversation_id": room, "ring_user_ids": [carol_id]}),
    )
    .await;
  assert_eq!(moved.status, StatusCode::NO_CONTENT, "{}", moved.body);
  let bob_events = call_events(&ws::events(&mut bob_socket, QUIET).await);
  assert_eq!(bob_events.len(), 1, "{bob_events:?}");
  assert_eq!(bob_events[0]["type"], "call_move");
  assert_eq!(bob_events[0]["conversation_id"], dm.as_str());
  assert_eq!(bob_events[0]["to_conversation_id"], room.as_str());
  assert_eq!(bob_events[0]["moved_by"], alice_id.as_str());
  assert!(
    call_events(&ws::events(&mut carol_socket, QUIET).await).is_empty(),
    "the added person isn't in the DM"
  );

  // Bob's client follows first; the meeting's call starts and rings Carol, not Alice.
  let room_call = join_token(app, bob, &room, bob_device).await;
  report(app, "joined", &room_call, bob_id, bob_device).await;
  let carol_events = call_events(&ws::events(&mut carol_socket, QUIET).await);
  let kinds: Vec<_> = carol_events.iter().map(|e| e["type"].as_str().unwrap()).collect();
  assert_eq!(kinds, ["call_start", "call_ring"], "{carol_events:?}");
  assert_eq!(carol_events[1]["from_user_id"], alice_id.as_str());
  let bob_events = call_events(&ws::events(&mut bob_socket, QUIET).await);
  assert!(
    bob_events.iter().all(|event| event["type"] != "call_ring"),
    "{bob_events:?}"
  );

  // Alice joining doesn't answer for Carol; Carol does.
  let room_call_again = join_token(app, alice, &room, alice_device).await;
  assert_eq!(room_call_again, room_call);
  report(app, "joined", &room_call, alice_id, alice_device).await;
  let state = app.get(carol, &format!("/conversations/{room}/call")).await;
  assert_eq!(state.body["is_ringing"], true, "{}", state.body);
  report(app, "joined", &room_call, carol_id, carol_device).await;
  let state = app.get(carol, &format!("/conversations/{room}/call")).await;
  assert_eq!(state.body["is_ringing"], false, "{}", state.body);

  // The DM is untouched: Carol is no member of it.
  let dm_members = app.get(alice, &format!("/conversations/{dm}/members")).await;
  let listed = dm_members.body.to_string();
  assert!(!listed.contains(carol_id.as_str()), "{listed}");
  scene.app.stop().await;
}

#[tokio::test]
async fn a_move_into_a_running_meeting_call_rings_at_once() {
  let Some(scene) = dm_call().await else { return };
  let Scene {
    app,
    alice,
    carol,
    ids: [alice_id, bob_id, carol_id],
    devices: [alice_device, _, carol_device],
    dm,
    ..
  } = &scene;
  let room = app
    .post(
      alice,
      "/meetings/instant",
      json!({"invitees": [bob_id, carol_id]}),
    )
    .await
    .body["conversation_id"]
    .as_str()
    .unwrap()
    .to_owned();
  let room_call = join_token(app, alice, &room, alice_device).await;
  report(app, "joined", &room_call, alice_id, alice_device).await;
  let mut carol_socket = ws::connect(app, carol, carol_device).await;
  ws::events(&mut carol_socket, QUIET).await;

  let moved = app
    .post(
      alice,
      &format!("/conversations/{dm}/call/move"),
      json!({"to_conversation_id": room, "ring_user_ids": [carol_id]}),
    )
    .await;

  assert_eq!(moved.status, StatusCode::NO_CONTENT, "{}", moved.body);
  let kinds: Vec<_> = call_events(&ws::events(&mut carol_socket, QUIET).await)
    .iter()
    .map(|e| e["type"].as_str().unwrap().to_owned())
    .collect();
  assert_eq!(kinds, ["call_update", "call_ring"]);
  scene.app.stop().await;
}

#[tokio::test]
async fn a_second_move_into_another_room_is_refused() {
  let Some(scene) = dm_call().await else { return };
  let Scene {
    app,
    alice,
    ids: [_, bob_id, carol_id],
    dm,
    ..
  } = &scene;
  let mut rooms = Vec::new();
  for _ in 0..2 {
    let room = app
      .post(
        alice,
        "/meetings/instant",
        json!({"invitees": [bob_id, carol_id]}),
      )
      .await
      .body["conversation_id"]
      .as_str()
      .unwrap()
      .to_owned();
    rooms.push(room);
  }
  let first = app
    .post(
      alice,
      &format!("/conversations/{dm}/call/move"),
      json!({"to_conversation_id": rooms[0]}),
    )
    .await;
  let again = app
    .post(
      alice,
      &format!("/conversations/{dm}/call/move"),
      json!({"to_conversation_id": rooms[0]}),
    )
    .await;
  let other = app
    .post(
      alice,
      &format!("/conversations/{dm}/call/move"),
      json!({"to_conversation_id": rooms[1]}),
    )
    .await;

  assert_eq!(first.status, StatusCode::NO_CONTENT, "{}", first.body);
  assert_eq!(again.status, StatusCode::NO_CONTENT, "a retry: {}", again.body);
  assert_eq!(other.status, StatusCode::CONFLICT, "{}", other.body);
  scene.app.stop().await;
}

#[tokio::test]
async fn only_people_in_the_call_move_it_and_only_into_their_own_meeting() {
  let Some(scene) = dm_call().await else { return };
  let Scene {
    app,
    alice,
    bob,
    carol,
    ids: [_, bob_id, _],
    dm,
    ..
  } = &scene;
  let alices_room = app
    .post(alice, "/meetings/instant", json!({"invitees": [bob_id]}))
    .await
    .body["conversation_id"]
    .as_str()
    .unwrap()
    .to_owned();
  // Bob is in the meeting, but doesn't organize it.
  let not_organizer = app
    .post(
      bob,
      &format!("/conversations/{dm}/call/move"),
      json!({"to_conversation_id": alices_room}),
    )
    .await;
  assert_eq!(
    not_organizer.status,
    StatusCode::FORBIDDEN,
    "{}",
    not_organizer.body
  );

  // Bob isn't in the call (only Alice joined it).
  let bobs_room = app.post(bob, "/meetings/instant", json!({})).await.body["conversation_id"]
    .as_str()
    .unwrap()
    .to_owned();
  let not_in_call = app
    .post(
      bob,
      &format!("/conversations/{dm}/call/move"),
      json!({"to_conversation_id": bobs_room}),
    )
    .await;
  assert_eq!(not_in_call.status, StatusCode::FORBIDDEN, "{}", not_in_call.body);

  // Carol isn't in the DM.
  let carols_room = app.post(carol, "/meetings/instant", json!({})).await.body["conversation_id"]
    .as_str()
    .unwrap()
    .to_owned();
  let outsider = app
    .post(
      carol,
      &format!("/conversations/{dm}/call/move"),
      json!({"to_conversation_id": carols_room}),
    )
    .await;
  assert!(
    matches!(outsider.status, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND),
    "{}",
    outsider.body
  );

  // A channel call doesn't move: it can take more people itself.
  let channel = channel_with(app, &[]).await;
  let alice_id = user_id(app, alice).await;
  let alice_device = register_device(app, alice).await;
  let channel_call = join_token(app, alice, &channel, &alice_device).await;
  report(app, "joined", &channel_call, &alice_id, &alice_device).await;
  let from_channel = app
    .post(
      alice,
      &format!("/conversations/{channel}/call/move"),
      json!({"to_conversation_id": alices_room}),
    )
    .await;
  assert_eq!(
    from_channel.status,
    StatusCode::BAD_REQUEST,
    "{}",
    from_channel.body
  );
  scene.app.stop().await;
}

/// Alice and Bob have a DM; Alice is in its call; Carol is somebody else.
struct Scene {
  app: TestApp,
  alice: TestUser,
  bob: TestUser,
  carol: TestUser,
  ids: [String; 3],
  devices: [String; 3],
  dm: String,
}

async fn dm_call() -> Option<Scene> {
  let app =
    TestApp::start_with_meetings_and_calls(&meetings_config(), &calls_config(), SIGNING_KEY_PEM).await?;
  let (alice, bob, carol) = (owner(), member("bob"), member("carol"));
  let ids = [
    user_id(&app, &alice).await,
    user_id(&app, &bob).await,
    user_id(&app, &carol).await,
  ];
  let devices = [
    register_device(&app, &alice).await,
    register_device(&app, &bob).await,
    register_device(&app, &carol).await,
  ];
  let dm = app.post(&alice, "/dms", json!({ "user_id": ids[1] })).await.id();
  let call = join_token(&app, &alice, &dm, &devices[0]).await;
  report(&app, "joined", &call, &ids[0], &devices[0]).await;
  Some(Scene {
    app,
    alice,
    bob,
    carol,
    ids,
    devices,
    dm,
  })
}

fn meetings_config() -> MeetingsConfig {
  MeetingsConfig {
    public_base_url: "https://chat.example.com".into(),
    organizer_fallback_address: "meetings@example.com".into(),
    caldav: None,
    max_invitees: 10,
    max_shared_link_guests: 50,
    guest_token_secret: "0123456789abcdef0123456789abcdef".to_owned().into(),
    guest_grace_secs: 7200,
    maybe_inbox_token: None,
    smtp: None,
    mail_texts: MailTexts::default(),
  }
}

fn calls_config() -> CallsConfig {
  CallsConfig {
    sfu_url: "https://sfu.example.com:4433/call".into(),
    issuer: teamcraft_server::testing::TEST_ISSUER.into(),
    signing_key_path: PathBuf::from("unused.pem"),
    join_token_ttl_secs: 60,
    call_idle_secs: 3600,
    maybe_sfu_registration_token: Some(SFU_TOKEN.to_owned().into()),
    ring_timeout_secs: 30,
    maybe_sfu_control_url: None,
    maybe_sfu_certificate_sha256: None,
    roster_grace_secs: 60,
    roster_timeout_secs: 120,
  }
}

/// Requests a join token (which opens the call) and returns the call id.
async fn join_token(app: &TestApp, user: &TestUser, conversation: &str, device: &str) -> String {
  let reply = app
    .post(
      user,
      &format!("/conversations/{conversation}/call/join-token"),
      json!({ "device_id": device }),
    )
    .await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  reply.body["call_id"].as_str().unwrap().to_owned()
}

async fn report(app: &TestApp, action: &str, call: &str, user: &str, device: &str) {
  let status = app
    .http
    .post(format!(
      "{}/api/internal/v1/calls/participants/{action}",
      app.base_url
    ))
    .bearer_auth(SFU_TOKEN)
    .json(&json!({"call_id": call, "user_id": user, "device_id": device}))
    .send()
    .await
    .unwrap()
    .status();
  assert_eq!(status, StatusCode::NO_CONTENT);
}

/// The `call_*` events as JSON, in order.
fn call_events(events: &[GatewayEvent]) -> Vec<Value> {
  events
    .iter()
    .map(|event| serde_json::to_value(event).unwrap())
    .filter(|json| {
      json["type"]
        .as_str()
        .is_some_and(|kind| kind.starts_with("call_"))
    })
    .collect()
}
