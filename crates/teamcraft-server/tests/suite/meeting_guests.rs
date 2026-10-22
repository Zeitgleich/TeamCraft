//! Meeting guests: personal one-time links, guest sessions, the lobby and the meeting chat against
//! a real server and Postgres (`--features integration`, needs `DATABASE_URL`).
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use teamcraft_server::config::{MailTexts, MeetingsConfig};
use teamcraft_server::testing::{FakeCalendar, FakeMailer, TestApp, TestUser};

const BASE_URL: &str = "https://chat.example.com";
const STARTS_AT: i64 = 1_893_488_400_000;
const ENDS_AT: i64 = STARTS_AT + 3_600_000;
const QUIET: Duration = Duration::from_millis(500);

struct Rig {
  app: TestApp,
  mailer: Arc<FakeMailer>,
}

async fn start() -> Option<Rig> {
  let config = MeetingsConfig {
    public_base_url: BASE_URL.into(),
    organizer_fallback_address: "meetings@example.com".into(),
    caldav: None,
    max_invitees: 20,
    guest_token_secret: "0123456789abcdef0123456789abcdef".to_owned().into(),
    guest_grace_secs: 7200,
    maybe_inbox_token: None,
    smtp: None,
    mail_texts: MailTexts::default(),
  };
  let mailer = Arc::new(FakeMailer::default());
  let app = TestApp::start_with_meetings(&config, Arc::new(FakeCalendar::default()), mailer.clone()).await?;
  Some(Rig { app, mailer })
}

macro_rules! rig {
  () => {
    match start().await {
      Some(rig) => rig,
      None => return,
    }
  };
}

impl Rig {
  /// Schedules a meeting with one external invitee and returns (meeting id, chat id, invite token).
  async fn meeting_with_guest(&self, organizer: &TestUser, has_lobby: bool) -> (String, String, String) {
    let reply = self
      .app
      .post(
        organizer,
        "/meetings",
        json!({
          "title": "Customer call", "starts_at": STARTS_AT, "ends_at": ENDS_AT, "has_lobby": has_lobby,
          "invitees": [{"kind": "external", "email": "gus@example.org", "name": "Gus"}],
        }),
      )
      .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let mail = self
      .mailer
      .sent()
      .into_iter()
      .find(|m| m.to_address == "gus@example.org")
      .unwrap();
    (
      reply.id(),
      reply.body["conversation_id"].as_str().unwrap().to_owned(),
      invite_token(&mail.text),
    )
  }

  async fn join(&self, token: &str, name: &str) -> (StatusCode, Value) {
    self
      .request(
        None,
        Method::POST,
        "/meetings/join",
        Some(json!({"invite_token": token, "display_name": name})),
      )
      .await
  }

  /// A request with an arbitrary bearer token (guest sessions) or none.
  async fn request(
    &self,
    maybe_token: Option<&str>,
    method: Method,
    path: &str,
    body: Option<Value>,
  ) -> (StatusCode, Value) {
    let mut request = self
      .app
      .http
      .request(method, format!("{}/api/v1{path}", self.app.base_url));
    if let Some(token) = maybe_token {
      request = request.bearer_auth(token);
    }
    if let Some(body) = body {
      request = request.json(&body);
    }
    let response = request.send().await.unwrap();
    let status = response.status();
    (status, response.json().await.unwrap_or(Value::Null))
  }

  /// Registers a device for the guest and returns its ID.
  async fn guest_device(&self, guest_token: &str) -> String {
    let (status, body) = self
      .request(
        Some(guest_token),
        Method::POST,
        "/devices",
        Some(json!({"name": "browser", "public_key": "cHVibGlj", "cross_signature": "c2ln"})),
      )
      .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().unwrap().to_owned()
  }
}

fn invite_token(text: &str) -> String {
  text
    .split("#invite=")
    .nth(1)
    .expect("a personal link")
    .split_whitespace()
    .next()
    .unwrap()
    .to_owned()
}

fn events_of(events: &[teamcraft_proto::GatewayEvent], kind: &str) -> Vec<Value> {
  events
    .iter()
    .map(|event| serde_json::to_value(event).unwrap())
    .filter(|json| json["type"] == kind)
    .collect()
}

mod joining {
  use super::*;

  #[tokio::test]
  async fn a_guest_trades_the_link_for_a_session_waits_in_the_lobby_and_is_admitted() {
    let rig = rig!();
    let app = &rig.app;
    let alice = owner();
    let alice_device = register_device(app, &alice).await;
    let (meeting, chat, token) = rig.meeting_with_guest(&alice, true).await;
    let mut alice_socket = ws::connect(app, &alice, &alice_device).await;
    ws::events(&mut alice_socket, Duration::from_millis(300)).await;

    let (status, joined) = rig.join(&token, "Gus (customer)").await;

    assert_eq!(status, StatusCode::OK, "{joined}");
    assert_eq!(joined["lobby_state"], "waiting");
    assert_eq!(
      (joined["meeting_id"].as_str(), joined["conversation_id"].as_str()),
      (Some(meeting.as_str()), Some(chat.as_str()))
    );
    let guest_token = joined["guest_token"].as_str().unwrap().to_owned();
    let guest_id = joined["user_id"].as_str().unwrap().to_owned();
    assert!(joined["expires_at"].as_i64().unwrap() > ENDS_AT);

    // The host hears about the waiting guest and sees them in the lobby.
    let heard = events_of(&ws::events(&mut alice_socket, QUIET).await, "lobby_join");
    assert_eq!(heard.len(), 1);
    assert_eq!(
      (heard[0]["meeting_id"].as_str(), heard[0]["user"]["id"].as_str()),
      (Some(meeting.as_str()), Some(guest_id.as_str()))
    );
    assert_eq!(heard[0]["user"]["display_name"], "Gus (customer)");
    let lobby = app.get(&alice, &format!("/meetings/{meeting}/lobby")).await;
    assert_eq!(lobby.body["entries"][0]["user"]["id"], guest_id.as_str());

    // The guest is nobody yet: no chat, no meeting details, but their lobby state.
    let guest = |method: Method, path: String, body: Option<Value>| {
      let (rig, token) = (&rig, guest_token.clone());
      async move { rig.request(Some(&token), method, &path, body).await }
    };
    assert_eq!(
      guest(Method::GET, format!("/conversations/{chat}"), None).await.0,
      StatusCode::NOT_FOUND
    );
    assert_eq!(
      guest(Method::GET, format!("/meetings/{meeting}"), None).await.0,
      StatusCode::NOT_FOUND
    );
    assert_eq!(
      guest(Method::GET, format!("/meetings/{meeting}/lobby/me"), None)
        .await
        .1["state"],
      "waiting"
    );
    let guest_device = rig.guest_device(&guest_token).await;
    let mut guest_socket = ws::connect_with_token(app, guest_token.clone(), &guest_device).await;
    ws::events(&mut guest_socket, Duration::from_millis(200)).await;

    // The host admits; the guest hears it and now belongs to the meeting chat.
    let admit = app
      .post(
        &alice,
        &format!("/meetings/{meeting}/lobby/{guest_id}/admit"),
        json!({}),
      )
      .await;
    assert_eq!(admit.status, StatusCode::NO_CONTENT, "{}", admit.body);
    let decision = events_of(&ws::events(&mut guest_socket, QUIET).await, "lobby_decision");
    assert_eq!(
      (decision[0]["meeting_id"].as_str(), decision[0]["state"].as_str()),
      (Some(meeting.as_str()), Some("admitted"))
    );
    assert_eq!(
      guest(Method::GET, format!("/conversations/{chat}"), None).await.0,
      StatusCode::OK
    );
    assert_eq!(
      guest(Method::GET, format!("/meetings/{meeting}"), None).await.0,
      StatusCode::OK
    );

    // Guest and organizer talk in the persisted meeting chat.
    let (status, body) = guest(
      Method::POST,
      format!("/conversations/{chat}/messages"),
      Some(message_body(1, &guest_device)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let history = app.get(&alice, &format!("/conversations/{chat}/messages")).await;
    assert_eq!(history.body["messages"].as_array().unwrap().len(), 1);
    assert!(
      app.get(&alice, &format!("/meetings/{meeting}/lobby")).await.body["entries"]
        .as_array()
        .unwrap()
        .is_empty()
    );
    rig.app.stop().await;
  }

  #[tokio::test]
  async fn a_link_works_once_and_only_with_our_signature() {
    let rig = rig!();
    let (_, _, token) = rig.meeting_with_guest(&owner(), true).await;

    let first = rig.join(&token, "Gus").await;
    let second = rig.join(&token, "Someone else").await;
    let forged = rig.join(&format!("{token}x"), "Gus").await;
    let garbage = rig.join("not-a-token", "Gus").await;
    let blank_name = rig.join(&token, "  ").await;

    assert_eq!(first.0, StatusCode::OK);
    assert_eq!(second.0, StatusCode::UNAUTHORIZED, "{}", second.1);
    assert_eq!(forged.0, StatusCode::UNAUTHORIZED);
    assert_eq!(garbage.0, StatusCode::UNAUTHORIZED);
    assert_eq!(blank_name.0, StatusCode::BAD_REQUEST);
    rig.app.stop().await;
  }

  #[tokio::test]
  async fn cancelled_and_finished_meetings_cannot_be_joined() {
    let rig = rig!();
    let alice = owner();
    let (cancelled_id, _, cancelled_token) = rig.meeting_with_guest(&alice, true).await;
    rig.app.delete(&alice, &format!("/meetings/{cancelled_id}")).await;
    let (over_id, _, over_token) = rig.meeting_with_guest(&alice, true).await;
    // Ended long before the two hours of grace.
    let moved = rig
      .app
      .patch(
        &alice,
        &format!("/meetings/{over_id}"),
        json!({"starts_at": 1_000_000_000_000_i64, "ends_at": 1_000_003_600_000_i64}),
      )
      .await;
    assert_eq!(moved.status, StatusCode::OK, "{}", moved.body);

    let cancelled = rig.join(&cancelled_token, "Gus").await;
    let over = rig.join(&over_token, "Gus").await;

    assert_eq!(
      (cancelled.0, over.0),
      (StatusCode::UNAUTHORIZED, StatusCode::UNAUTHORIZED)
    );
    rig.app.stop().await;
  }

  #[tokio::test]
  async fn without_a_lobby_the_guest_is_in_the_chat_at_once() {
    let rig = rig!();
    let (_, chat, token) = rig.meeting_with_guest(&owner(), false).await;

    let (status, joined) = rig.join(&token, "Gus").await;

    assert_eq!(
      (status, joined["lobby_state"].as_str()),
      (StatusCode::OK, Some("admitted"))
    );
    let guest_token = joined["guest_token"].as_str().unwrap();
    let chat_access = rig
      .request(
        Some(guest_token),
        Method::GET,
        &format!("/conversations/{chat}"),
        None,
      )
      .await;
    assert_eq!(chat_access.0, StatusCode::OK);
    rig.app.stop().await;
  }
}

mod lobby {
  use super::*;

  #[tokio::test]
  async fn a_denied_guest_hears_it_and_stays_out() {
    let rig = rig!();
    let app = &rig.app;
    let alice = owner();
    let (meeting, chat, token) = rig.meeting_with_guest(&alice, true).await;
    let (_, joined) = rig.join(&token, "Gus").await;
    let (guest_token, guest_id) = (
      joined["guest_token"].as_str().unwrap().to_owned(),
      joined["user_id"].as_str().unwrap().to_owned(),
    );
    let guest_device = rig.guest_device(&guest_token).await;
    let mut guest_socket = ws::connect_with_token(app, guest_token.clone(), &guest_device).await;
    ws::events(&mut guest_socket, Duration::from_millis(200)).await;

    let denied = app
      .post(
        &alice,
        &format!("/meetings/{meeting}/lobby/{guest_id}/deny"),
        json!({}),
      )
      .await;

    assert_eq!(denied.status, StatusCode::NO_CONTENT, "{}", denied.body);
    let decision = events_of(&ws::events(&mut guest_socket, QUIET).await, "lobby_decision");
    assert_eq!(decision[0]["state"], "denied");
    // The decision reached the guest; their session is over for good (D8), not just their connection.
    for path in [
      format!("/conversations/{chat}"),
      format!("/meetings/{meeting}/lobby/me"),
      "/me".to_owned(),
    ] {
      assert_eq!(
        rig.request(Some(&guest_token), Method::GET, &path, None).await.0,
        StatusCode::FORBIDDEN,
        "{path}"
      );
    }
    let audit = app.get(&admin(), "/admin/audit").await;
    assert!(audit.body.to_string().contains("user_deactivated"));
    let late = app
      .post(
        &alice,
        &format!("/meetings/{meeting}/lobby/{guest_id}/admit"),
        json!({}),
      )
      .await;
    assert_eq!(
      late.status,
      StatusCode::CONFLICT,
      "a decision is final: {}",
      late.body
    );
    rig.app.stop().await;
  }

  #[tokio::test]
  async fn only_hosts_decide_and_hand_out_links() {
    let rig = rig!();
    let app = &rig.app;
    let (alice, bob, eve) = (owner(), member("bob"), member("eve"));
    let bob_id = user_id(app, &bob).await;
    let reply = app
      .post(
        &alice,
        "/meetings",
        json!({"title": "Sync", "starts_at": STARTS_AT, "ends_at": ENDS_AT, "invitees": [{"kind": "user", "user_id": bob_id}]}),
      )
      .await;
    let meeting = reply.id();
    let link = app
      .post(
        &alice,
        &format!("/meetings/{meeting}/guest-links"),
        json!({"label": "Press"}),
      )
      .await;
    assert_eq!(link.status, StatusCode::CREATED, "{}", link.body);
    let join_url = link.body["join_url"].as_str().unwrap();
    assert!(
      join_url.starts_with(&format!("{BASE_URL}/meeting/{meeting}#invite=")),
      "{join_url}"
    );
    let (_, joined) = rig.join(&invite_token(join_url), "Press guest").await;
    let guest_id = joined["user_id"].as_str().unwrap().to_owned();

    // An invited member is no host; a stranger doesn't even know the meeting exists.
    let path = format!("/meetings/{meeting}/lobby/{guest_id}/admit");
    assert_eq!(
      app.post(&bob, &path, json!({})).await.status,
      StatusCode::FORBIDDEN
    );
    assert_eq!(
      app.get(&bob, &format!("/meetings/{meeting}/lobby")).await.status,
      StatusCode::FORBIDDEN
    );
    assert_eq!(
      app
        .post(&bob, &format!("/meetings/{meeting}/guest-links"), json!({}))
        .await
        .status,
      StatusCode::FORBIDDEN
    );
    assert_eq!(
      app.post(&eve, &path, json!({})).await.status,
      StatusCode::NOT_FOUND
    );
    assert_eq!(
      app.post(&alice, &path, json!({})).await.status,
      StatusCode::NO_CONTENT
    );
    rig.app.stop().await;
  }
}

mod sessions {
  use super::*;

  #[tokio::test]
  async fn guests_cannot_schedule_meetings_and_tokens_of_other_keys_are_refused() {
    let rig = rig!();
    let (_, _, token) = rig.meeting_with_guest(&owner(), true).await;
    let (_, joined) = rig.join(&token, "Gus").await;
    let guest_token = joined["guest_token"].as_str().unwrap();

    let schedule = rig
      .request(
        Some(guest_token),
        Method::POST,
        "/meetings",
        Some(json!({"title": "x", "starts_at": STARTS_AT, "ends_at": ENDS_AT})),
      )
      .await;
    let me = rig.request(Some(guest_token), Method::GET, "/me", None).await;
    // The invite token is not a session token.
    let invite_as_session = rig.request(Some(&token), Method::GET, "/me", None).await;

    assert_eq!(schedule.0, StatusCode::FORBIDDEN);
    assert_eq!((me.0, me.1["role"].as_str()), (StatusCode::OK, Some("guest")));
    assert_eq!(me.1["display_name"], "Gus");
    assert_eq!(invite_as_session.0, StatusCode::UNAUTHORIZED);
    rig.app.stop().await;
  }
}
