//! Call roster, `call_*` gateway events and ringing (`--features integration`, needs `DATABASE_URL`).
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

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
    app.stop().await;
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
