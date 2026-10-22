//! Call history with missed calls, and the meeting host controls forwarded to the SFU.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode as HttpStatus};
use axum::routing::post;
use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_proto::GatewayEvent;
use teamcraft_server::config::{CallsConfig, MailTexts, MeetingsConfig};
use teamcraft_server::testing::{TestApp, TestUser};

const SIGNING_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----
MC4CAQAwBQYDK2VwBCIEIAxxS2yuWa9SUUAfZhI0772UQ48VXZLz45+L2A5IfcWA
-----END PRIVATE KEY-----
";
const SFU_TOKEN: &str = "sfu-registration-token-for-tests";
const QUIET: Duration = Duration::from_millis(400);

fn calls_config(ring_timeout_secs: u64, maybe_control_url: Option<String>) -> CallsConfig {
  CallsConfig {
    sfu_url: "https://sfu.example.com:4433/call".into(),
    issuer: teamcraft_server::testing::TEST_ISSUER.into(),
    signing_key_path: PathBuf::from("unused.pem"),
    join_token_ttl_secs: 60,
    call_idle_secs: 3600,
    maybe_sfu_registration_token: Some(SFU_TOKEN.to_owned().into()),
    ring_timeout_secs,
    maybe_sfu_control_url: maybe_control_url,
  }
}

async fn join_token(app: &TestApp, user: &TestUser, conversation: &str, device: &str) -> reqwest::StatusCode {
  app
    .post(
      user,
      &format!("/conversations/{conversation}/call/join-token"),
      json!({"device_id": device}),
    )
    .await
    .status
}

async fn call_id(app: &TestApp, user: &TestUser, conversation: &str, device: &str) -> String {
  let reply = app
    .post(
      user,
      &format!("/conversations/{conversation}/call/join-token"),
      json!({"device_id": device}),
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

mod history {
  use super::*;

  struct Dm {
    app: TestApp,
    alice: TestUser,
    bob: TestUser,
    dm: String,
    alice_id: String,
    bob_id: String,
    alice_device: String,
    bob_device: String,
  }

  async fn dm(ring_timeout_secs: u64) -> Option<Dm> {
    let app =
      TestApp::start_with_calls_config(&calls_config(ring_timeout_secs, None), SIGNING_KEY_PEM).await?;
    let (alice, bob) = (owner(), member("bob"));
    let (alice_id, bob_id) = (user_id(&app, &alice).await, user_id(&app, &bob).await);
    let reply = app.post(&alice, "/dms", json!({"user_id": bob_id})).await;
    let dm = reply.id();
    let (alice_device, bob_device) = (
      register_device(&app, &alice).await,
      register_device(&app, &bob).await,
    );
    Some(Dm {
      app,
      alice,
      bob,
      dm,
      alice_id,
      bob_id,
      alice_device,
      bob_device,
    })
  }

  fn kinds(reply: &teamcraft_server::testing::Reply) -> Vec<(String, Value, Value)> {
    reply.body["events"]
      .as_array()
      .unwrap()
      .iter()
      .map(|e| {
        (
          e["kind"].as_str().unwrap().to_owned(),
          e["actor_id"].clone(),
          e["subject_id"].clone(),
        )
      })
      .collect()
  }

  #[tokio::test]
  async fn an_unanswered_ring_becomes_a_missed_call_in_the_dm() {
    let Some(s) = dm(1).await else { return };
    let mut bob_socket = ws::connect(&s.app, &s.bob, &s.bob_device).await;
    ws::events(&mut bob_socket, QUIET).await;
    let call = call_id(&s.app, &s.alice, &s.dm, &s.alice_device).await;
    report(&s.app, "joined", &call, &s.alice_id, &s.alice_device).await;

    // Nobody answers: after the ring timeout the call is missed (once).
    tokio::time::sleep(Duration::from_millis(1600)).await;
    let events = ws::events(&mut bob_socket, QUIET).await;
    let missed: Vec<_> = events
      .iter()
      .filter(|event| matches!(event, GatewayEvent::SystemEvent { event } if event.kind == teamcraft_proto::SystemEventKind::MissedCall))
      .collect();
    assert_eq!(missed.len(), 1, "{events:?}");

    // Alice hangs up afterwards: no second report.
    report(&s.app, "left", &call, &s.alice_id, &s.alice_device).await;
    let listed = s
      .app
      .get(&s.bob, &format!("/conversations/{}/system-events", s.dm))
      .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(
      kinds(&listed),
      vec![("missed_call".to_owned(), json!(s.alice_id), json!(s.bob_id))]
    );

    let bobs = s.app.get(&s.bob, "/calls/history").await;
    assert_eq!(bobs.body["calls"].as_array().unwrap().len(), 1);
    assert_eq!(bobs.body["calls"][0]["is_missed"], true);
    assert_eq!(bobs.body["calls"][0]["participants"], json!([s.alice_id]));
    assert!(bobs.body["calls"][0]["ended_at"].is_i64());
    let alices = s.app.get(&s.alice, "/calls/history").await;
    assert_eq!(
      alices.body["calls"][0]["is_missed"], false,
      "the caller did not miss it"
    );
    s.app.stop().await;
  }

  #[tokio::test]
  async fn a_call_hung_up_before_the_timeout_is_missed_too_and_an_answered_one_is_not() {
    let Some(s) = dm(60).await else { return };
    let call = call_id(&s.app, &s.alice, &s.dm, &s.alice_device).await;
    report(&s.app, "joined", &call, &s.alice_id, &s.alice_device).await;
    report(&s.app, "left", &call, &s.alice_id, &s.alice_device).await;
    let listed = s
      .app
      .get(&s.bob, &format!("/conversations/{}/system-events", s.dm))
      .await;
    assert_eq!(kinds(&listed).len(), 1);

    // The second call is answered.
    let second = call_id(&s.app, &s.alice, &s.dm, &s.alice_device).await;
    assert_ne!(second, call);
    report(&s.app, "joined", &second, &s.alice_id, &s.alice_device).await;
    report(&s.app, "joined", &second, &s.bob_id, &s.bob_device).await;
    report(&s.app, "left", &second, &s.alice_id, &s.alice_device).await;
    report(&s.app, "left", &second, &s.bob_id, &s.bob_device).await;
    let listed = s
      .app
      .get(&s.bob, &format!("/conversations/{}/system-events", s.dm))
      .await;
    assert_eq!(kinds(&listed).len(), 1, "no new missed-call event");

    let history = s.app.get(&s.bob, "/calls/history").await;
    let calls = history.body["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["call_id"], second.as_str(), "newest first");
    assert_eq!(
      (calls[0]["is_missed"].clone(), calls[1]["is_missed"].clone()),
      (json!(false), json!(true))
    );
    assert_eq!(calls[0]["participants"].as_array().unwrap().len(), 2);

    let page = s.app.get(&s.bob, "/calls/history?limit=1").await;
    assert_eq!(
      (
        page.body["calls"].as_array().unwrap().len(),
        page.body["has_more"].clone()
      ),
      (1, json!(true))
    );
    let rest = s
      .app
      .get(&s.bob, &format!("/calls/history?before={second}"))
      .await;
    assert_eq!(rest.body["calls"].as_array().unwrap().len(), 1);
    assert_eq!(rest.body["has_more"], false);

    // Strangers see none of it.
    let eve = member("eve");
    assert_eq!(
      s.app.get(&eve, "/calls/history").await.body["calls"]
        .as_array()
        .unwrap()
        .len(),
      0
    );
    s.app.stop().await;
  }

  #[tokio::test]
  async fn channels_do_not_ring_so_nothing_is_missed() {
    let Some(app) = TestApp::start_with_calls_config(&calls_config(1, None), SIGNING_KEY_PEM).await else {
      return;
    };
    let (alice, bob) = (owner(), member("bob"));
    let channel = channel_with(&app, &[&bob]).await;
    let (alice_id, alice_device) = (user_id(&app, &alice).await, register_device(&app, &alice).await);
    let call = call_id(&app, &alice, &channel, &alice_device).await;
    report(&app, "joined", &call, &alice_id, &alice_device).await;
    report(&app, "left", &call, &alice_id, &alice_device).await;
    let history = app.get(&bob, "/calls/history").await;
    assert_eq!(history.body["calls"][0]["is_missed"], false);
    app.stop().await;
  }
}

mod host_controls {
  use super::*;

  type Seen = Arc<Mutex<Vec<(String, Value)>>>;

  /// A stand-in for the SFU's control endpoint.
  async fn fake_sfu(status: HttpStatus) -> (String, Seen) {
    async fn handler(
      State((status, seen)): State<(HttpStatus, Seen)>,
      headers: HeaderMap,
      body: String,
    ) -> HttpStatus {
      let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
      seen
        .lock()
        .unwrap()
        .push((auth, serde_json::from_str(&body).unwrap()));
      status
    }
    let seen: Seen = Arc::default();
    let app = Router::new()
      .route("/internal/v1/calls/control", post(handler))
      .with_state((status, seen.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, seen)
  }

  fn meetings_config() -> MeetingsConfig {
    MeetingsConfig {
      public_base_url: "https://chat.example.com".into(),
      organizer_fallback_address: "meetings@example.com".into(),
      caldav: None,
      max_invitees: 5,
      guest_token_secret: "0123456789abcdef0123456789abcdef".to_owned().into(),
      guest_grace_secs: 7200,
      maybe_inbox_token: None,
      smtp: None,
      mail_texts: MailTexts::default(),
    }
  }

  struct Scene {
    app: TestApp,
    host: TestUser,
    carol: TestUser,
    dave: TestUser,
    meeting: String,
    chat: String,
    call: String,
    seen: Seen,
    host_id: String,
    carol_id: String,
    devices: [String; 3],
  }

  /// alice hosts an instant meeting; carol and dave are in the call.
  async fn running_meeting(sfu_status: HttpStatus, with_control: bool) -> Option<Scene> {
    let (url, seen) = fake_sfu(sfu_status).await;
    let config = calls_config(30, with_control.then_some(url));
    let app = TestApp::start_with_meetings_and_calls(&meetings_config(), &config, SIGNING_KEY_PEM).await?;
    let (host, carol, dave) = (owner(), member("carol"), member("dave"));
    let (host_id, carol_id, dave_id) = (
      user_id(&app, &host).await,
      user_id(&app, &carol).await,
      user_id(&app, &dave).await,
    );
    let devices = [
      register_device(&app, &host).await,
      register_device(&app, &carol).await,
      register_device(&app, &dave).await,
    ];
    let created = app
      .post(
        &host,
        "/meetings",
        json!({
          "title": "Sprint review",
          "starts_at": 1_893_488_400_000_i64,
          "ends_at": 1_893_492_000_000_i64,
          "invitees": [
            {"kind": "user", "user_id": carol_id},
            {"kind": "user", "user_id": dave_id},
          ],
        }),
      )
      .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let meeting = created.body["id"].as_str().unwrap().to_owned();
    let chat = created.body["conversation_id"].as_str().unwrap().to_owned();
    let call = call_id(&app, &host, &chat, &devices[0]).await;
    report(&app, "joined", &call, &host_id, &devices[0]).await;
    report(&app, "joined", &call, &carol_id, &devices[1]).await;
    report(&app, "joined", &call, &dave_id, &devices[2]).await;
    Some(Scene {
      app,
      host,
      carol,
      dave,
      meeting,
      chat,
      call,
      seen,
      host_id,
      carol_id,
      devices,
    })
  }

  #[tokio::test]
  async fn the_host_controls_the_call_and_the_sfu_is_told() {
    let Some(s) = running_meeting(HttpStatus::NO_CONTENT, true).await else {
      return;
    };
    let path = format!("/meetings/{}/controls", s.meeting);

    for (body, expected) in [
      (json!({"action": "mute_all"}), json!({"action": "mute_all"})),
      (json!({"action": "lock"}), json!({"action": "lock"})),
      (json!({"action": "unlock"}), json!({"action": "unlock"})),
      (
        json!({"action": "remove_participant", "user_id": s.carol_id}),
        json!({"action": "remove_participant", "user_id": s.carol_id}),
      ),
    ] {
      let reply = s.app.post(&s.host, &path, body.clone()).await;
      assert_eq!(reply.status, StatusCode::NO_CONTENT, "{body}: {}", reply.body);
      let seen = s.seen.lock().unwrap();
      let (auth, sent) = seen.last().unwrap();
      assert_eq!(auth, &format!("Bearer {SFU_TOKEN}"));
      assert_eq!(sent["call_id"], s.call.as_str());
      assert_eq!(sent["by_user_id"], s.host_id.as_str());
      for (key, value) in expected.as_object().unwrap() {
        assert_eq!(&sent[key], value, "{body}");
      }
    }
    assert_eq!(s.seen.lock().unwrap().len(), 4);
    s.app.stop().await;
  }

  #[tokio::test]
  async fn a_deactivated_user_is_dropped_from_the_running_call() {
    let Some(s) = running_meeting(HttpStatus::NO_CONTENT, true).await else {
      return;
    };
    let reply = s
      .app
      .post(
        &s.host,
        &format!("/admin/users/{}/deactivate", s.carol_id),
        json!({}),
      )
      .await;
    assert!(reply.status.is_success(), "{}", reply.body);
    let sent = s.seen.lock().unwrap().last().expect("the SFU was told").1.clone();
    assert_eq!(sent["call_id"], s.call.as_str());
    assert_eq!(sent["action"], "remove_participant");
    assert_eq!(sent["user_id"], s.carol_id.as_str());
    s.app.stop().await;
  }

  #[tokio::test]
  async fn only_hosts_may_and_the_host_cannot_be_removed() {
    let Some(s) = running_meeting(HttpStatus::NO_CONTENT, true).await else {
      return;
    };
    let path = format!("/meetings/{}/controls", s.meeting);
    assert_eq!(
      s.app
        .post(&s.carol, &path, json!({"action": "mute_all"}))
        .await
        .status,
      StatusCode::FORBIDDEN
    );
    let stranger = member("eve");
    assert_eq!(
      s.app
        .post(&stranger, &path, json!({"action": "mute_all"}))
        .await
        .status,
      StatusCode::NOT_FOUND
    );
    assert_eq!(
      s.app
        .post(
          &s.host,
          &path,
          json!({"action": "remove_participant", "user_id": s.host_id})
        )
        .await
        .status,
      StatusCode::BAD_REQUEST
    );
    assert_eq!(
      s.app
        .post(&s.host, &path, json!({"action": "dance"}))
        .await
        .status,
      StatusCode::BAD_REQUEST
    );
    assert!(s.seen.lock().unwrap().is_empty(), "nothing reached the SFU");
    s.app.stop().await;
  }

  #[tokio::test]
  async fn locked_meetings_turn_newcomers_away_and_removed_people_stay_out() {
    let Some(s) = running_meeting(HttpStatus::NO_CONTENT, true).await else {
      return;
    };
    let path = format!("/meetings/{}/controls", s.meeting);
    s.app.post(&s.host, &path, json!({"action": "lock"})).await;
    assert_eq!(
      join_token(&s.app, &s.carol, &s.chat, &s.devices[1]).await,
      StatusCode::FORBIDDEN
    );
    assert_eq!(
      join_token(&s.app, &s.host, &s.chat, &s.devices[0]).await,
      StatusCode::OK,
      "the host can always join"
    );
    s.app.post(&s.host, &path, json!({"action": "unlock"})).await;
    assert_eq!(
      join_token(&s.app, &s.carol, &s.chat, &s.devices[1]).await,
      StatusCode::OK
    );

    s.app
      .post(
        &s.host,
        &path,
        json!({"action": "remove_participant", "user_id": s.carol_id}),
      )
      .await;
    assert_eq!(
      join_token(&s.app, &s.carol, &s.chat, &s.devices[1]).await,
      StatusCode::FORBIDDEN
    );
    assert_eq!(
      join_token(&s.app, &s.dave, &s.chat, &s.devices[2]).await,
      StatusCode::OK,
      "others are unaffected"
    );
    s.app.stop().await;
  }

  #[tokio::test]
  async fn ending_the_meeting_closes_it_for_good() {
    let Some(s) = running_meeting(HttpStatus::NO_CONTENT, true).await else {
      return;
    };
    let path = format!("/meetings/{}/controls", s.meeting);
    let ended = s.app.post(&s.host, &path, json!({"action": "end"})).await;
    assert_eq!(ended.status, StatusCode::NO_CONTENT, "{}", ended.body);
    assert_eq!(s.seen.lock().unwrap().last().unwrap().1["action"], "end");
    // The SFU reports everybody gone; nobody gets a new token, not even the host.
    for (user, device) in [(&s.host, &s.devices[0]), (&s.carol, &s.devices[1])] {
      assert_eq!(
        join_token(&s.app, user, &s.chat, device).await,
        StatusCode::CONFLICT
      );
    }
    assert_eq!(
      s.app
        .post(&s.host, &path, json!({"action": "mute_all"}))
        .await
        .status,
      StatusCode::CONFLICT
    );
    let audit = s.app.get(&admin(), "/admin/audit").await;
    assert!(audit.body.to_string().contains("meeting_control"));
    s.app.stop().await;
  }

  #[tokio::test]
  async fn without_a_call_or_a_control_url_the_answer_says_why() {
    let Some(s) = running_meeting(HttpStatus::NO_CONTENT, false).await else {
      return;
    };
    let path = format!("/meetings/{}/controls", s.meeting);
    assert_eq!(
      s.app
        .post(&s.host, &path, json!({"action": "mute_all"}))
        .await
        .status,
      StatusCode::SERVICE_UNAVAILABLE
    );
    s.app.stop().await;

    let Some(s) = running_meeting(HttpStatus::NOT_FOUND, true).await else {
      return;
    };
    let path = format!("/meetings/{}/controls", s.meeting);
    let reply = s.app.post(&s.host, &path, json!({"action": "mute_all"})).await;
    assert_eq!(
      (reply.status, reply.error_code()),
      (StatusCode::NOT_FOUND, "call_not_found")
    );
    s.app.stop().await;

    // Nobody is in the call yet.
    let (url, _) = fake_sfu(HttpStatus::NO_CONTENT).await;
    let Some(app) = TestApp::start_with_meetings_and_calls(
      &meetings_config(),
      &calls_config(30, Some(url)),
      SIGNING_KEY_PEM,
    )
    .await
    else {
      return;
    };
    let host = owner();
    let meeting = app
      .post(
        &host,
        "/meetings",
        json!({"title": "Solo", "starts_at": 1_893_488_400_000_i64, "ends_at": 1_893_492_000_000_i64}),
      )
      .await
      .body["id"]
      .as_str()
      .unwrap()
      .to_owned();
    let reply = app
      .post(
        &host,
        &format!("/meetings/{meeting}/controls"),
        json!({"action": "mute_all"}),
      )
      .await;
    assert_eq!(reply.error_code(), "call_not_found");
    app.stop().await;
  }
}
