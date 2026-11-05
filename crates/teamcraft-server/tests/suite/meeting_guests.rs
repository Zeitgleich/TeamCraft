//! Meeting guests: the meeting link, personal one-time links, guest sessions, the lobby and the
//! meeting chat against a real server and Postgres (`--features integration`, needs
//! `DATABASE_URL`).
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use teamcraft_crypto::meeting_link::is_meeting_link_token;
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
  /// Schedules a meeting with one external invitee and returns (meeting id, chat id, the link's
  /// token from the invitation mail).
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
      credential(&mail.text),
    )
  }

  /// A personal one-time link the host makes by hand.
  async fn one_time_link(&self, host: &TestUser, meeting: &str) -> String {
    let reply = self
      .app
      .post(host, &format!("/meetings/{meeting}/guest-links"), json!({}))
      .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    reply.body["join_url"].as_str().unwrap().to_owned()
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

  /// Joins with a meeting link's token, or a personal link's one-time token.
  async fn join(&self, token: &str, name: &str) -> (StatusCode, Value) {
    let body = if is_meeting_link_token(token) {
      json!({"link_token": token, "display_name": name})
    } else {
      json!({"invite_token": token, "display_name": name})
    };
    self
      .request(None, Method::POST, "/meetings/join", Some(body))
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

/// Publishes a 32-byte identity key for `user`, so their meetings' links carry its fingerprint.
async fn put_identity_key(app: &TestApp, user: &TestUser) {
  let key = format!("{}=", "A".repeat(43));
  let reply = app
    .put(
      user,
      "/me/identity-key",
      json!({"public_key": key, "signature": "c2ln", "key_bag": {"encrypted_key_bag": "YmFn", "wrapped_backup_keys": "d3JhcA=="}}),
    )
    .await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
}

/// What a guest's page sends from the first link in `text`: a personal link's `#invite=` token,
/// else the meeting link's token from its path.
fn credential(text: &str) -> String {
  if let Some(rest) = text.split("#invite=").nth(1) {
    return rest
      .split(|c: char| c == '&' || c.is_whitespace())
      .next()
      .unwrap()
      .to_owned();
  }
  link_token(text)
}

/// The token in the path of the first meeting link in `text`.
fn link_token(text: &str) -> String {
  text
    .split("/meeting/")
    .nth(1)
    .expect("a meeting link")
    .split(|c: char| c == '#' || c.is_whitespace())
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
  async fn a_one_time_link_works_once_and_only_with_our_signature() {
    let rig = rig!();
    let alice = owner();
    let (meeting, _, _) = rig.meeting_with_guest(&alice, true).await;
    let token = credential(&rig.one_time_link(&alice, &meeting).await);

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
    // On the server; the organizer's device lets a guest of the link in by itself (e2ee.md §4.6).
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
    // The meeting link's path, the one-time token and the organizer's fingerprint in the fragment.
    let token = link_token(join_url);
    assert!(is_meeting_link_token(&token), "{join_url}");
    assert!(join_url.contains("#invite="), "{join_url}");
    assert!(!join_url.contains(&format!("/meeting/{meeting}")), "{join_url}");
    let (_, joined) = rig.join(&credential(join_url), "Press guest").await;
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

mod meeting_link {
  use super::*;

  #[tokio::test]
  async fn the_mail_calendar_and_api_carry_the_same_unguessable_link() {
    let rig = rig!();
    let app = &rig.app;
    let alice = owner();
    // The organizer's identity key gives the link its fingerprint.
    put_identity_key(app, &alice).await;
    let (meeting, _, token) = rig.meeting_with_guest(&alice, true).await;

    let mail = rig
      .mailer
      .sent()
      .into_iter()
      .find(|m| m.to_address == "gus@example.org")
      .unwrap();
    let api = app.get(&alice, &format!("/meetings/{meeting}")).await;
    let join_url = api.body["join_url"].as_str().unwrap();

    assert!(is_meeting_link_token(&token), "{token}");
    assert!(
      join_url.starts_with(&format!("{BASE_URL}/meeting/{token}#v=")),
      "{join_url}"
    );
    let fingerprint = join_url.split("#v=").nth(1).unwrap();
    assert_eq!(fingerprint.len(), 64, "{fingerprint}");
    assert!(fingerprint.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(mail.text.contains(join_url), "{}", mail.text);
    assert!(
      mail.ics.contains(&format!("URL:{BASE_URL}/meeting/{token}")),
      "{}",
      mail.ics
    );
    assert!(!join_url.contains(&meeting), "the snowflake is no public handle");
    // A guest of a link without `v` (mailed before #987) gets the same fingerprint at the join.
    let (_, joined) = rig.join(&token, "Gus").await;
    assert_eq!(joined["organizer_fingerprint"], fingerprint);
    rig.app.stop().await;
  }

  #[tokio::test]
  async fn a_new_link_stops_the_old_one_and_goes_out_to_the_invitees() {
    let rig = rig!();
    let app = &rig.app;
    let alice = owner();
    let (meeting, _, old) = rig.meeting_with_guest(&alice, true).await;
    let (_, gus) = rig.join(&old, "Gus").await;
    let mails_before = rig.mailer.sent().len();

    let renewed = app
      .post(&alice, &format!("/meetings/{meeting}/link"), json!({}))
      .await;

    assert_eq!(renewed.status, StatusCode::OK, "{}", renewed.body);
    let new = link_token(renewed.body["join_url"].as_str().unwrap());
    assert_ne!(old, new);
    assert!(is_meeting_link_token(&new));
    assert_eq!(rig.join(&old, "Ida").await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(rig.join(&new, "Ida").await.0, StatusCode::OK);
    let update = rig
      .mailer
      .sent()
      .into_iter()
      .skip(mails_before)
      .find(|m| m.to_address == "gus@example.org");
    assert!(
      update.is_some_and(|mail| mail.text.contains(&new)),
      "the invitee gets the new link"
    );
    // The guest who got in before stays where they were.
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
    // An old personal link of the meeting stops too (#987 review, M5).
    let one_time = credential(&rig.one_time_link(&alice, &meeting).await);
    app
      .post(&alice, &format!("/meetings/{meeting}/link"), json!({}))
      .await;
    assert_eq!(rig.join(&one_time, "Max").await.0, StatusCode::UNAUTHORIZED);
    // A member with the old link knocks at nothing.
    let knock = app
      .post(&member("eve"), &format!("/meetings/link/{old}/knock"), json!({}))
      .await;
    assert_eq!(knock.status, StatusCode::NOT_FOUND);
    // Only the organizer makes the link anew.
    let bob = member("bob");
    assert_eq!(
      app
        .post(&bob, &format!("/meetings/{meeting}/link"), json!({}))
        .await
        .status,
      StatusCode::NOT_FOUND
    );
    rig.app.stop().await;
  }

  #[tokio::test]
  async fn a_new_link_makes_guests_still_waiting_wait_for_a_click() {
    let rig = rig!();
    let app = &rig.app;
    let alice = owner();
    let (meeting, _, token) = rig.meeting_with_guest(&alice, false).await;
    let (_, gus) = rig.join(&token, "Gus").await;

    app
      .post(&alice, &format!("/meetings/{meeting}/link"), json!({}))
      .await;

    let lobby = app.get(&alice, &format!("/meetings/{meeting}/lobby")).await;
    let entry = &lobby.body["entries"][0];
    assert_eq!(entry["user"]["id"], gus["user_id"]);
    assert_eq!(
      entry["is_by_link"], false,
      "no admission by itself with the old link"
    );
    rig.app.stop().await;
  }

  #[tokio::test]
  async fn a_guest_hears_when_the_organizers_app_is_too_old_to_let_them_in() {
    let rig = rig!();
    let app = &rig.app;
    let alice = owner();
    let (meeting, _, token) = rig.meeting_with_guest(&alice, true).await;
    let (_, gus) = rig.join(&token, "Gus").await;
    let guest_token = gus["guest_token"].as_str().unwrap();
    let path = format!("/meetings/{meeting}/lobby/me");

    // No app of Alice's was seen letting guests in without a link secret: an older client.
    let before = rig.request(Some(guest_token), Method::GET, &path, None).await;
    assert_eq!(before.1["is_organizer_app_outdated"], true, "{}", before.1);
    let marked = app.put(&alice, "/meetings/admitter", json!({})).await;
    assert_eq!(marked.status, StatusCode::NO_CONTENT, "{}", marked.body);
    let after = rig.request(Some(guest_token), Method::GET, &path, None).await;
    assert_eq!(after.1["is_organizer_app_outdated"], Value::Null, "{}", after.1);
    assert_eq!(after.1["state"], "waiting");
    rig.app.stop().await;
  }

  #[tokio::test]
  async fn guests_of_the_link_are_marked_and_old_links_are_not() {
    let rig = rig!();
    let app = &rig.app;
    let alice = owner();
    let (meeting, _, token) = rig.meeting_with_guest(&alice, false).await;
    let one_time = credential(&rig.one_time_link(&alice, &meeting).await);

    let (_, by_link) = rig.join(&token, "Gus").await;
    let (_, by_invite) = rig.join(&one_time, "Ida").await;

    assert_eq!(by_link["lobby_state"], "waiting");
    let lobby = app.get(&alice, &format!("/meetings/{meeting}/lobby")).await;
    let entries = lobby.body["entries"].as_array().unwrap();
    let marked = |user: &Value| {
      entries
        .iter()
        .find(|entry| entry["user"]["id"] == *user)
        .map(|entry| (entry["is_by_link"].as_bool(), entry["has_device"].as_bool()))
    };
    assert_eq!(marked(&by_link["user_id"]), Some((Some(true), Some(false))));
    assert_eq!(marked(&by_invite["user_id"]), Some((Some(false), Some(false))));
    rig.app.stop().await;
  }

  #[tokio::test]
  async fn malformed_and_unknown_tokens_open_nothing() {
    let rig = rig!();
    let app = &rig.app;
    let alice = owner();
    rig.meeting_with_guest(&alice, true).await;
    let unknown = "A".repeat(32);

    for token in [unknown.as_str(), "too-short", "1234567890"] {
      let body = json!({"link_token": token, "display_name": "Gus"});
      let (status, _) = rig
        .request(None, Method::POST, "/meetings/join", Some(body))
        .await;
      assert_eq!(status, StatusCode::UNAUTHORIZED, "{token}");
      let knock = app
        .post(
          &member("eve"),
          &format!("/meetings/link/{token}/knock"),
          json!({}),
        )
        .await;
      assert_eq!(knock.status, StatusCode::NOT_FOUND, "{token}");
    }
    let (status, _) = rig
      .request(
        None,
        Method::POST,
        "/meetings/join",
        Some(json!({"display_name": "Gus"})),
      )
      .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
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
    // Since #987 the shared guest link is the meeting link.
    let api = app.get(&alice, &format!("/meetings/{meeting}")).await;
    assert_eq!(first, api.body["join_url"].as_str().unwrap());

    let (gus_status, gus) = rig.join(&credential(&first), "Gus").await;
    let (ida_status, ida) = rig.join(&credential(&first), "Ida").await;

    assert_eq!((gus_status, ida_status), (StatusCode::OK, StatusCode::OK));
    // The meeting has no lobby, but guests wait on the server until an organizer's device adds them.
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
    let (_, gus) = rig.join(&credential(&old), "Gus").await;

    let path = format!("/meetings/{meeting}/shared-guest-link");
    assert_eq!(app.delete(&alice, &path).await.status, StatusCode::NO_CONTENT);
    assert_eq!(
      app.delete(&alice, &path).await.status,
      StatusCode::NO_CONTENT,
      "revoking twice is fine"
    );
    let late = rig.join(&credential(&old), "Ida").await;
    let new = rig.shared_link(&alice, &meeting).await;
    let with_new = rig.join(&credential(&new), "Ida").await;
    let old_again = rig.join(&credential(&old), "Max").await;

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
    let token = credential(&rig.shared_link(&alice, &meeting).await);

    // `max_shared_link_guests` is 3 in this rig: three may wait, and three may be let in.
    let mut joined = Vec::new();
    for name in ["One", "Two", "Three"] {
      let (status, body) = rig.join(&token, name).await;
      assert_eq!(status, StatusCode::OK, "{name}");
      joined.push(body);
    }
    let fourth = rig.join(&token, "Four").await;
    assert_eq!(fourth.0, StatusCode::CONFLICT, "the lobby is full: {}", fourth.1);

    // A place counts once its guest is let in; turning a guest away frees theirs (#987 review, M3).
    rig.admit(&alice, &meeting, &joined[0]).await;
    rig.admit(&alice, &meeting, &joined[1]).await;
    let third = joined[2]["user_id"].as_str().unwrap();
    let deny = rig
      .app
      .post(
        &alice,
        &format!("/meetings/{meeting}/lobby/{third}/deny"),
        json!({}),
      )
      .await;
    assert_eq!(deny.status, StatusCode::NO_CONTENT, "{}", deny.body);
    let (status, late) = rig.join(&token, "Four").await;
    assert_eq!(status, StatusCode::OK, "{late}");
    rig.admit(&alice, &meeting, &late).await;
    let fifth = rig.join(&token, "Five").await;
    assert_eq!(fifth.0, StatusCode::CONFLICT, "three are in: {}", fifth.1);
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

    let cancelled_join = rig.join(&credential(&cancelled_link), "Gus").await;
    let over_join = rig.join(&credential(&over_link), "Gus").await;
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
    let (_, joined) = rig.join(&credential(&link), "Gus").await;
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
    assert_eq!(rig.join(&credential(&link), "Ida").await.0, StatusCode::OK);
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
  async fn a_member_with_the_new_link_knocks_by_its_token_and_an_old_link_still_works() {
    let rig = rig!();
    let app = &rig.app;
    let (alice, eve) = (owner(), member("eve"));
    let eve_id = user_id(app, &eve).await;
    let reply = app.post(&alice, "/meetings/instant", json!({})).await;
    let meeting = reply.id();
    let token = link_token(reply.body["join_url"].as_str().unwrap());

    let knock = app
      .post(&eve, &format!("/meetings/link/{token}/knock"), json!({}))
      .await;

    assert_eq!(knock.status, StatusCode::OK, "{}", knock.body);
    assert_eq!(
      (knock.body["meeting_id"].as_str(), knock.body["state"].as_str()),
      (Some(meeting.as_str()), Some("waiting"))
    );
    let lobby = app.get(&alice, &format!("/meetings/{meeting}/lobby")).await;
    assert_eq!(lobby.body["entries"][0]["user"]["id"], eve_id.as_str());
    assert_eq!(lobby.body["entries"][0]["is_by_link"], true);
    // The organizer is in already; the old link with the ID goes through the lobby, unmarked.
    let mine = app
      .post(&alice, &format!("/meetings/link/{token}/knock"), json!({}))
      .await;
    assert_eq!(mine.body["state"], "admitted");
    let carol = member("carol");
    let old = app
      .post(&carol, &format!("/meetings/{meeting}/lobby/me"), json!({}))
      .await;
    assert_eq!(old.body["state"], "waiting");
    let carol_id = user_id(app, &carol).await;
    let lobby = app.get(&alice, &format!("/meetings/{meeting}/lobby")).await;
    let carol_entry = lobby.body["entries"]
      .as_array()
      .unwrap()
      .iter()
      .find(|entry| entry["user"]["id"] == carol_id.as_str())
      .cloned()
      .unwrap();
    assert_eq!(carol_entry["is_by_link"], false);
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
      .join(&credential(&rig.shared_link(&alice, &meeting).await), "Gus")
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
  async fn a_guests_device_tells_the_hosts_once_with_its_key_and_only_hosts_read_the_entry() {
    let rig = rig!();
    let app = &rig.app;
    let alice = owner();
    let alice_device = register_device(app, &alice).await;
    let (meeting, _, token) = rig.meeting_with_guest(&alice, true).await;
    let (_, joined) = rig.join(&token, "Gus").await;
    let guest_token = joined["guest_token"].as_str().unwrap().to_owned();
    let guest_id = joined["user_id"].as_str().unwrap().to_owned();
    let device_path = format!("/meetings/{meeting}/lobby/me/device");
    let mut alice_socket = ws::connect(app, &alice, &alice_device).await;
    ws::events(&mut alice_socket, Duration::from_millis(300)).await;

    let fingerprint = "ab".repeat(32);
    let report = |fingerprint: String| {
      let (rig, token, path) = (&rig, guest_token.clone(), device_path.clone());
      async move {
        rig
          .request(
            Some(&token),
            Method::PUT,
            &path,
            Some(json!({"fingerprint": fingerprint})),
          )
          .await
          .0
      }
    };
    assert_eq!(
      report(fingerprint.clone()).await,
      StatusCode::CONFLICT,
      "no device yet"
    );
    rig.guest_device(&guest_token).await;
    assert_eq!(report("not hex".to_owned()).await, StatusCode::BAD_REQUEST);
    assert_eq!(report(fingerprint.clone()).await, StatusCode::NO_CONTENT);
    // Reporting again tells nobody and changes nothing (#987 review, L3).
    assert_eq!(report("cd".repeat(32)).await, StatusCode::NO_CONTENT);
    let member_put = app
      .put(&alice, &device_path, json!({"fingerprint": fingerprint}))
      .await;
    assert_eq!(member_put.status, StatusCode::FORBIDDEN);

    let heard = events_of(&ws::events(&mut alice_socket, QUIET).await, "lobby_join");
    assert_eq!(heard.len(), 1, "{heard:?}");
    assert_eq!(heard[0]["user"]["id"], guest_id.as_str());
    let entry = app
      .get(&alice, &format!("/meetings/{meeting}/lobby/{guest_id}"))
      .await;
    assert_eq!(entry.status, StatusCode::OK, "{}", entry.body);
    assert_eq!(
      (
        entry.body["is_by_link"].as_bool(),
        entry.body["has_device"].as_bool()
      ),
      (Some(true), Some(true))
    );
    assert_eq!(entry.body["knock_fingerprint"], fingerprint.as_str());
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

mod admitters {
  use base64::Engine as _;
  use base64::engine::general_purpose::STANDARD;
  use rand_core::OsRng;
  use teamcraft_crypto::identity::IdentityKeyPair;
  use teamcraft_crypto::ids::{ConversationId, MeetingId};
  use teamcraft_crypto::meeting_admitters::{AdmittersDraft, LinkAdmission};

  use super::*;

  #[tokio::test]
  async fn the_organizer_publishes_admitters_that_members_and_admitted_guests_read() {
    let rig = rig!();
    let app = &rig.app;
    let (alice, bob, eve) = (owner(), member("bob"), member("eve"));
    let (meeting, chat) = rig.meeting_with(&alice, &bob, false).await;
    let path = format!("/conversations/{chat}/meeting-admitters");
    let organizer = IdentityKeyPair::generate(&mut OsRng);
    let missing = app.get(&alice, &path).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.body["error_code"], "meeting_admitters_not_found");

    let first = statement(&organizer, &meeting, &chat, 5);
    assert_eq!(
      app.put(&alice, &path, json!({"statement": first})).await.status,
      StatusCode::NO_CONTENT
    );

    // Members of the chat read it as it was published; strangers don't learn the meeting exists.
    let read = app.get(&bob, &path).await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.body);
    assert_eq!(read.body["statement"], first.as_str());
    assert_eq!(app.get(&eve, &path).await.status, StatusCode::NOT_FOUND);
    // Only the organizer publishes.
    assert_eq!(
      app.put(&bob, &path, json!({"statement": first})).await.status,
      StatusCode::FORBIDDEN
    );
    assert_eq!(
      app.put(&eve, &path, json!({"statement": first})).await.status,
      StatusCode::NOT_FOUND
    );

    // A guest reads it once let in, never while waiting, and never publishes.
    let token = rig.link_of(&alice, &meeting).await;
    let (_, joined) = rig.join(&token, "Gus").await;
    let guest_token = joined["guest_token"].as_str().unwrap().to_owned();
    let as_guest = |method: Method, body: Option<Value>| {
      let (rig, token, path) = (&rig, guest_token.clone(), path.clone());
      async move { rig.request(Some(&token), method, &path, body).await }
    };
    assert_eq!(as_guest(Method::GET, None).await.0, StatusCode::NOT_FOUND);
    rig.admit(&alice, &meeting, &joined).await;
    let (status, body) = as_guest(Method::GET, None).await;
    assert_eq!(
      (status, body["statement"].as_str()),
      (StatusCode::OK, Some(first.as_str()))
    );
    assert_eq!(
      as_guest(Method::PUT, Some(json!({"statement": first}))).await.0,
      StatusCode::FORBIDDEN
    );
    rig.app.stop().await;
  }

  #[tokio::test]
  async fn the_server_keeps_the_newest_and_refuses_malformed_admitters() {
    let rig = rig!();
    let app = &rig.app;
    let (alice, bob) = (owner(), member("bob"));
    let (meeting, chat) = rig.meeting_with(&alice, &bob, false).await;
    let path = format!("/conversations/{chat}/meeting-admitters");
    let organizer = IdentityKeyPair::generate(&mut OsRng);
    let put = |statement: String| {
      let (app, alice, path) = (&rig.app, alice.clone(), path.clone());
      async move {
        app
          .put(&alice, &path, json!({"statement": statement}))
          .await
          .status
      }
    };
    let newest = statement(&organizer, &meeting, &chat, 6);
    assert_eq!(put(newest.clone()).await, StatusCode::NO_CONTENT);

    // No rollback on the server: the same or an older sequence is refused, and the newest stays.
    for sequence in [6, 5] {
      assert_eq!(
        put(statement(&organizer, &meeting, &chat, sequence)).await,
        StatusCode::CONFLICT,
        "{sequence}"
      );
    }
    assert_eq!(app.get(&bob, &path).await.body["statement"], newest.as_str());

    // Another meeting's or chat's list, a broken signature, garbage, and a sequence the database
    // can't hold.
    let mut tampered = STANDARD
      .decode(statement(&organizer, &meeting, &chat, 9))
      .unwrap();
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    for (bad, why) in [
      (statement(&organizer, "1", &chat, 9), "another meeting"),
      (statement(&organizer, &meeting, "1", 9), "another chat"),
      (STANDARD.encode(tampered), "a broken signature"),
      (STANDARD.encode([1, 2, 3]), "garbage"),
      (
        statement(&organizer, &meeting, &chat, u64::MAX),
        "a sequence past i64",
      ),
    ] {
      assert_eq!(put(bad).await, StatusCode::BAD_REQUEST, "{why}");
    }
    assert_eq!(app.get(&bob, &path).await.body["statement"], newest.as_str());
    rig.app.stop().await;
  }

  #[tokio::test]
  async fn without_a_lobby_members_let_in_people_of_the_link_and_hear_them_knock() {
    let rig = rig!();
    let app = &rig.app;
    let (alice, bob) = (owner(), member("bob"));
    let bob_device = register_device(app, &bob).await;
    let (meeting, _) = rig.meeting_with(&alice, &bob, false).await;
    let mut bob_socket = ws::connect(app, &bob, &bob_device).await;
    ws::events(&mut bob_socket, Duration::from_millis(300)).await;
    let token = rig.link_of(&alice, &meeting).await;
    let (_, by_link) = rig.join(&token, "Gus").await;
    let one_time = rig.one_time_link(&alice, &meeting).await;
    let (_, by_hand) = rig.join(&credential(&one_time), "Hanna").await;
    let link_guest = by_link["user_id"].as_str().unwrap().to_owned();
    let hand_guest = by_hand["user_id"].as_str().unwrap().to_owned();

    let heard = events_of(&ws::events(&mut bob_socket, QUIET).await, "lobby_join");
    assert!(
      heard
        .iter()
        .any(|event| event["user"]["id"] == link_guest.as_str()),
      "a member hears knocks in a meeting without a lobby: {heard:?}"
    );
    // Bob is no host: he sees only who came with the meeting link.
    let lobby = app.get(&bob, &format!("/meetings/{meeting}/lobby")).await;
    assert_eq!(lobby.status, StatusCode::OK, "{}", lobby.body);
    assert_eq!(lobby.body["is_host"], false);
    let listed: Vec<&str> = lobby.body["entries"]
      .as_array()
      .unwrap()
      .iter()
      .filter_map(|entry| entry["user"]["id"].as_str())
      .collect();
    assert_eq!(listed, vec![link_guest.as_str()]);
    assert_eq!(
      app.get(&alice, &format!("/meetings/{meeting}/lobby")).await.body["is_host"],
      true
    );

    let decide = |guest: String, decision: &'static str| {
      let (app, bob, meeting) = (&rig.app, bob.clone(), meeting.clone());
      async move {
        app
          .post(
            &bob,
            &format!("/meetings/{meeting}/lobby/{guest}/{decision}"),
            json!({}),
          )
          .await
          .status
      }
    };
    assert_eq!(decide(hand_guest.clone(), "admit").await, StatusCode::FORBIDDEN);
    assert_eq!(decide(link_guest, "admit").await, StatusCode::NO_CONTENT);
    // Turning people away stays with the hosts.
    assert_eq!(decide(hand_guest, "deny").await, StatusCode::FORBIDDEN);
    rig.app.stop().await;
  }

  #[tokio::test]
  async fn with_a_lobby_only_hosts_see_and_decide() {
    let rig = rig!();
    let app = &rig.app;
    let (alice, bob) = (owner(), member("bob"));
    let (meeting, _) = rig.meeting_with(&alice, &bob, true).await;
    let token = rig.link_of(&alice, &meeting).await;
    let (_, joined) = rig.join(&token, "Gus").await;
    let guest = joined["user_id"].as_str().unwrap();

    assert_eq!(
      app.get(&bob, &format!("/meetings/{meeting}/lobby")).await.status,
      StatusCode::FORBIDDEN
    );
    assert_eq!(
      app
        .post(
          &bob,
          &format!("/meetings/{meeting}/lobby/{guest}/admit"),
          json!({})
        )
        .await
        .status,
      StatusCode::FORBIDDEN
    );
    rig.app.stop().await;
  }

  #[tokio::test]
  async fn the_meeting_list_marks_the_meetings_whose_lobby_the_caller_hosts() {
    let rig = rig!();
    let app = &rig.app;
    let (alice, bob) = (owner(), member("bob"));
    let (meeting, chat) = rig.meeting_with(&alice, &bob, true).await;
    let listed = |user: TestUser| {
      let (app, meeting) = (&rig.app, meeting.clone());
      async move {
        let list = app.get(&user, &format!("/meetings?from={STARTS_AT}")).await;
        assert_eq!(list.status, StatusCode::OK, "{}", list.body);
        let entry = list.body["meetings"]
          .as_array()
          .unwrap()
          .iter()
          .find(|entry| entry["id"] == meeting.as_str())
          .cloned()
          .unwrap();
        let lobby = app.get(&user, &format!("/meetings/{meeting}/lobby")).await;
        (entry["can_host_lobby"].clone(), lobby)
      }
    };

    // The organizer hosts; a member of a meeting with a lobby doesn't, and the field is left out.
    let (flag, lobby) = listed(alice.clone()).await;
    assert_eq!(flag, true);
    assert_eq!(lobby.body["is_host"], true);
    let (flag, lobby) = listed(bob.clone()).await;
    assert_eq!(flag, Value::Null);
    assert_eq!(lobby.status, StatusCode::FORBIDDEN);

    // Made chat admin, Bob hosts the lobby too, as the lobby itself says.
    let bob_id = user_id(app, &bob).await;
    teamcraft_db::testing::sqlx::query(
      "UPDATE memberships SET role = 'admin' WHERE conversation_id = $1 AND user_id = $2",
    )
    .bind(chat.parse::<i64>().unwrap())
    .bind(bob_id.parse::<i64>().unwrap())
    .execute(app.sql_pool())
    .await
    .unwrap();
    let (flag, lobby) = listed(bob.clone()).await;
    assert_eq!(flag, true);
    assert_eq!(lobby.body["is_host"], true);

    // A cancelled meeting has no lobby to host.
    assert_eq!(
      app.delete(&alice, &format!("/meetings/{meeting}")).await.status,
      StatusCode::OK
    );
    for user in [alice, bob] {
      let (flag, _) = listed(user).await;
      assert_eq!(flag, Value::Null);
    }
    rig.app.stop().await;
  }

  impl Rig {
    /// Schedules a meeting of `organizer` with `invitee` and returns (meeting id, chat id).
    async fn meeting_with(
      &self,
      organizer: &TestUser,
      invitee: &TestUser,
      has_lobby: bool,
    ) -> (String, String) {
      let invitee_id = user_id(&self.app, invitee).await;
      let reply = self
        .app
        .post(
          organizer,
          "/meetings",
          json!({
            "title": "Sync", "starts_at": STARTS_AT, "ends_at": ENDS_AT, "has_lobby": has_lobby,
            "invitees": [{"kind": "user", "user_id": invitee_id}],
          }),
        )
        .await;
      assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
      (
        reply.id(),
        reply.body["conversation_id"].as_str().unwrap().to_owned(),
      )
    }

    /// The token of the meeting's link, as "Copy link" hands it out.
    async fn link_of(&self, organizer: &TestUser, meeting: &str) -> String {
      let reply = self.app.get(organizer, &format!("/meetings/{meeting}")).await;
      link_token(reply.body["join_url"].as_str().unwrap())
    }
  }

  /// A signed admitters statement for the meeting and chat (IDs as the API writes them), base64.
  fn statement(organizer: &IdentityKeyPair, meeting: &str, chat: &str, sequence: u64) -> String {
    let signed = AdmittersDraft {
      meeting_id: MeetingId::new(meeting.parse().unwrap()),
      conversation_id: ConversationId::new(chat.parse().unwrap()),
      sequence,
      expires_at_ms: u64::try_from(ENDS_AT).unwrap(),
      link_admission: LinkAdmission::ByThemselves,
      admitters: Vec::new(),
    }
    .sign(organizer)
    .unwrap();
    STANDARD.encode(signed.to_bytes().unwrap())
  }
}
