//! Call roster, `call_*` gateway events and ringing (`--features integration`, needs `DATABASE_URL`).
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::path::PathBuf;
use std::time::Duration;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_proto::GatewayEvent;
use teamcraft_server::config::CallsConfig;
use teamcraft_server::testing::{TestApp, TestUser};

/// Throwaway Ed25519 key used only by these tests. It protects nothing.
const SIGNING_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----
MC4CAQAwBQYDK2VwBCIEIAxxS2yuWa9SUUAfZhI0772UQ48VXZLz45+L2A5IfcWA
-----END PRIVATE KEY-----
";
const SFU_TOKEN: &str = "sfu-registration-token-for-tests";
const QUIET: Duration = Duration::from_millis(400);

macro_rules! app {
  ($ring_timeout_secs:expr) => {
    match TestApp::start_with_calls_config(
      &calls_config($ring_timeout_secs, Some(SFU_TOKEN)),
      SIGNING_KEY_PEM,
    )
    .await
    {
      Some(app) => app,
      None => return,
    }
  };
}

mod channels {
  use super::*;

  #[tokio::test]
  async fn roster_follows_the_sfu_reports_and_members_hear_about_it() {
    let app = app!(30);
    let (alice, bob) = (owner(), member("bob"));
    let channel = channel_with(&app, &[&bob]).await;
    let (alice_device, bob_device) = (
      register_device(&app, &alice).await,
      register_device(&app, &bob).await,
    );
    let (alice_id, bob_id) = (user_id(&app, &alice).await, user_id(&app, &bob).await);
    let mut bob_socket = ws::connect(&app, &bob, &bob_device).await;
    let eve = member("eve");
    let eve_device = register_device(&app, &eve).await;
    let mut eve_socket = ws::connect(&app, &eve, &eve_device).await;
    ws::events(&mut bob_socket, QUIET).await;

    let call_id = join_token(&app, &alice, &channel, &alice_device).await;
    assert_eq!(
      app
        .get(&alice, &format!("/conversations/{channel}/call"))
        .await
        .status,
      StatusCode::NOT_FOUND
    );

    assert_eq!(
      report(
        &app,
        "joined",
        Some(SFU_TOKEN),
        &call_id,
        &alice_id,
        &alice_device
      )
      .await,
      StatusCode::NO_CONTENT
    );
    let start = call_events(&ws::events(&mut bob_socket, QUIET).await);
    assert_eq!(start.len(), 1, "{start:?}");
    assert_eq!(start[0]["type"], "call_start");
    assert_eq!(start[0]["call_id"], call_id.as_str());
    assert_eq!(start[0]["started_by"], alice_id.as_str());
    assert_eq!(start[0]["is_ringing"], false);

    report(&app, "joined", Some(SFU_TOKEN), &call_id, &bob_id, &bob_device).await;
    let update = call_events(&ws::events(&mut bob_socket, QUIET).await);
    assert_eq!(update[0]["type"], "call_update");
    assert_eq!(update[0]["participants"].as_array().unwrap().len(), 2);

    let state = app.get(&bob, &format!("/conversations/{channel}/call")).await;
    assert_eq!(state.status, StatusCode::OK, "{}", state.body);
    assert_eq!(state.body["call_id"], call_id.as_str());
    assert_eq!(state.body["participants"].as_array().unwrap().len(), 2);

    // Reports are idempotent.
    report(&app, "joined", Some(SFU_TOKEN), &call_id, &bob_id, &bob_device).await;
    assert!(call_events(&ws::events(&mut bob_socket, QUIET).await).is_empty());

    report(&app, "left", Some(SFU_TOKEN), &call_id, &alice_id, &alice_device).await;
    let update = call_events(&ws::events(&mut bob_socket, QUIET).await);
    assert_eq!(update[0]["type"], "call_update");
    assert_eq!(update[0]["participants"].as_array().unwrap().len(), 1);

    report(&app, "left", Some(SFU_TOKEN), &call_id, &bob_id, &bob_device).await;
    let end = call_events(&ws::events(&mut bob_socket, QUIET).await);
    assert_eq!(
      (end[0]["type"].as_str(), end[0]["call_id"].as_str()),
      (Some("call_end"), Some(call_id.as_str()))
    );
    assert_eq!(
      app
        .get(&bob, &format!("/conversations/{channel}/call"))
        .await
        .status,
      StatusCode::NOT_FOUND
    );

    assert!(
      call_events(&ws::events(&mut eve_socket, QUIET).await).is_empty(),
      "non-members hear nothing"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn only_members_can_read_the_roster() {
    let app = app!(30);
    let alice = owner();
    let channel = channel_with(&app, &[]).await;
    let device = register_device(&app, &alice).await;
    let call_id = join_token(&app, &alice, &channel, &device).await;
    report(
      &app,
      "joined",
      Some(SFU_TOKEN),
      &call_id,
      &user_id(&app, &alice).await,
      &device,
    )
    .await;

    let reply = app
      .get(&member("eve"), &format!("/conversations/{channel}/call"))
      .await;

    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.body);
    app.stop().await;
  }

  /// Issue #976: the list a client checks its roster against names the members' running calls
  /// only, and a call both left is gone from it, whoever left first.
  #[tokio::test]
  async fn the_running_calls_list_follows_the_roster_for_members_only() {
    let app = app!(30);
    let (alice, bob) = (owner(), member("bob"));
    let channel = channel_with(&app, &[&bob]).await;
    let (alice_device, bob_device) = (
      register_device(&app, &alice).await,
      register_device(&app, &bob).await,
    );
    let (alice_id, bob_id) = (user_id(&app, &alice).await, user_id(&app, &bob).await);
    for (first, second) in [
      ((&alice_id, &alice_device), (&bob_id, &bob_device)),
      ((&bob_id, &bob_device), (&alice_id, &alice_device)),
    ] {
      let call_id = join_token(&app, &alice, &channel, &alice_device).await;
      report(
        &app,
        "joined",
        Some(SFU_TOKEN),
        &call_id,
        &alice_id,
        &alice_device,
      )
      .await;
      report(&app, "joined", Some(SFU_TOKEN), &call_id, &bob_id, &bob_device).await;

      let listed = running_calls(&app, &bob).await;
      assert_eq!(listed.len(), 1, "{listed:?}");
      assert_eq!(listed[0]["conversation_id"], channel.as_str());
      assert_eq!(listed[0]["call_id"], call_id.as_str());
      assert_eq!(listed[0]["participants"].as_array().unwrap().len(), 2);
      assert!(
        running_calls(&app, &member("eve")).await.is_empty(),
        "not a member"
      );

      report(&app, "left", Some(SFU_TOKEN), &call_id, first.0, first.1).await;
      let listed = running_calls(&app, &alice).await;
      assert_eq!(listed[0]["participants"].as_array().unwrap().len(), 1);
      assert_eq!(listed[0]["participants"][0]["device_id"], second.1.as_str());
      report(&app, "left", Some(SFU_TOKEN), &call_id, second.0, second.1).await;
      assert!(running_calls(&app, &alice).await.is_empty());
      assert!(running_calls(&app, &bob).await.is_empty());
    }
    app.stop().await;
  }

  async fn running_calls(app: &TestApp, user: &TestUser) -> Vec<Value> {
    let reply = app.get(user, "/calls").await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    reply.body["calls"].as_array().unwrap().clone()
  }
}

mod direct_messages {
  use super::*;

  #[tokio::test]
  async fn starting_a_call_rings_the_other_person_until_they_join() {
    let app = app!(30);
    let (alice, bob) = (owner(), member("bob"));
    let (alice_device, bob_device) = (
      register_device(&app, &alice).await,
      register_device(&app, &bob).await,
    );
    let (alice_id, bob_id) = (user_id(&app, &alice).await, user_id(&app, &bob).await);
    let dm = app.post(&alice, "/dms", json!({ "user_id": bob_id })).await.id();
    let mut alice_socket = ws::connect(&app, &alice, &alice_device).await;
    let mut bob_socket = ws::connect(&app, &bob, &bob_device).await;
    ws::events(&mut bob_socket, QUIET).await;
    ws::events(&mut alice_socket, QUIET).await;

    let call_id = join_token(&app, &alice, &dm, &alice_device).await;
    report(
      &app,
      "joined",
      Some(SFU_TOKEN),
      &call_id,
      &alice_id,
      &alice_device,
    )
    .await;

    let bob_events = call_events(&ws::events(&mut bob_socket, QUIET).await);
    let kinds: Vec<_> = bob_events.iter().map(|e| e["type"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["call_start", "call_ring"]);
    assert_eq!(bob_events[0]["is_ringing"], true);
    assert_eq!(bob_events[1]["from_user_id"], alice_id.as_str());
    let alice_events = call_events(&ws::events(&mut alice_socket, QUIET).await);
    assert_eq!(alice_events.len(), 1, "the caller is not rung: {alice_events:?}");
    assert_eq!(alice_events[0]["type"], "call_start");

    report(&app, "joined", Some(SFU_TOKEN), &call_id, &bob_id, &bob_device).await;
    let update = call_events(&ws::events(&mut bob_socket, QUIET).await);
    assert_eq!(update[0]["type"], "call_update");
    assert_eq!(update[0]["is_ringing"], false);
    app.stop().await;
  }

  #[tokio::test]
  async fn ringing_stops_after_the_timeout() {
    let app = app!(1);
    let (alice, bob) = (owner(), member("bob"));
    let (alice_device, bob_device) = (
      register_device(&app, &alice).await,
      register_device(&app, &bob).await,
    );
    let (alice_id, bob_id) = (user_id(&app, &alice).await, user_id(&app, &bob).await);
    let dm = app.post(&alice, "/dms", json!({ "user_id": bob_id })).await.id();
    let mut bob_socket = ws::connect(&app, &bob, &bob_device).await;
    ws::events(&mut bob_socket, QUIET).await;

    let call_id = join_token(&app, &alice, &dm, &alice_device).await;
    report(
      &app,
      "joined",
      Some(SFU_TOKEN),
      &call_id,
      &alice_id,
      &alice_device,
    )
    .await;

    // start + ring, then (event driven, up to the wait) the update that ends the ringing.
    let events = call_events(&ws::events(&mut bob_socket, Duration::from_secs(4)).await);
    let last = events.last().unwrap();
    assert_eq!(
      (last["type"].as_str(), last["is_ringing"].as_bool()),
      (Some("call_update"), Some(false)),
      "{events:?}"
    );
    let state = app.get(&bob, &format!("/conversations/{dm}/call")).await;
    assert_eq!(state.body["is_ringing"], false);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_caller_hanging_up_while_ringing_ends_the_call_without_a_later_update() {
    let app = app!(1);
    let (alice, bob) = (owner(), member("bob"));
    let (alice_device, bob_device) = (
      register_device(&app, &alice).await,
      register_device(&app, &bob).await,
    );
    let (alice_id, bob_id) = (user_id(&app, &alice).await, user_id(&app, &bob).await);
    let dm = app.post(&alice, "/dms", json!({ "user_id": bob_id })).await.id();
    let mut bob_socket = ws::connect(&app, &bob, &bob_device).await;
    ws::events(&mut bob_socket, QUIET).await;
    let call_id = join_token(&app, &alice, &dm, &alice_device).await;
    report(
      &app,
      "joined",
      Some(SFU_TOKEN),
      &call_id,
      &alice_id,
      &alice_device,
    )
    .await;
    report(&app, "left", Some(SFU_TOKEN), &call_id, &alice_id, &alice_device).await;

    let events = call_events(&ws::events(&mut bob_socket, Duration::from_secs(2)).await);

    let kinds: Vec<_> = events.iter().map(|e| e["type"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["call_start", "call_ring", "call_end"]);
    assert_eq!(events[2].get("cause"), None, "the last one left");
    app.stop().await;
  }
}

/// Issue #997: a 1:1 call ends for both when one side leaves.
mod one_to_one {
  use super::*;

  #[tokio::test]
  async fn one_side_hanging_up_ends_the_call_for_both_and_all_their_devices() {
    let app = app!(30);
    let (alice, bob) = (owner(), member("bob"));
    let (alice_device, bob_phone, bob_desktop) = (
      register_device(&app, &alice).await,
      register_device(&app, &bob).await,
      register_device(&app, &bob).await,
    );
    let (alice_id, bob_id) = (user_id(&app, &alice).await, user_id(&app, &bob).await);
    let dm = app.post(&alice, "/dms", json!({ "user_id": bob_id })).await.id();
    let mut alice_socket = ws::connect(&app, &alice, &alice_device).await;
    let mut bob_phone_socket = ws::connect(&app, &bob, &bob_phone).await;
    let mut bob_desktop_socket = ws::connect(&app, &bob, &bob_desktop).await;

    let token = app
      .post(
        &alice,
        &format!("/conversations/{dm}/call/join-token"),
        json!({ "device_id": alice_device }),
      )
      .await;
    assert_eq!(token.body["ends_when_one_side_leaves"], true, "{}", token.body);
    let call_id = token.body["call_id"].as_str().unwrap().to_owned();
    report(
      &app,
      "joined",
      Some(SFU_TOKEN),
      &call_id,
      &alice_id,
      &alice_device,
    )
    .await;
    join_token(&app, &bob, &dm, &bob_phone).await;
    report(&app, "joined", Some(SFU_TOKEN), &call_id, &bob_id, &bob_phone).await;
    join_token(&app, &bob, &dm, &bob_desktop).await;
    report(&app, "joined", Some(SFU_TOKEN), &call_id, &bob_id, &bob_desktop).await;
    for socket in [&mut alice_socket, &mut bob_phone_socket, &mut bob_desktop_socket] {
      ws::events(socket, QUIET).await;
    }

    // One of Bob's devices leaving ends nothing: Bob is still in the call.
    report(&app, "left", Some(SFU_TOKEN), &call_id, &bob_id, &bob_desktop).await;
    let update = call_events(&ws::events(&mut alice_socket, QUIET).await);
    assert_eq!(update.len(), 1, "{update:?}");
    assert_eq!(update[0]["type"], "call_update");

    // Alice hangs up: the call ends for both, on every device.
    report(&app, "left", Some(SFU_TOKEN), &call_id, &alice_id, &alice_device).await;
    for socket in [&mut alice_socket, &mut bob_phone_socket, &mut bob_desktop_socket] {
      let events = call_events(&ws::events(socket, QUIET).await);
      let last = events.last().unwrap();
      assert_eq!(
        (
          last["type"].as_str(),
          last["call_id"].as_str(),
          last["cause"].as_str()
        ),
        (Some("call_end"), Some(call_id.as_str()), Some("one_side_left")),
        "{events:?}"
      );
    }
    assert_eq!(
      app.get(&bob, &format!("/conversations/{dm}/call")).await.status,
      StatusCode::NOT_FOUND
    );
    // Bob's phone leaving by itself is no news, and nobody hears of the call again.
    assert_eq!(
      report(&app, "left", Some(SFU_TOKEN), &call_id, &bob_id, &bob_phone).await,
      StatusCode::NOT_FOUND
    );
    assert!(call_events(&ws::events(&mut alice_socket, QUIET).await).is_empty());
    // The history has the call with its end.
    let history = app.get(&alice, "/calls/history").await;
    let entry = history.body["calls"]
      .as_array()
      .unwrap()
      .iter()
      .find(|entry| entry["call_id"] == call_id.as_str())
      .cloned()
      .unwrap();
    assert!(entry["ended_at"].as_i64().is_some(), "{entry}");
    assert_eq!(entry["is_missed"], false);
    app.stop().await;
  }

  #[tokio::test]
  async fn with_the_setting_off_the_call_runs_until_the_last_leaves() {
    let app = app!(30);
    let (alice, bob) = (owner(), member("bob"));
    let mut rules = app.get(&alice, "/admin/settings").await.body["server_rules"].clone();
    assert_eq!(rules["one_to_one_call_ends_for_both"], true, "on by default");
    rules["one_to_one_call_ends_for_both"] = json!(false);
    let saved = app
      .put(
        &alice,
        "/admin/settings",
        json!({"section": "server_rules", "values": rules}),
      )
      .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
    let (alice_device, bob_device) = (
      register_device(&app, &alice).await,
      register_device(&app, &bob).await,
    );
    let (alice_id, bob_id) = (user_id(&app, &alice).await, user_id(&app, &bob).await);
    let dm = app.post(&alice, "/dms", json!({ "user_id": bob_id })).await.id();
    let mut bob_socket = ws::connect(&app, &bob, &bob_device).await;
    let token = app
      .post(
        &alice,
        &format!("/conversations/{dm}/call/join-token"),
        json!({ "device_id": alice_device }),
      )
      .await;
    assert_eq!(token.body.get("ends_when_one_side_leaves"), None);
    let call_id = token.body["call_id"].as_str().unwrap().to_owned();
    report(
      &app,
      "joined",
      Some(SFU_TOKEN),
      &call_id,
      &alice_id,
      &alice_device,
    )
    .await;
    join_token(&app, &bob, &dm, &bob_device).await;
    report(&app, "joined", Some(SFU_TOKEN), &call_id, &bob_id, &bob_device).await;
    ws::events(&mut bob_socket, QUIET).await;

    report(&app, "left", Some(SFU_TOKEN), &call_id, &alice_id, &alice_device).await;

    let events = call_events(&ws::events(&mut bob_socket, QUIET).await);
    let kinds: Vec<_> = events.iter().map(|e| e["type"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["call_update"]);
    assert_eq!(
      app.get(&bob, &format!("/conversations/{dm}/call")).await.status,
      StatusCode::OK
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn a_channel_call_is_no_one_to_one_call() {
    let app = app!(30);
    let (alice, bob) = (owner(), member("bob"));
    let channel = channel_with(&app, &[&bob]).await;
    let (alice_device, bob_device) = (
      register_device(&app, &alice).await,
      register_device(&app, &bob).await,
    );
    let (alice_id, bob_id) = (user_id(&app, &alice).await, user_id(&app, &bob).await);
    let token = app
      .post(
        &alice,
        &format!("/conversations/{channel}/call/join-token"),
        json!({ "device_id": alice_device }),
      )
      .await;
    assert_eq!(token.body.get("ends_when_one_side_leaves"), None);
    let call_id = token.body["call_id"].as_str().unwrap().to_owned();
    report(
      &app,
      "joined",
      Some(SFU_TOKEN),
      &call_id,
      &alice_id,
      &alice_device,
    )
    .await;
    join_token(&app, &bob, &channel, &bob_device).await;
    report(&app, "joined", Some(SFU_TOKEN), &call_id, &bob_id, &bob_device).await;

    report(&app, "left", Some(SFU_TOKEN), &call_id, &alice_id, &alice_device).await;

    assert_eq!(
      app
        .get(&bob, &format!("/conversations/{channel}/call"))
        .await
        .status,
      StatusCode::OK,
      "a huddle runs until the last one leaves"
    );
    app.stop().await;
  }
}

mod reconciliation {
  use super::*;

  #[tokio::test]
  async fn the_sfu_roster_drops_stale_devices_and_ends_empty_calls() {
    let app = app!(30);
    let (alice, bob) = (owner(), member("bob"));
    let channel = channel_with(&app, &[&bob]).await;
    let (alice_device, bob_device) = (
      register_device(&app, &alice).await,
      register_device(&app, &bob).await,
    );
    let (alice_id, bob_id) = (user_id(&app, &alice).await, user_id(&app, &bob).await);
    let mut bob_socket = ws::connect(&app, &bob, &bob_device).await;
    let call_id = join_token(&app, &alice, &channel, &alice_device).await;
    report(
      &app,
      "joined",
      Some(SFU_TOKEN),
      &call_id,
      &alice_id,
      &alice_device,
    )
    .await;
    report(&app, "joined", Some(SFU_TOKEN), &call_id, &bob_id, &bob_device).await;
    ws::events(&mut bob_socket, QUIET).await;

    // Alice's leave got lost: the SFU only has Bob.
    let only_bob = json!({ "uptime_secs": 600, "calls": [{
      "call_id": call_id,
      "participants": [{ "user_id": bob_id, "device_id": bob_device }],
    }]});
    assert_eq!(
      roster(&app, Some(SFU_TOKEN), &only_bob).await,
      StatusCode::NO_CONTENT
    );
    let update = call_events(&ws::events(&mut bob_socket, QUIET).await);
    assert_eq!(update[0]["type"], "call_update", "{update:?}");
    assert_eq!(update[0]["participants"].as_array().unwrap().len(), 1);

    // The SFU has no call any more.
    let empty = json!({ "uptime_secs": 630, "calls": [] });
    roster(&app, Some(SFU_TOKEN), &empty).await;
    let end = call_events(&ws::events(&mut bob_socket, QUIET).await);
    assert_eq!(
      (end[0]["type"].as_str(), end[0]["call_id"].as_str()),
      (Some("call_end"), Some(call_id.as_str()))
    );
    assert_eq!(
      app
        .get(&bob, &format!("/conversations/{channel}/call"))
        .await
        .status,
      StatusCode::NOT_FOUND
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn leaving_on_all_devices_clears_the_users_devices() {
    let app = app!(30);
    let (alice, bob) = (owner(), member("bob"));
    let channel = channel_with(&app, &[&bob]).await;
    let (laptop, phone, bob_device) = (
      register_device(&app, &alice).await,
      register_device(&app, &alice).await,
      register_device(&app, &bob).await,
    );
    let (alice_id, bob_id) = (user_id(&app, &alice).await, user_id(&app, &bob).await);
    let mut bob_socket = ws::connect(&app, &bob, &bob_device).await;
    let call_id = join_token(&app, &alice, &channel, &laptop).await;
    report(&app, "joined", Some(SFU_TOKEN), &call_id, &alice_id, &laptop).await;
    report(&app, "joined", Some(SFU_TOKEN), &call_id, &alice_id, &phone).await;
    report(&app, "joined", Some(SFU_TOKEN), &call_id, &bob_id, &bob_device).await;
    ws::events(&mut bob_socket, QUIET).await;

    let path = format!("/conversations/{channel}/call/leave");
    let reply = app.post(&alice, &path, json!({})).await;

    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
    let update = call_events(&ws::events(&mut bob_socket, QUIET).await);
    assert_eq!(update[0]["type"], "call_update", "{update:?}");
    let participants = update[0]["participants"].as_array().unwrap();
    assert_eq!(participants.len(), 1);
    assert_eq!(participants[0]["user_id"], bob_id.as_str());
    // A retry after a lost answer changes nothing.
    assert_eq!(
      app.post(&alice, &path, json!({})).await.status,
      StatusCode::NO_CONTENT
    );
    assert!(call_events(&ws::events(&mut bob_socket, QUIET).await).is_empty());
    app.stop().await;
  }

  #[tokio::test]
  async fn a_restarted_server_takes_over_the_call_from_the_first_roster() {
    // This server never issued a token for the call: it ran on the SFU through a restart.
    let app = app!(30);
    let (alice, bob) = (owner(), member("bob"));
    let channel = channel_with(&app, &[&bob]).await;
    let (alice_device, bob_device) = (
      register_device(&app, &alice).await,
      register_device(&app, &bob).await,
    );
    let (alice_id, bob_id) = (user_id(&app, &alice).await, user_id(&app, &bob).await);
    let mut bob_socket = ws::connect(&app, &bob, &bob_device).await;
    ws::events(&mut bob_socket, QUIET).await;
    let call_id = "987654321";

    let running = json!({ "uptime_secs": 600, "calls": [{
      "call_id": call_id,
      "conversation_id": channel,
      "started_by": alice_id,
      "started_at": 1_800_000_000_000_i64,
      "participants": [
        { "user_id": alice_id, "device_id": alice_device },
        { "user_id": bob_id, "device_id": bob_device },
      ],
    }]});
    assert_eq!(
      roster(&app, Some(SFU_TOKEN), &running).await,
      StatusCode::NO_CONTENT
    );

    let update = call_events(&ws::events(&mut bob_socket, QUIET).await);
    assert_eq!(update.len(), 1, "{update:?}");
    assert_eq!(update[0]["type"], "call_update");
    assert_eq!(update[0]["is_ringing"], false, "nothing rings again");
    let state = app.get(&bob, &format!("/conversations/{channel}/call")).await;
    assert_eq!(state.status, StatusCode::OK, "{}", state.body);
    assert_eq!(state.body["call_id"], call_id);
    assert_eq!(state.body["participants"].as_array().unwrap().len(), 2);
    // A new join token names the running call.
    assert_eq!(join_token(&app, &bob, &channel, &bob_device).await, call_id);
    app.stop().await;
  }

  #[tokio::test]
  async fn an_sfu_restart_keeps_the_call_while_everybody_rejoins() {
    let app = app!(30);
    let (alice, bob) = (owner(), member("bob"));
    let channel = channel_with(&app, &[&bob]).await;
    let (alice_device, bob_device) = (
      register_device(&app, &alice).await,
      register_device(&app, &bob).await,
    );
    let (alice_id, bob_id) = (user_id(&app, &alice).await, user_id(&app, &bob).await);
    let mut bob_socket = ws::connect(&app, &bob, &bob_device).await;
    let call_id = join_token(&app, &alice, &channel, &alice_device).await;
    report(
      &app,
      "joined",
      Some(SFU_TOKEN),
      &call_id,
      &alice_id,
      &alice_device,
    )
    .await;
    report(&app, "joined", Some(SFU_TOKEN), &call_id, &bob_id, &bob_device).await;
    ws::events(&mut bob_socket, QUIET).await;

    // The restarted SFU has no rooms yet; then both rejoin.
    roster(&app, Some(SFU_TOKEN), &json!({ "uptime_secs": 0, "calls": [] })).await;
    report(
      &app,
      "joined",
      Some(SFU_TOKEN),
      &call_id,
      &alice_id,
      &alice_device,
    )
    .await;
    report(&app, "joined", Some(SFU_TOKEN), &call_id, &bob_id, &bob_device).await;
    let back = json!({ "uptime_secs": 30, "calls": [{
      "call_id": call_id,
      "participants": [
        { "user_id": alice_id, "device_id": alice_device },
        { "user_id": bob_id, "device_id": bob_device },
      ],
    }]});
    roster(&app, Some(SFU_TOKEN), &back).await;

    assert!(
      call_events(&ws::events(&mut bob_socket, QUIET).await).is_empty(),
      "no call_end, no re-ring"
    );
    let state = app.get(&bob, &format!("/conversations/{channel}/call")).await;
    assert_eq!(state.body["call_id"], call_id.as_str());
    app.stop().await;
  }

  #[tokio::test]
  async fn rosters_need_the_registration_token() {
    let app = app!(30);
    let empty = json!({ "uptime_secs": 600, "calls": [] });

    let missing = roster(&app, None, &empty).await;
    let wrong = roster(&app, Some("not-the-token"), &empty).await;

    assert_eq!(
      (missing, wrong),
      (StatusCode::UNAUTHORIZED, StatusCode::UNAUTHORIZED)
    );
    app.stop().await;
  }

  /// Posts an SFU roster to the internal endpoint and returns the status.
  async fn roster(app: &TestApp, maybe_token: Option<&str>, body: &Value) -> StatusCode {
    let mut request = app
      .http
      .post(format!("{}/api/internal/v1/calls/roster", app.base_url))
      .json(body);
    if let Some(token) = maybe_token {
      request = request.bearer_auth(token);
    }
    request.send().await.unwrap().status()
  }
}

mod internal_endpoint {
  use super::*;

  #[tokio::test]
  async fn reports_need_the_registration_token() {
    let app = app!(30);
    let alice = owner();
    let channel = channel_with(&app, &[]).await;
    let device = register_device(&app, &alice).await;
    let id = user_id(&app, &alice).await;
    let call_id = join_token(&app, &alice, &channel, &device).await;

    let missing = report(&app, "joined", None, &call_id, &id, &device).await;
    let wrong = report(&app, "joined", Some("not-the-token"), &call_id, &id, &device).await;

    assert_eq!(
      (missing, wrong),
      (StatusCode::UNAUTHORIZED, StatusCode::UNAUTHORIZED)
    );
    assert_eq!(
      app
        .get(&alice, &format!("/conversations/{channel}/call"))
        .await
        .status,
      StatusCode::NOT_FOUND
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn unknown_calls_are_rejected_and_unconfigured_servers_are_unavailable() {
    let app = app!(30);
    let alice = owner();
    let device = register_device(&app, &alice).await;
    let id = user_id(&app, &alice).await;
    let unknown = report(&app, "joined", Some(SFU_TOKEN), &id, &id, &device).await;
    assert_eq!(unknown, StatusCode::NOT_FOUND);
    app.stop().await;

    let Some(unconfigured) = TestApp::start_with_calls_config(&calls_config(30, None), SIGNING_KEY_PEM).await
    else {
      return;
    };
    let status = report(&unconfigured, "joined", Some(SFU_TOKEN), &id, &id, &device).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    unconfigured.stop().await;
  }
}

fn calls_config(ring_timeout_secs: u64, maybe_token: Option<&str>) -> CallsConfig {
  CallsConfig {
    sfu_url: "https://sfu.example.com:4433/call".into(),
    issuer: teamcraft_server::testing::TEST_ISSUER.into(),
    signing_key_path: PathBuf::from("unused.pem"),
    join_token_ttl_secs: 60,
    call_idle_secs: 3600,
    maybe_sfu_registration_token: maybe_token.map(|token| token.to_owned().into()),
    ring_timeout_secs,
    maybe_sfu_control_url: None,
    maybe_sfu_certificate_sha256: None,
    roster_grace_secs: 60,
    roster_timeout_secs: 120,
    max_devices_per_user_per_call: 3,
  }
}

/// Requests a join token (which creates the call) and returns the call id.
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

/// Posts an SFU report to the internal endpoint and returns the status.
async fn report(
  app: &TestApp,
  action: &str,
  maybe_token: Option<&str>,
  call_id: &str,
  user_id: &str,
  device_id: &str,
) -> StatusCode {
  let mut request = app
    .http
    .post(format!(
      "{}/api/internal/v1/calls/participants/{action}",
      app.base_url
    ))
    .json(&json!({ "call_id": call_id, "user_id": user_id, "device_id": device_id }));
  if let Some(token) = maybe_token {
    request = request.bearer_auth(token);
  }
  request.send().await.unwrap().status()
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
