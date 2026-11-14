//! Link probe tickets (#1959) against a real server and Postgres (`--features integration`, needs
//! `DATABASE_URL`): who gets one, what it is, and its limit.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use common::*;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use teamcraft_proto::{CALL_JOIN_TOKEN_AUDIENCE, CALL_PROBE_TICKET_AUDIENCE, CallProbeClaims, CallTokenKind};
use teamcraft_server::config::{CallsConfig, MailTexts, MeetingsConfig};
use teamcraft_server::testing::{TEST_ISSUER, TestApp};

use super::calls::SIGNING_KEY_PEM;

const PUBLIC_KEY_PEM: &str = "-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEA8YVjY2Zv9XPhv003tFAgzW7J6gfDn+ICj5xCB2LXMVg=
-----END PUBLIC KEY-----
";
const SFU_URL: &str = "https://sfu.example.com:4433/call";
const STARTS_AT: i64 = 1_893_488_400_000;
const ENDS_AT: i64 = STARTS_AT + 3_600_000;
const TICKETS_PER_MINUTE: u32 = 3;

macro_rules! app {
  () => {
    match TestApp::start_with_meetings_and_calls(&meetings_config(), &calls_config(), SIGNING_KEY_PEM).await {
      Some(app) => app,
      None => return,
    }
  };
}

#[tokio::test]
async fn a_member_gets_a_probe_ticket_that_is_no_join_token() {
  let app = app!();
  let alice = owner();
  let device = register_device(&app, &alice).await;

  let reply = app
    .post(&alice, "/calls/probe-ticket", json!({ "device_id": device }))
    .await;

  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  assert_eq!(reply.body["sfu_url"], SFU_URL);
  let ticket = reply.body["ticket"].as_str().unwrap();
  let claims = decode(ticket, CALL_PROBE_TICKET_AUDIENCE).expect("a probe ticket");
  assert_eq!(claims.kind, CallTokenKind::Probe);
  assert_eq!(claims.device_id.to_string(), device);
  assert!(claims.exp - claims.iat <= 60);
  assert_eq!(claims.jti.len(), 32);
  assert!(
    decode(ticket, CALL_JOIN_TOKEN_AUDIENCE).is_none(),
    "not valid where a join token is"
  );
  // Nothing about a call: no call id, and no call runs anywhere.
  assert!(reply.body.get("call_id").is_none());
  let running = app.get(&alice, "/calls").await;
  assert_eq!(
    running.body["calls"].as_array().map(Vec::len).unwrap_or(0),
    0,
    "{}",
    running.body
  );
  app.stop().await;
}

#[tokio::test]
async fn only_the_callers_own_device_and_only_so_many_a_minute() {
  let app = app!();
  let alice = owner();
  let bob = member("bob");
  let alice_device = register_device(&app, &alice).await;
  let bob_device = register_device(&app, &bob).await;

  let foreign = app
    .post(&alice, "/calls/probe-ticket", json!({ "device_id": bob_device }))
    .await;
  assert_eq!(foreign.status, StatusCode::NOT_FOUND, "{}", foreign.body);

  for _ in 0..TICKETS_PER_MINUTE {
    let reply = app
      .post(
        &alice,
        "/calls/probe-ticket",
        json!({ "device_id": alice_device }),
      )
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  }
  let over = app
    .post(
      &alice,
      "/calls/probe-ticket",
      json!({ "device_id": alice_device }),
    )
    .await;
  assert_eq!(over.status, StatusCode::TOO_MANY_REQUESTS, "{}", over.body);
  app.stop().await;
}

#[tokio::test]
async fn a_guest_gets_one_only_while_waiting_in_a_meeting_that_is_on() {
  let app = app!();
  let alice = owner();
  let meeting = schedule(&app, &alice).await;
  let link = shared_link(&app, &alice, &meeting).await;
  let joined = join(&app, &link, "Gus").await;
  let guest_token = joined["guest_token"].as_str().unwrap().to_owned();
  let device = guest_device(&app, &guest_token).await;

  let without_meeting = request(&app, &guest_token, json!({ "device_id": device })).await;
  assert_eq!(without_meeting.0, StatusCode::FORBIDDEN, "{}", without_meeting.1);
  let waiting = request(
    &app,
    &guest_token,
    json!({ "device_id": device, "meeting_id": meeting }),
  )
  .await;
  assert_eq!(waiting.0, StatusCode::OK, "{}", waiting.1);
  let other_meeting = schedule(&app, &alice).await;
  let elsewhere = request(
    &app,
    &guest_token,
    json!({ "device_id": device, "meeting_id": other_meeting }),
  )
  .await;
  assert_eq!(
    elsewhere.0,
    StatusCode::NOT_FOUND,
    "not in that lobby: {}",
    elsewhere.1
  );

  // A refused guest's session ends: no ticket.
  let guest_id = joined["user_id"].as_str().unwrap();
  let denied = app
    .post(
      &alice,
      &format!("/meetings/{meeting}/lobby/{guest_id}/deny"),
      json!({}),
    )
    .await;
  assert_eq!(denied.status, StatusCode::NO_CONTENT, "{}", denied.body);
  let after = request(
    &app,
    &guest_token,
    json!({ "device_id": device, "meeting_id": meeting }),
  )
  .await;
  assert_eq!(after.0, StatusCode::FORBIDDEN, "{}", after.1);

  // Nor a guest of a meeting that ended.
  let second = join(&app, &link, "Gil").await;
  let second_token = second["guest_token"].as_str().unwrap().to_owned();
  let second_device = guest_device(&app, &second_token).await;
  let ended = app
    .post(
      &alice,
      &format!("/meetings/{meeting}/controls"),
      json!({"action": "end"}),
    )
    .await;
  assert_eq!(ended.status, StatusCode::NO_CONTENT, "{}", ended.body);
  let over = request(
    &app,
    &second_token,
    json!({ "device_id": second_device, "meeting_id": meeting }),
  )
  .await;
  assert!(
    matches!(
      over.0,
      StatusCode::FORBIDDEN | StatusCode::CONFLICT | StatusCode::UNAUTHORIZED
    ),
    "{:?} {}",
    over.0,
    over.1
  );
  app.stop().await;
}

async fn schedule(app: &TestApp, organizer: &teamcraft_server::testing::TestUser) -> String {
  let reply = app
    .post(
      organizer,
      "/meetings",
      json!({"title": "Project A", "starts_at": STARTS_AT, "ends_at": ENDS_AT, "has_lobby": true, "invitees": []}),
    )
    .await;
  assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
  reply.id()
}

/// The meeting's shared guest link's token.
async fn shared_link(app: &TestApp, host: &teamcraft_server::testing::TestUser, meeting: &str) -> String {
  let reply = app
    .put(host, &format!("/meetings/{meeting}/shared-guest-link"), json!({}))
    .await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  let url = reply.body["join_url"].as_str().unwrap();
  url
    .split("/meeting/")
    .nth(1)
    .unwrap()
    .split(['#', '?'])
    .next()
    .unwrap()
    .to_owned()
}

async fn join(app: &TestApp, link_token: &str, name: &str) -> Value {
  let response = app
    .http
    .post(format!("{}/api/v1/meetings/join", app.base_url))
    .json(&json!({"link_token": link_token, "display_name": name}))
    .send()
    .await
    .unwrap();
  assert!(response.status().is_success(), "{}", response.status());
  response.json().await.unwrap()
}

async fn guest_device(app: &TestApp, guest_token: &str) -> String {
  let response = app
    .http
    .post(format!("{}/api/v1/devices", app.base_url))
    .bearer_auth(guest_token)
    .json(&json!({"name": "browser", "public_key": "cHVibGlj", "cross_signature": "c2ln"}))
    .send()
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::CREATED);
  let body: Value = response.json().await.unwrap();
  body["id"].as_str().unwrap().to_owned()
}

async fn request(app: &TestApp, guest_token: &str, body: Value) -> (StatusCode, Value) {
  let response = app
    .http
    .request(
      Method::POST,
      format!("{}/api/v1/calls/probe-ticket", app.base_url),
    )
    .bearer_auth(guest_token)
    .json(&body)
    .send()
    .await
    .unwrap();
  let status = response.status();
  (status, response.json().await.unwrap_or(Value::Null))
}

fn decode(ticket: &str, audience: &str) -> Option<CallProbeClaims> {
  let mut validation = Validation::new(Algorithm::EdDSA);
  validation.set_audience(&[audience]);
  validation.set_issuer(&[TEST_ISSUER]);
  jsonwebtoken::decode::<CallProbeClaims>(
    ticket,
    &DecodingKey::from_ed_pem(PUBLIC_KEY_PEM.as_bytes()).unwrap(),
    &validation,
  )
  .ok()
  .map(|data| data.claims)
}

fn meetings_config() -> MeetingsConfig {
  MeetingsConfig {
    public_base_url: "https://chat.example.com".into(),
    organizer_fallback_address: "meetings@example.com".into(),
    caldav: None,
    max_invitees: 20,
    guest_token_secret: "0123456789abcdef0123456789abcdef".to_owned().into(),
    guest_grace_secs: 7200,
    max_shared_link_guests: 3,
    start_ring_lead_secs: 3600,
    maybe_inbox_token: None,
    smtp: None,
    mail_texts: MailTexts::default(),
  }
}

fn calls_config() -> CallsConfig {
  CallsConfig {
    sfu_url: SFU_URL.into(),
    sfu_fallback_enabled: true,
    maybe_sfu_fallback_url: None,
    issuer: TEST_ISSUER.into(),
    signing_key_path: "unused.pem".into(),
    join_token_ttl_secs: 60,
    call_idle_secs: 3600,
    maybe_sfu_registration_token: None,
    ring_timeout_secs: 30,
    maybe_sfu_control_url: None,
    maybe_sfu_certificate_sha256: None,
    roster_grace_secs: 60,
    roster_timeout_secs: 120,
    max_devices_per_user_per_call: 3,
    probe_ticket_ttl_secs: 30,
    probe_tickets_per_device_per_minute: TICKETS_PER_MINUTE,
  }
}
