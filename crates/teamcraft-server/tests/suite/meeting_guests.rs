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
    max_shared_link_guests: 3,
    start_ring_lead_secs: 3600,
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

  /// The meeting's shared guest link, as the host gets it.
  async fn shared_link(&self, host: &TestUser, meeting: &str) -> String {
    let reply = self
      .app
      .put(host, &format!("/meetings/{meeting}/shared-guest-link"), json!({}))
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    reply.body["join_url"].as_str().unwrap().to_owned()
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

  /// A host admits the guest who just joined.
  async fn admit(&self, host: &TestUser, meeting: &str, joined: &Value) {
    let guest = joined["user_id"].as_str().unwrap();
    let admit = self
      .app
      .post(
        host,
        &format!("/meetings/{meeting}/lobby/{guest}/admit"),
        json!({}),
      )
      .await;
    assert_eq!(admit.status, StatusCode::NO_CONTENT, "{}", admit.body);
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
  async fn without_a_lobby_the_guest_still_waits_for_a_hosts_device() {
    let rig = rig!();
    let (_, chat, token) = rig.meeting_with_guest(&owner(), false).await;

    let (status, joined) = rig.join(&token, "Gus").await;

    assert_eq!(
      (status, joined["lobby_state"].as_str()),
      (StatusCode::OK, Some("waiting"))
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
    assert_eq!(chat_access.0, StatusCode::NOT_FOUND);
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

mod removed_invitees {
  use super::*;

  #[tokio::test]
  async fn an_unused_link_of_a_removed_invitee_stops_working() {
    let rig = rig!();
    let alice = owner();
    let (meeting, _, token) = rig.meeting_with_guest(&alice, true).await;

    let reply = rig
      .app
      .patch(&alice, &format!("/meetings/{meeting}"), json!({"invitees": []}))
      .await;

    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let (status, _) = rig.join(&token, "Gus").await;
    assert_eq!(
      status,
      StatusCode::UNAUTHORIZED,
      "the removed invitee's link is dead"
    );
    rig.app.stop().await;
  }

  #[tokio::test]
  async fn a_removed_invitee_who_already_joined_loses_the_session() {
    let rig = rig!();
    let alice = owner();
    let (meeting, chat, token) = rig.meeting_with_guest(&alice, false).await;
    let (_, joined) = rig.join(&token, "Gus").await;
    let guest_token = joined["guest_token"].as_str().unwrap().to_owned();

    let reply = rig
      .app
      .patch(&alice, &format!("/meetings/{meeting}"), json!({"invitees": []}))
      .await;

    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let after = rig
      .request(
        Some(&guest_token),
        Method::GET,
        &format!("/conversations/{chat}"),
        None,
      )
      .await;
    assert_eq!(
      after.0,
      StatusCode::FORBIDDEN,
      "the session ended with the removal"
    );
    rig.app.stop().await;
  }
}

mod shared_link {
  use super::*;

  #[tokio::test]
  async fn one_link_lets_several_guests_into_the_lobby_even_without_one() {
    let rig = rig!();
    let app = &rig.app;
    let alice = owner();
    let (meeting, _, _) = rig.meeting_with_guest(&alice, false).await;

    let first = rig.shared_link(&alice, &meeting).await;
    let again = rig.shared_link(&alice, &meeting).await;
    assert_eq!(first, again, "asking again returns the same link");
    assert!(
      first.starts_with(&format!("{BASE_URL}/meeting/{meeting}#invite=")),
      "{first}"
    );

    let (gus_status, gus) = rig.join(&invite_token(&first), "Gus").await;
    let (ida_status, ida) = rig.join(&invite_token(&first), "Ida").await;

    assert_eq!((gus_status, ida_status), (StatusCode::OK, StatusCode::OK));
    // The meeting has no lobby, but guests of the shared link always wait for a host.
    assert_eq!(
      (gus["lobby_state"].as_str(), ida["lobby_state"].as_str()),
      (Some("waiting"), Some("waiting"))
    );
    assert_ne!(
      gus["user_id"], ida["user_id"],
      "every guest is a person of their own"
    );
    let lobby = app.get(&alice, &format!("/meetings/{meeting}/lobby")).await;
    assert_eq!(lobby.body["entries"].as_array().unwrap().len(), 2);
    rig.app.stop().await;
  }

  #[tokio::test]
  async fn a_revoked_link_admits_nobody_new_and_the_next_one_is_new() {
    let rig = rig!();
    let app = &rig.app;
    let alice = owner();
    let (meeting, _, _) = rig.meeting_with_guest(&alice, true).await;
    let old = rig.shared_link(&alice, &meeting).await;
    let (_, gus) = rig.join(&invite_token(&old), "Gus").await;

    let path = format!("/meetings/{meeting}/shared-guest-link");
    assert_eq!(app.delete(&alice, &path).await.status, StatusCode::NO_CONTENT);
    assert_eq!(
      app.delete(&alice, &path).await.status,
      StatusCode::NO_CONTENT,
      "revoking twice is fine"
    );
    let late = rig.join(&invite_token(&old), "Ida").await;
    let new = rig.shared_link(&alice, &meeting).await;
    let with_new = rig.join(&invite_token(&new), "Ida").await;
    let old_again = rig.join(&invite_token(&old), "Max").await;

    assert_eq!(late.0, StatusCode::UNAUTHORIZED, "{}", late.1);
    assert_ne!(old, new);
    assert_eq!(with_new.0, StatusCode::OK, "{}", with_new.1);
    assert_eq!(old_again.0, StatusCode::UNAUTHORIZED);
    // The guest who got in before the revocation is still waiting for a decision.
    let guest_token = gus["guest_token"].as_str().unwrap();
    let mine = rig
      .request(
        Some(guest_token),
        Method::GET,
        &format!("/meetings/{meeting}/lobby/me"),
        None,
      )
      .await;
    assert_eq!(mine.1["state"], "waiting");
    rig.app.stop().await;
  }

  #[tokio::test]
  async fn the_link_admits_at_most_the_configured_number_of_guests() {
    let rig = rig!();
    let alice = owner();
    let (meeting, _, _) = rig.meeting_with_guest(&alice, true).await;
    let token = invite_token(&rig.shared_link(&alice, &meeting).await);

    // `max_shared_link_guests` is 3 in this rig.
    for name in ["One", "Two", "Three"] {
      assert_eq!(rig.join(&token, name).await.0, StatusCode::OK, "{name}");
    }
    let fourth = rig.join(&token, "Four").await;

    assert_eq!(fourth.0, StatusCode::CONFLICT, "{}", fourth.1);
    rig.app.stop().await;
  }

  #[tokio::test]
  async fn the_link_ends_with_a_cancelled_or_finished_meeting() {
    let rig = rig!();
    let app = &rig.app;
    let alice = owner();
    let (cancelled, _, _) = rig.meeting_with_guest(&alice, true).await;
    let cancelled_link = rig.shared_link(&alice, &cancelled).await;
    app.delete(&alice, &format!("/meetings/{cancelled}")).await;
    let (over, _, _) = rig.meeting_with_guest(&alice, true).await;
    let over_link = rig.shared_link(&alice, &over).await;
    let moved = app
      .patch(
        &alice,
        &format!("/meetings/{over}"),
        json!({"starts_at": 1_000_000_000_000_i64, "ends_at": 1_000_003_600_000_i64}),
      )
      .await;
    assert_eq!(moved.status, StatusCode::OK, "{}", moved.body);

    let cancelled_join = rig.join(&invite_token(&cancelled_link), "Gus").await;
    let over_join = rig.join(&invite_token(&over_link), "Gus").await;
    let new_for_cancelled = app
      .put(
        &alice,
        &format!("/meetings/{cancelled}/shared-guest-link"),
        json!({}),
      )
      .await;

    assert_eq!(
      (cancelled_join.0, over_join.0),
      (StatusCode::UNAUTHORIZED, StatusCode::UNAUTHORIZED)
    );
    assert_eq!(new_for_cancelled.status, StatusCode::CONFLICT);
    rig.app.stop().await;
  }

  #[tokio::test]
  async fn only_hosts_get_and_revoke_the_link() {
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
    let link = rig.shared_link(&alice, &meeting).await;
    let (_, joined) = rig.join(&invite_token(&link), "Gus").await;
    let guest_token = joined["guest_token"].as_str().unwrap().to_owned();
    let path = format!("/meetings/{meeting}/shared-guest-link");

    // An invited member is no host, a stranger and a waiting guest don't see the meeting.
    assert_eq!(
      app.put(&bob, &path, json!({})).await.status,
      StatusCode::FORBIDDEN
    );
    assert_eq!(app.delete(&bob, &path).await.status, StatusCode::FORBIDDEN);
    assert_eq!(
      app.put(&eve, &path, json!({})).await.status,
      StatusCode::NOT_FOUND
    );
    assert_eq!(app.delete(&eve, &path).await.status, StatusCode::NOT_FOUND);
    for method in [Method::PUT, Method::DELETE] {
      let (status, _) = rig
        .request(Some(&guest_token), method.clone(), &path, Some(json!({})))
        .await;
      // A guest session doesn't reach these routes at all.
      assert_eq!(status, StatusCode::FORBIDDEN, "{method}");
    }
    // The link still works: none of that revoked it.
    assert_eq!(rig.join(&invite_token(&link), "Ida").await.0, StatusCode::OK);
    rig.app.stop().await;
  }
}

mod knocking {
  use super::*;

  #[tokio::test]
  async fn a_member_with_the_link_knocks_waits_and_is_admitted() {
    let rig = rig!();
    let app = &rig.app;
    let (alice, eve) = (owner(), member("eve"));
    let alice_device = register_device(app, &alice).await;
    let eve_id = user_id(app, &eve).await;
    let meeting = app.post(&alice, "/meetings/instant", json!({})).await.id();
    let mut alice_socket = ws::connect(app, &alice, &alice_device).await;
    ws::events(&mut alice_socket, Duration::from_millis(300)).await;
    let knock_path = format!("/meetings/{meeting}/lobby/me");
    assert_eq!(
      app.get(&eve, &format!("/meetings/{meeting}")).await.status,
      StatusCode::NOT_FOUND
    );

    let knock = app.post(&eve, &knock_path, json!({})).await;
    let again = app.post(&eve, &knock_path, json!({})).await;

    assert_eq!(knock.status, StatusCode::OK, "{}", knock.body);
    assert_eq!(
      (knock.body["state"].as_str(), again.body["state"].as_str()),
      (Some("waiting"), Some("waiting"))
    );
    let heard = events_of(&ws::events(&mut alice_socket, QUIET).await, "lobby_join");
    assert_eq!(
      heard.len(),
      1,
      "knocking again while waiting is no news: {heard:?}"
    );
    assert_eq!(heard[0]["user"]["id"], eve_id.as_str());
    let lobby = app.get(&alice, &format!("/meetings/{meeting}/lobby")).await;
    assert_eq!(lobby.body["entries"][0]["user"]["id"], eve_id.as_str());

    let admit = app
      .post(
        &alice,
        &format!("/meetings/{meeting}/lobby/{eve_id}/admit"),
        json!({}),
      )
      .await;
    assert_eq!(admit.status, StatusCode::NO_CONTENT, "{}", admit.body);
    assert_eq!(
      app.get(&eve, &format!("/meetings/{meeting}")).await.status,
      StatusCode::OK
    );
    assert_eq!(
      app.post(&eve, &knock_path, json!({})).await.body["state"],
      "admitted"
    );
    rig.app.stop().await;
  }

  #[tokio::test]
  async fn a_member_who_left_after_being_admitted_waits_again() {
    let rig = rig!();
    let app = &rig.app;
    let (alice, eve) = (owner(), member("eve"));
    let alice_device = register_device(app, &alice).await;
    let eve_id = user_id(app, &eve).await;
    let created = app.post(&alice, "/meetings/instant", json!({})).await;
    let (meeting, chat) = (
      created.id(),
      created.body["conversation_id"].as_str().unwrap().to_owned(),
    );
    let knock_path = format!("/meetings/{meeting}/lobby/me");
    app.post(&eve, &knock_path, json!({})).await;
    app
      .post(
        &alice,
        &format!("/meetings/{meeting}/lobby/{eve_id}/admit"),
        json!({}),
      )
      .await;
    // Meeting chats have no "leave" yet; the membership ends as it would through an admin.
    let left = app
      .state
      .database
      .remove_conversation_member(chat.parse().unwrap(), eve_id.parse().unwrap())
      .await
      .unwrap();
    assert!(left);
    let mut alice_socket = ws::connect(app, &alice, &alice_device).await;
    ws::events(&mut alice_socket, Duration::from_millis(300)).await;

    let knock = app.post(&eve, &knock_path, json!({})).await;

    assert_eq!(knock.body["state"], "waiting", "{}", knock.body);
    let heard = events_of(&ws::events(&mut alice_socket, QUIET).await, "lobby_join");
    assert_eq!(heard.len(), 1);
    rig.app.stop().await;
  }

  #[tokio::test]
  async fn a_denied_member_keeps_their_account_and_stays_out() {
    let rig = rig!();
    let app = &rig.app;
    let (alice, eve) = (owner(), member("eve"));
    let eve_id = user_id(app, &eve).await;
    let meeting = app.post(&alice, "/meetings/instant", json!({})).await.id();
    let knock_path = format!("/meetings/{meeting}/lobby/me");
    app.post(&eve, &knock_path, json!({})).await;

    let denied = app
      .post(
        &alice,
        &format!("/meetings/{meeting}/lobby/{eve_id}/deny"),
        json!({}),
      )
      .await;

    assert_eq!(denied.status, StatusCode::NO_CONTENT, "{}", denied.body);
    assert_eq!(app.get(&eve, "/me").await.status, StatusCode::OK);
    assert_eq!(
      app.post(&eve, &knock_path, json!({})).await.body["state"],
      "denied",
      "a decision is final"
    );
    assert_eq!(
      app.get(&eve, &format!("/meetings/{meeting}")).await.status,
      StatusCode::NOT_FOUND
    );
    rig.app.stop().await;
  }

  #[tokio::test]
  async fn invited_members_are_in_and_guests_and_ended_meetings_are_refused() {
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
    let knock_path = format!("/meetings/{meeting}/lobby/me");
    let (_, joined) = rig
      .join(&invite_token(&rig.shared_link(&alice, &meeting).await), "Gus")
      .await;
    let guest_token = joined["guest_token"].as_str().unwrap().to_owned();

    let invited = app.post(&bob, &knock_path, json!({})).await;
    let guest = rig
      .request(Some(&guest_token), Method::POST, &knock_path, None)
      .await;
    let unknown = app.post(&eve, "/meetings/1234567/lobby/me", json!({})).await;
    app.delete(&alice, &format!("/meetings/{meeting}")).await;
    let cancelled_outsider = app.post(&eve, &knock_path, json!({})).await;
    let cancelled_member = app.post(&bob, &knock_path, json!({})).await;

    assert_eq!(invited.body["state"], "admitted", "{}", invited.body);
    assert_eq!(guest.0, StatusCode::FORBIDDEN);
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    // An outsider can't tell a cancelled meeting from none; a member learns that it is over.
    assert_eq!(cancelled_outsider.status, StatusCode::NOT_FOUND);
    assert_eq!(cancelled_member.status, StatusCode::CONFLICT);
    rig.app.stop().await;
  }
}

mod scope_and_leaving {
  use super::*;

  #[tokio::test]
  async fn an_admitted_guest_reaches_only_the_routes_a_guest_needs() {
    let rig = rig!();
    let alice = owner();
    let (meeting, chat, token) = rig.meeting_with_guest(&alice, false).await;
    let (_, joined) = rig.join(&token, "Gus").await;
    rig.admit(&alice, &meeting, &joined).await;
    let guest_token = joined["guest_token"].as_str().unwrap().to_owned();

    for (method, path, body) in [
      (Method::GET, "/users?query=a".to_owned(), None),
      (Method::POST, "/dms".to_owned(), Some(json!({"user_id": "1"}))),
      (Method::POST, "/meetings/instant".to_owned(), Some(json!({}))),
      (Method::GET, "/meetings".to_owned(), None),
      (Method::GET, "/admin/users".to_owned(), None),
      (
        Method::POST,
        format!("/meetings/{meeting}/guest-links"),
        Some(json!({})),
      ),
      (Method::PATCH, format!("/conversations/{chat}"), Some(json!({}))),
    ] {
      let (status, body) = rig.request(Some(&guest_token), method.clone(), &path, body).await;
      assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}: {body}");
    }
    let (status, _) = rig
      .request(
        Some(&guest_token),
        Method::GET,
        &format!("/conversations/{chat}"),
        None,
      )
      .await;
    assert_eq!(status, StatusCode::OK);
    rig.app.stop().await;
  }

  #[tokio::test]
  async fn a_guest_who_leaves_loses_the_session_and_the_chat() {
    let rig = rig!();
    let app = &rig.app;
    let alice = owner();
    let alice_device = register_device(app, &alice).await;
    let (meeting, chat, token) = rig.meeting_with_guest(&alice, false).await;
    let (_, joined) = rig.join(&token, "Gus").await;
    rig.admit(&alice, &meeting, &joined).await;
    let guest_token = joined["guest_token"].as_str().unwrap().to_owned();
    let guest_id = joined["user_id"].as_str().unwrap().to_owned();
    let mut alice_socket = ws::connect(app, &alice, &alice_device).await;
    ws::events(&mut alice_socket, Duration::from_millis(300)).await;

    let (status, body) = rig
      .request(
        Some(&guest_token),
        Method::POST,
        &format!("/meetings/{meeting}/leave"),
        Some(json!({})),
      )
      .await;

    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let removed = events_of(&ws::events(&mut alice_socket, QUIET).await, "membership_remove");
    assert_eq!(removed[0]["user_id"], guest_id.as_str());
    let (status, _) = rig
      .request(
        Some(&guest_token),
        Method::GET,
        &format!("/conversations/{chat}"),
        None,
      )
      .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "the session is over");
    let members = app.get(&alice, &format!("/conversations/{chat}/members")).await;
    assert!(!members.body.to_string().contains(&guest_id));
    rig.app.stop().await;
  }

  #[tokio::test]
  async fn a_waiting_guest_who_leaves_cannot_be_admitted_and_members_cannot_use_leave() {
    let rig = rig!();
    let app = &rig.app;
    let alice = owner();
    let (meeting, _, token) = rig.meeting_with_guest(&alice, true).await;
    let (_, joined) = rig.join(&token, "Gus").await;
    let guest_token = joined["guest_token"].as_str().unwrap().to_owned();
    let guest_id = joined["user_id"].as_str().unwrap().to_owned();

    let member = app
      .post(&alice, &format!("/meetings/{meeting}/leave"), json!({}))
      .await;
    assert_eq!(member.status, StatusCode::FORBIDDEN, "{}", member.body);

    let (status, _) = rig
      .request(
        Some(&guest_token),
        Method::POST,
        &format!("/meetings/{meeting}/leave"),
        Some(json!({})),
      )
      .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let lobby = app.get(&alice, &format!("/meetings/{meeting}/lobby")).await;
    assert!(lobby.body["entries"].as_array().unwrap().is_empty());
    let admit = app
      .post(
        &alice,
        &format!("/meetings/{meeting}/lobby/{guest_id}/admit"),
        json!({}),
      )
      .await;
    assert_eq!(admit.status, StatusCode::CONFLICT, "{}", admit.body);
    rig.app.stop().await;
  }

  #[tokio::test]
  async fn the_link_proof_is_set_once_and_only_hosts_read_it() {
    let rig = rig!();
    let app = &rig.app;
    let alice = owner();
    let (meeting, _, token) = rig.meeting_with_guest(&alice, true).await;
    let (_, joined) = rig.join(&token, "Gus").await;
    let guest_token = joined["guest_token"].as_str().unwrap().to_owned();
    let guest_id = joined["user_id"].as_str().unwrap().to_owned();
    let proof_path = format!("/meetings/{meeting}/lobby/me/proof");

    let put = |proof: &'static str| {
      let (rig, token, path) = (&rig, guest_token.clone(), proof_path.clone());
      async move {
        rig
          .request(Some(&token), Method::PUT, &path, Some(json!({"proof": proof})))
          .await
          .0
      }
    };
    assert_eq!(put("cHJvb2Y=").await, StatusCode::NO_CONTENT);
    assert_eq!(put("b3RoZXI=").await, StatusCode::CONFLICT, "set once");
    let member = app.put(&alice, &proof_path, json!({"proof": "cHJvb2Y="})).await;
    assert_eq!(member.status, StatusCode::FORBIDDEN);

    let entry = app
      .get(&alice, &format!("/meetings/{meeting}/lobby/{guest_id}"))
      .await;
    assert_eq!(entry.status, StatusCode::OK, "{}", entry.body);
    assert_eq!(entry.body["proof"], "cHJvb2Y=");
    let lobby = app.get(&alice, &format!("/meetings/{meeting}/lobby")).await;
    assert_eq!(lobby.body["entries"][0]["proof"], "cHJvb2Y=");
    let (status, _) = rig
      .request(
        Some(&guest_token),
        Method::GET,
        &format!("/meetings/{meeting}/lobby/{guest_id}"),
        None,
      )
      .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "guests don't read lobby entries");
    rig.app.stop().await;
  }

  #[tokio::test]
  async fn guests_see_no_addresses_and_cannot_reset_their_identity_key() {
    let rig = rig!();
    let app = &rig.app;
    let alice = owner();
    let (meeting, _, token) = rig.meeting_with_guest(&alice, false).await;
    let (_, joined) = rig.join(&token, "Gus").await;
    rig.admit(&alice, &meeting, &joined).await;
    let guest_token = joined["guest_token"].as_str().unwrap().to_owned();

    let (status, body) = rig
      .request(
        Some(&guest_token),
        Method::GET,
        &format!("/meetings/{meeting}"),
        None,
      )
      .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.to_string().contains("gus@example.org"), "{body}");
    let host_view = app.get(&alice, &format!("/meetings/{meeting}")).await;
    assert!(host_view.body.to_string().contains("gus@example.org"));

    let (status, body) = rig
      .request(
        Some(&guest_token),
        Method::PUT,
        "/me/identity-key",
        Some(json!({"public_key": "cHVi", "signature": "c2ln", "replace": true})),
      )
      .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    rig.app.stop().await;
  }

  #[tokio::test]
  async fn guests_of_a_finished_meeting_are_ended_by_the_sweep() {
    let rig = rig!();
    let app = &rig.app;
    let alice = owner();
    let (meeting, chat, token) = rig.meeting_with_guest(&alice, false).await;
    let (_, joined) = rig.join(&token, "Gus").await;
    let guest_id = joined["user_id"].as_str().unwrap().to_owned();
    let grace = time::Duration::seconds(7200);
    let ends = time::OffsetDateTime::from_unix_timestamp(ENDS_AT / 1000).unwrap();

    use teamcraft_server::services::meeting_guests::sweep_expired_guests_at;
    assert_eq!(
      sweep_expired_guests_at(&app.state, ends).await.unwrap(),
      0,
      "still in the meeting"
    );
    assert_eq!(
      sweep_expired_guests_at(&app.state, ends + grace - time::Duration::seconds(1))
        .await
        .unwrap(),
      0,
      "still in the grace time"
    );
    assert_eq!(
      sweep_expired_guests_at(&app.state, ends + grace + time::Duration::seconds(1))
        .await
        .unwrap(),
      1
    );
    assert_eq!(
      sweep_expired_guests_at(&app.state, ends + grace + time::Duration::seconds(1))
        .await
        .unwrap(),
      0
    );
    let members = app.get(&alice, &format!("/conversations/{chat}/members")).await;
    assert!(!members.body.to_string().contains(&guest_id));
    let _ = meeting;
    rig.app.stop().await;
  }
}
