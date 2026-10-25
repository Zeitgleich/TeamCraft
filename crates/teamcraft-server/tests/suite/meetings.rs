//! Meetings: scheduling, calendar events, invitees, answers and the meeting chat against a real
//! server and Postgres (`--features integration`, needs `DATABASE_URL`).
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_db::testing::sqlx;
use teamcraft_server::config::{MailTexts, MeetingsConfig};
use teamcraft_server::testing::{FakeCalendar, FakeMailer, Reply, TestApp, TestUser};

const GUEST_SECRET: &str = "0123456789abcdef0123456789abcdef";
const INBOX_TOKEN: &str = "inbox-token-for-tests";
const BASE_URL: &str = "https://chat.example.com";
/// 2030-01-01 09:00 UTC and one hour later, far enough in the future for every test.
const STARTS_AT: i64 = 1_893_488_400_000;
const ENDS_AT: i64 = STARTS_AT + 3_600_000;

fn config() -> MeetingsConfig {
  MeetingsConfig {
    public_base_url: BASE_URL.into(),
    organizer_fallback_address: "meetings@example.com".into(),
    caldav: None,
    max_invitees: 5,
    guest_token_secret: GUEST_SECRET.to_owned().into(),
    guest_grace_secs: 7200,
    max_shared_link_guests: 50,
    maybe_inbox_token: Some(INBOX_TOKEN.to_owned().into()),
    smtp: None,
    mail_texts: MailTexts::default(),
  }
}

async fn start() -> Option<(TestApp, Arc<FakeCalendar>)> {
  let (app, calendar, _) = start_with_mail().await?;
  Some((app, calendar))
}

async fn start_with_mail() -> Option<(TestApp, Arc<FakeCalendar>, Arc<FakeMailer>)> {
  let calendar = Arc::new(FakeCalendar::default());
  let mailer = Arc::new(FakeMailer::default());
  let app = TestApp::start_with_meetings(&config(), calendar.clone(), mailer.clone()).await?;
  Some((app, calendar, mailer))
}

macro_rules! app {
  () => {
    match start().await {
      Some(parts) => parts,
      None => return,
    }
  };
}

async fn schedule(app: &TestApp, organizer: &TestUser, invitees: Value) -> Reply {
  app
    .post(
      organizer,
      "/meetings",
      json!({
        "title": "Sprint review",
        "description": "Demo and planning",
        "starts_at": STARTS_AT,
        "ends_at": ENDS_AT,
        "invitees": invitees,
      }),
    )
    .await
}

fn unfolded(ics: &str) -> String {
  ics.replace("\r\n ", "")
}

mod scheduling {
  use super::*;

  #[tokio::test]
  async fn a_meeting_gets_a_chat_a_calendar_event_and_a_join_link() {
    let (app, calendar) = app!();
    let (alice, bob) = (owner(), member("bob"));
    let bob_id = user_id(&app, &bob).await;
    let alice_id = user_id(&app, &alice).await;

    let reply = schedule(
      &app,
      &alice,
      json!([
        {"kind": "user", "user_id": bob_id},
        {"kind": "external", "email": "Gus@Example.org", "name": "Gus Guest"},
      ]),
    )
    .await;

    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let meeting = &reply.body;
    let id = reply.id();
    assert_eq!(meeting["title"], "Sprint review");
    assert_eq!(
      (meeting["kind"].as_str(), meeting["status"].as_str()),
      (Some("scheduled"), Some("scheduled"))
    );
    assert_eq!(meeting["has_lobby"], true);
    assert_eq!(meeting["organizer_user_id"], alice_id.as_str());
    assert_eq!(meeting["join_url"], format!("{BASE_URL}/meeting/{id}").as_str());
    let attendees = meeting["attendees"].as_array().unwrap();
    assert_eq!(attendees.len(), 2);
    assert!(attendees.iter().all(|a| a["rsvp"] == "needs_action"));
    assert!(
      attendees
        .iter()
        .any(|a| a["email"] == "gus@example.org" && a["name"] == "Gus Guest")
    );
    assert!(attendees.iter().any(|a| a["user_id"] == bob_id.as_str()));

    // The meeting chat: organizer and invited users are members, the external invitee is not.
    let chat = meeting["conversation_id"].as_str().unwrap();
    let conversation = app.get(&bob, &format!("/conversations/{chat}")).await;
    assert_eq!(conversation.status, StatusCode::OK, "{}", conversation.body);
    assert_eq!(conversation.body["kind"], "meeting");
    assert_eq!(
      app
        .get(&member("eve"), &format!("/conversations/{chat}"))
        .await
        .status,
      StatusCode::NOT_FOUND
    );

    // The calendar event.
    let ics = unfolded(&calendar.event(&id).expect("event stored"));
    assert!(ics.contains(&format!("UID:{id}@chat.example.com")), "{ics}");
    assert!(ics.contains("DTSTART:20300101T090000Z") && ics.contains("DTEND:20300101T100000Z"));
    assert!(ics.contains("SUMMARY:Sprint review"));
    assert!(ics.contains(&format!("URL:{BASE_URL}/meeting/{id}")));
    assert!(ics.contains("mailto:gus@example.org") && ics.contains("mailto:bob@example.com"));
    assert!(
      ics.contains("ORGANIZER;CN=\"alice\":mailto:alice@example.com"),
      "{ics}"
    );
    assert!(!ics.contains("METHOD"));
    app.stop().await;
  }

  #[tokio::test]
  async fn recurrence_is_stored_returned_and_written_to_the_calendar() {
    let (app, calendar) = app!();
    let alice = owner();

    let reply = app
      .post(
        &alice,
        "/meetings",
        json!({
          "title": "Standup", "starts_at": STARTS_AT, "ends_at": ENDS_AT,
          "recurrence": {"frequency": "weekly", "interval": 2, "count": 6},
        }),
      )
      .await;

    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert_eq!(
      reply.body["recurrence"],
      json!({"frequency": "weekly", "interval": 2, "count": 6})
    );
    let ics = calendar.event(&reply.id()).unwrap();
    assert!(ics.contains("RRULE:FREQ=WEEKLY;INTERVAL=2;COUNT=6"), "{ics}");
    app.stop().await;
  }

  #[tokio::test]
  async fn invalid_requests_are_rejected() {
    let (app, calendar) = app!();
    let alice = owner();
    let base = |patch: Value| {
      let mut body = json!({"title": "Sync", "starts_at": STARTS_AT, "ends_at": ENDS_AT});
      for (key, value) in patch.as_object().unwrap() {
        body[key] = value.clone();
      }
      body
    };
    let too_many: Vec<Value> = (0..6)
      .map(|n| json!({"kind": "external", "email": format!("p{n}@example.org")}))
      .collect();
    for (what, body) in [
      ("blank title", base(json!({"title": "  "}))),
      ("end before start", base(json!({"ends_at": STARTS_AT - 1}))),
      (
        "bad address",
        base(json!({"invitees": [{"kind": "external", "email": "nobody"}]})),
      ),
      (
        "unknown user",
        base(json!({"invitees": [{"kind": "user", "user_id": "12345"}]})),
      ),
      ("too many invitees", base(json!({"invitees": too_many}))),
      (
        "bad recurrence",
        base(json!({"recurrence": {"frequency": "daily", "interval": 0}})),
      ),
      (
        "unknown invitee kind",
        base(json!({"invitees": [{"kind": "robot"}]})),
      ),
    ] {
      let reply = app.post(&alice, "/meetings", body).await;
      assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{what}: {}", reply.body);
    }
    assert!(calendar.names().is_empty(), "nothing reaches the calendar");
    app.stop().await;
  }

  #[tokio::test]
  async fn guests_cannot_schedule_and_unconfigured_servers_say_so() {
    let (app, _) = app!();
    let reply = schedule(&app, &guest("gina"), json!([])).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.body);
    app.stop().await;

    let Some(plain) = TestApp::start().await else {
      return;
    };
    let reply = schedule(&plain, &owner(), json!([])).await;
    assert_eq!(
      (reply.status, reply.error_code()),
      (StatusCode::SERVICE_UNAVAILABLE, "unavailable")
    );
    plain.stop().await;
  }

  #[tokio::test]
  async fn a_failing_calendar_undoes_the_meeting() {
    let (app, calendar) = app!();
    let alice = owner();
    calendar.set_failing(true);

    let reply = schedule(&app, &alice, json!([])).await;

    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE, "{}", reply.body);
    let listed = app.get(&alice, "/meetings?from=0").await;
    assert!(listed.body["meetings"].as_array().unwrap().is_empty());
    let conversations = app.get(&alice, "/conversations").await;
    assert!(conversations.body["conversations"].as_array().unwrap().is_empty());
    app.stop().await;
  }
}

mod visibility {
  use super::*;

  #[tokio::test]
  async fn organizer_and_invitees_see_the_meeting_and_hear_about_changes() {
    let (app, _) = app!();
    let (alice, bob, eve) = (owner(), member("bob"), member("eve"));
    let bob_id = user_id(&app, &bob).await;
    let (bob_device, eve_device) = (
      register_device(&app, &bob).await,
      register_device(&app, &eve).await,
    );
    let mut bob_socket = ws::connect(&app, &bob, &bob_device).await;
    let mut eve_socket = ws::connect(&app, &eve, &eve_device).await;
    ws::events(&mut bob_socket, Duration::from_millis(300)).await;
    ws::events(&mut eve_socket, Duration::from_millis(100)).await;

    let id = schedule(&app, &alice, json!([{"kind": "user", "user_id": bob_id}]))
      .await
      .id();

    let seen = meeting_updates(&ws::events(&mut bob_socket, Duration::from_millis(500)).await);
    assert_eq!(seen, std::slice::from_ref(&id));
    assert!(meeting_updates(&ws::events(&mut eve_socket, Duration::from_millis(300)).await).is_empty());
    for user in [&alice, &bob] {
      assert_eq!(
        app.get(user, &format!("/meetings/{id}")).await.status,
        StatusCode::OK
      );
      let listed = app.get(user, "/meetings").await;
      assert_eq!(listed.body["meetings"][0]["id"], id.as_str());
    }
    assert_eq!(
      app.get(&eve, &format!("/meetings/{id}")).await.status,
      StatusCode::NOT_FOUND
    );
    assert!(
      app.get(&eve, "/meetings").await.body["meetings"]
        .as_array()
        .unwrap()
        .is_empty()
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn the_list_is_filtered_by_time_and_ordered() {
    let (app, _) = app!();
    let alice = owner();
    let later = app
      .post(
        &alice,
        "/meetings",
        json!({"title": "Later", "starts_at": STARTS_AT + 86_400_000, "ends_at": ENDS_AT + 86_400_000}),
      )
      .await
      .id();
    let first = schedule(&app, &alice, json!([])).await.id();

    let all = app.get(&alice, "/meetings").await;
    let ids: Vec<_> = all.body["meetings"]
      .as_array()
      .unwrap()
      .iter()
      .map(|m| m["id"].as_str().unwrap().to_owned())
      .collect();
    assert_eq!(ids, [first.clone(), later.clone()]);

    let early = app.get(&alice, &format!("/meetings?to={}", STARTS_AT + 1)).await;
    assert_eq!(early.body["meetings"].as_array().unwrap().len(), 1);
    let past = app
      .get(&alice, &format!("/meetings?from={}", ENDS_AT + 86_400_000 + 1))
      .await;
    assert!(past.body["meetings"].as_array().unwrap().is_empty());
    app.stop().await;
  }
}

mod changes {
  use super::*;

  #[tokio::test]
  async fn the_organizer_changes_time_title_and_invitees_and_the_calendar_follows() {
    let (app, calendar) = app!();
    let (alice, bob, carol) = (owner(), member("bob"), member("carol"));
    let (bob_id, carol_id) = (user_id(&app, &bob).await, user_id(&app, &carol).await);
    let created = schedule(&app, &alice, json!([{"kind": "user", "user_id": bob_id}])).await;
    let (id, chat) = (
      created.id(),
      created.body["conversation_id"].as_str().unwrap().to_owned(),
    );

    let reply = app
      .patch(
        &alice,
        &format!("/meetings/{id}"),
        json!({
          "title": "Sprint review (moved)",
          "starts_at": STARTS_AT + 7_200_000,
          "ends_at": ENDS_AT + 7_200_000,
          "invitees": [{"kind": "user", "user_id": carol_id}],
        }),
      )
      .await;

    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["title"], "Sprint review (moved)");
    let ics = unfolded(&calendar.event(&id).unwrap());
    assert!(ics.contains("SUMMARY:Sprint review (moved)") && ics.contains("DTSTART:20300101T110000Z"));
    assert!(ics.contains("SEQUENCE:1"), "{ics}");
    assert!(ics.contains("carol@example.com") && !ics.contains("bob@example.com"));
    // bob left the chat and the meeting, carol joined.
    assert_eq!(
      app.get(&bob, &format!("/conversations/{chat}")).await.status,
      StatusCode::NOT_FOUND
    );
    assert_eq!(
      app.get(&carol, &format!("/conversations/{chat}")).await.status,
      StatusCode::OK
    );
    assert_eq!(
      app.get(&bob, &format!("/meetings/{id}")).await.status,
      StatusCode::NOT_FOUND
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn invitees_keep_their_answer_when_the_list_is_resent() {
    let (app, _) = app!();
    let (alice, bob) = (owner(), member("bob"));
    let bob_id = user_id(&app, &bob).await;
    let id = schedule(&app, &alice, json!([{"kind": "user", "user_id": bob_id}]))
      .await
      .id();
    app
      .put(
        &bob,
        &format!("/meetings/{id}/rsvp"),
        json!({"status": "accepted"}),
      )
      .await;

    let reply = app
      .patch(
        &alice,
        &format!("/meetings/{id}"),
        json!({"invitees": [{"kind": "user", "user_id": bob_id}, {"kind": "external", "email": "x@example.org"}]}),
      )
      .await;

    let rsvps: Vec<_> = reply.body["attendees"]
      .as_array()
      .unwrap()
      .iter()
      .map(|a| a["rsvp"].as_str().unwrap().to_owned())
      .collect();
    assert_eq!(rsvps, ["accepted", "needs_action"]);
    app.stop().await;
  }

  #[tokio::test]
  async fn only_the_organizer_may_change_or_cancel() {
    let (app, calendar) = app!();
    let (alice, bob) = (owner(), member("bob"));
    let bob_id = user_id(&app, &bob).await;
    let id = schedule(&app, &alice, json!([{"kind": "user", "user_id": bob_id}]))
      .await
      .id();

    let patch = app
      .patch(&bob, &format!("/meetings/{id}"), json!({"title": "Mine now"}))
      .await;
    let cancel = app.delete(&bob, &format!("/meetings/{id}")).await;
    let eve_patch = app
      .patch(&member("eve"), &format!("/meetings/{id}"), json!({"title": "x"}))
      .await;

    assert_eq!(
      (patch.status, cancel.status, eve_patch.status),
      (
        StatusCode::FORBIDDEN,
        StatusCode::FORBIDDEN,
        StatusCode::NOT_FOUND
      )
    );
    assert!(calendar.event(&id).unwrap().contains("SUMMARY:Sprint review"));
    app.stop().await;
  }

  #[tokio::test]
  async fn cancelling_removes_the_event_archives_the_chat_and_locks_the_meeting() {
    let (app, calendar) = app!();
    let (alice, bob) = (owner(), member("bob"));
    let bob_id = user_id(&app, &bob).await;
    let created = schedule(&app, &alice, json!([{"kind": "user", "user_id": bob_id}])).await;
    let (id, chat) = (
      created.id(),
      created.body["conversation_id"].as_str().unwrap().to_owned(),
    );

    let cancelled = app.delete(&alice, &format!("/meetings/{id}")).await;

    assert_eq!(cancelled.status, StatusCode::OK, "{}", cancelled.body);
    assert_eq!(cancelled.body["status"], "cancelled");
    assert!(calendar.event(&id).is_none(), "the event left the calendar");
    let conversation = app.get(&bob, &format!("/conversations/{chat}")).await;
    assert_eq!(conversation.body["is_archived"], true, "{}", conversation.body);
    assert_eq!(
      app.delete(&alice, &format!("/meetings/{id}")).await.status,
      StatusCode::CONFLICT
    );
    assert_eq!(
      app
        .patch(&alice, &format!("/meetings/{id}"), json!({"title": "x"}))
        .await
        .status,
      StatusCode::CONFLICT
    );
    // Invitees still see it (struck through), so they learn it was cancelled.
    assert_eq!(
      app.get(&bob, &format!("/meetings/{id}")).await.body["status"],
      "cancelled"
    );
    app.stop().await;
  }
}

mod answers {
  use super::*;

  #[tokio::test]
  async fn invitees_answer_and_the_calendar_shows_it() {
    let (app, calendar) = app!();
    let (alice, bob) = (owner(), member("bob"));
    let bob_id = user_id(&app, &bob).await;
    let id = schedule(&app, &alice, json!([{"kind": "user", "user_id": bob_id}]))
      .await
      .id();

    let answer = app
      .put(
        &bob,
        &format!("/meetings/{id}/rsvp"),
        json!({"status": "declined"}),
      )
      .await;

    assert_eq!(answer.status, StatusCode::NO_CONTENT, "{}", answer.body);
    let meeting = app.get(&alice, &format!("/meetings/{id}")).await;
    assert_eq!(meeting.body["attendees"][0]["rsvp"], "declined");
    assert!(calendar.event(&id).unwrap().contains("PARTSTAT=DECLINED"));
    let bad = app
      .put(
        &bob,
        &format!("/meetings/{id}/rsvp"),
        json!({"status": "maybe-later"}),
      )
      .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST, "{}", bad.body);
    app.stop().await;
  }

  #[tokio::test]
  async fn people_who_are_not_invitees_cannot_answer() {
    let (app, _) = app!();
    let alice = owner();
    let id = schedule(&app, &alice, json!([])).await.id();

    let organizer = app
      .put(
        &alice,
        &format!("/meetings/{id}/rsvp"),
        json!({"status": "accepted"}),
      )
      .await;
    let stranger = app
      .put(
        &member("eve"),
        &format!("/meetings/{id}/rsvp"),
        json!({"status": "accepted"}),
      )
      .await;

    assert_eq!(
      organizer.status,
      StatusCode::FORBIDDEN,
      "the organizer is not an invitee"
    );
    assert_eq!(stranger.status, StatusCode::NOT_FOUND);
    app.stop().await;
  }
}

mod invitations {
  use teamcraft_server::meetings::ical::Method;

  use super::*;

  macro_rules! mail_app {
    () => {
      match start_with_mail().await {
        Some(parts) => parts,
        None => return,
      }
    };
  }

  #[tokio::test]
  async fn invitees_get_an_imip_request_and_externals_a_personal_link() {
    let (app, _, mailer) = mail_app!();
    let (alice, bob) = (owner(), member("bob"));
    let bob_id = user_id(&app, &bob).await;

    let reply = schedule(
      &app,
      &alice,
      json!([
        {"kind": "user", "user_id": bob_id},
        {"kind": "external", "email": "gus@example.org", "name": "Gus Guest"},
      ]),
    )
    .await;

    assert!(
      reply.body.get("undelivered_invitations").is_none(),
      "{}",
      reply.body
    );
    let id = reply.id();
    let sent = mailer.sent();
    assert_eq!(sent.len(), 2);
    let to_bob = sent.iter().find(|m| m.to_address == "bob@example.com").unwrap();
    let to_gus = sent.iter().find(|m| m.to_address == "gus@example.org").unwrap();
    for mail in &sent {
      assert_eq!(mail.method, Method::Request);
      assert_eq!(mail.subject, "Invitation: Sprint review");
      assert!(
        mail.text.contains("alice invites you to \"Sprint review\""),
        "{}",
        mail.text
      );
      assert!(mail.text.contains("Start: 2030-01-01 09:00 UTC"), "{}", mail.text);
      assert!(
        mail.ics.contains("METHOD:REQUEST") && mail.ics.contains(&format!("UID:{id}@chat.example.com"))
      );
    }
    // Members join with their account, externals with a personal link that carries a token.
    assert!(
      to_bob.text.contains(&format!("Join: {BASE_URL}/meeting/{id}\n")),
      "{}",
      to_bob.text
    );
    assert!(
      to_gus.text.contains(&format!("{BASE_URL}/meeting/{id}#invite=")),
      "{}",
      to_gus.text
    );
    assert!(!to_bob.text.contains("#invite="));
    app.stop().await;
  }

  #[tokio::test]
  async fn updates_additions_removals_and_cancellation_send_the_matching_mail() {
    let (app, _, mailer) = mail_app!();
    let alice = owner();
    let id = schedule(
      &app,
      &alice,
      json!([
        {"kind": "external", "email": "keep@example.org"},
        {"kind": "external", "email": "drop@example.org"},
      ]),
    )
    .await
    .id();
    let before = mailer.sent().len();

    app
      .patch(
        &alice,
        &format!("/meetings/{id}"),
        json!({
          "title": "Moved",
          "invitees": [
            {"kind": "external", "email": "keep@example.org"},
            {"kind": "external", "email": "new@example.org"},
          ],
        }),
      )
      .await;

    let sent = mailer.sent();
    let after: Vec<_> = sent[before..].iter().collect();
    let find = |address: &str| after.iter().find(|m| m.to_address == address).unwrap();
    assert_eq!(after.len(), 3);
    assert_eq!(
      (
        find("keep@example.org").method,
        find("keep@example.org").subject.as_str()
      ),
      (Method::Request, "Updated: Moved")
    );
    assert_eq!(
      (
        find("new@example.org").method,
        find("new@example.org").subject.as_str()
      ),
      (Method::Request, "Invitation: Moved")
    );
    assert_eq!(
      (
        find("drop@example.org").method,
        find("drop@example.org").subject.as_str()
      ),
      (Method::Cancel, "Cancelled: Moved")
    );
    assert!(
      find("drop@example.org").ics.contains("STATUS:CANCELLED")
        && find("keep@example.org").ics.contains("SEQUENCE:1")
    );
    // The same person keeps the same link across updates.
    let link = |mail: &teamcraft_server::meetings::mailer::OutgoingMail| {
      mail
        .text
        .split("#invite=")
        .nth(1)
        .map(|t| t.lines().next().unwrap().to_owned())
    };
    let first_link = sent[..before]
      .iter()
      .find(|m| m.to_address == "keep@example.org")
      .and_then(link);
    assert_eq!(first_link, link(find("keep@example.org")));

    let before_cancel = mailer.sent().len();
    app.delete(&alice, &format!("/meetings/{id}")).await;
    let cancelled = &mailer.sent()[before_cancel..];
    assert_eq!(cancelled.len(), 2);
    assert!(
      cancelled
        .iter()
        .all(|m| m.method == Method::Cancel && m.ics.contains("METHOD:CANCEL"))
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn refused_addresses_are_reported_but_do_not_fail_the_meeting() {
    let (app, _, mailer) = mail_app!();
    mailer.refuse("down@example.org");

    let reply = schedule(
      &app,
      &owner(),
      json!([{"kind": "external", "email": "down@example.org"}, {"kind": "external", "email": "up@example.org"}]),
    )
    .await;

    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert_eq!(reply.body["undelivered_invitations"], json!(["down@example.org"]));
    assert_eq!(mailer.sent().len(), 1);
    app.stop().await;
  }

  #[tokio::test]
  async fn instant_meetings_send_no_mail() {
    let (app, _, mailer) = mail_app!();
    let alice = owner();
    app.post(&alice, "/meetings/instant", json!({})).await;
    assert!(mailer.sent().is_empty());
    app.stop().await;
  }
}

mod email_replies {
  use super::*;

  fn reply_for(uid: &str, attendee: &str, status: &str) -> String {
    format!(
      "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REPLY\r\nBEGIN:VEVENT\r\nUID:{uid}\r\n\
       ATTENDEE;PARTSTAT={status};CN=Someone:mailto:{attendee}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
  }

  /// A reply as the inbox handler would forward it: the sender is the attendee of the reply.
  async fn post_reply(app: &TestApp, maybe_token: Option<&str>, body: String) -> (StatusCode, Value) {
    let sender = body
      .split("mailto:")
      .nth(1)
      .and_then(|rest| rest.split(['\r', '\n']).next())
      .unwrap_or("nobody@example.org")
      .to_owned();
    post_reply_from(app, maybe_token, &sender, body).await
  }

  async fn post_reply_from(
    app: &TestApp,
    maybe_token: Option<&str>,
    sender: &str,
    body: String,
  ) -> (StatusCode, Value) {
    let mut request = app
      .http
      .post(format!("{}/api/internal/v1/meetings/replies", app.base_url))
      .header("content-type", "text/calendar")
      .header("x-envelope-sender", sender)
      .body(body);
    if let Some(token) = maybe_token {
      request = request.bearer_auth(token);
    }
    let response = request.send().await.unwrap();
    let status = response.status();
    (status, response.json().await.unwrap_or(Value::Null))
  }

  #[tokio::test]
  async fn only_the_attendee_can_answer_for_themselves_and_only_for_themselves() {
    let (app, _) = app!();
    let alice = owner();
    let id = schedule(
      &app,
      &alice,
      json!([{"kind": "external", "email": "gus@example.org"}, {"kind": "external", "email": "hana@example.org"}]),
    )
    .await
    .id();
    let uid = format!("{id}@chat.example.com");
    let reply = reply_for(&uid, "gus@example.org", "DECLINED");

    // Mail from somebody else, or without a sender, changes nothing.
    let (status, _) = post_reply_from(&app, Some(INBOX_TOKEN), "mallory@example.org", reply.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let response = app
      .http
      .post(format!("{}/api/internal/v1/meetings/replies", app.base_url))
      .bearer_auth(INBOX_TOKEN)
      .header("content-type", "text/calendar")
      .body(reply.clone())
      .send()
      .await
      .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    // A reply with two attendees is refused whoever sends it.
    let two = reply.replace(
      "END:VEVENT",
      "ATTENDEE;PARTSTAT=DECLINED:mailto:hana@example.org\r\nEND:VEVENT",
    );
    assert_eq!(
      post_reply_from(&app, Some(INBOX_TOKEN), "gus@example.org", two)
        .await
        .0,
      StatusCode::BAD_REQUEST
    );
    let meeting = app.get(&alice, &format!("/meetings/{id}")).await;
    assert!(
      meeting.body["attendees"]
        .as_array()
        .unwrap()
        .iter()
        .all(|a| a["rsvp"] != "declined")
    );

    // The sender may be written as a header value with a display name.
    let (status, body) = post_reply_from(&app, Some(INBOX_TOKEN), "Gus Guest <Gus@Example.org>", reply).await;
    assert_eq!((status, body["applied"].as_u64()), (StatusCode::OK, Some(1)));
    app.stop().await;
  }

  #[tokio::test]
  async fn a_reply_by_e_mail_sets_the_answer_of_an_external_invitee_and_the_calendar_shows_it() {
    let (app, calendar) = app!();
    let (alice, bob) = (owner(), member("bob"));
    let bob_id = user_id(&app, &bob).await;
    let reply = schedule(
      &app,
      &alice,
      json!([{"kind": "external", "email": "gus@example.org"}, {"kind": "user", "user_id": bob_id}]),
    )
    .await;
    let id = reply.id();
    let uid = format!("{id}@chat.example.com");
    let mut alice_socket = ws::connect(&app, &alice, &register_device(&app, &alice).await).await;
    ws::events(&mut alice_socket, Duration::from_millis(300)).await;

    let (status, body) = post_reply(
      &app,
      Some(INBOX_TOKEN),
      reply_for(&uid, "Gus@Example.org", "ACCEPTED"),
    )
    .await;

    assert_eq!((status, body["applied"].as_u64()), (StatusCode::OK, Some(1)));
    let meeting = app.get(&alice, &format!("/meetings/{id}")).await;
    let gus = meeting.body["attendees"]
      .as_array()
      .unwrap()
      .iter()
      .find(|a| a["email"] == "gus@example.org")
      .unwrap()
      .clone();
    assert_eq!(gus["rsvp"], "accepted");
    assert!(calendar.event(&id).unwrap().contains("PARTSTAT=ACCEPTED"));
    assert_eq!(
      meeting_updates(&ws::events(&mut alice_socket, Duration::from_millis(500)).await),
      std::slice::from_ref(&id)
    );

    // A later reply replaces the answer.
    let (_, declined) = post_reply(
      &app,
      Some(INBOX_TOKEN),
      reply_for(&uid, "gus@example.org", "DECLINED"),
    )
    .await;
    assert_eq!(declined["applied"], 1);
    app.stop().await;
  }

  #[tokio::test]
  async fn unknown_meetings_and_people_are_ignored_and_malformed_replies_rejected() {
    let (app, _) = app!();
    let id = schedule(
      &app,
      &owner(),
      json!([{"kind": "external", "email": "gus@example.org"}]),
    )
    .await
    .id();
    let uid = format!("{id}@chat.example.com");

    let unknown_meeting = post_reply(
      &app,
      Some(INBOX_TOKEN),
      reply_for("1@chat.example.com", "gus@example.org", "ACCEPTED"),
    )
    .await;
    let unknown_person = post_reply(
      &app,
      Some(INBOX_TOKEN),
      reply_for(&uid, "stranger@example.org", "ACCEPTED"),
    )
    .await;
    let garbage = post_reply(&app, Some(INBOX_TOKEN), "hello".into()).await;
    let request = post_reply(
      &app,
      Some(INBOX_TOKEN),
      reply_for(&uid, "gus@example.org", "ACCEPTED").replace("REPLY", "REQUEST"),
    )
    .await;

    assert_eq!(
      (unknown_meeting.0, unknown_meeting.1["applied"].as_u64()),
      (StatusCode::OK, Some(0))
    );
    assert_eq!(
      (unknown_person.0, unknown_person.1["applied"].as_u64()),
      (StatusCode::OK, Some(0))
    );
    assert_eq!(garbage.0, StatusCode::BAD_REQUEST);
    assert_eq!(request.0, StatusCode::BAD_REQUEST);
    app.stop().await;
  }

  #[tokio::test]
  async fn the_endpoint_needs_the_inbox_token() {
    let (app, _) = app!();
    let body = reply_for("1@chat.example.com", "gus@example.org", "ACCEPTED");

    let missing = post_reply(&app, None, body.clone()).await;
    let wrong = post_reply(&app, Some("wrong"), body).await;

    assert_eq!(
      (missing.0, wrong.0),
      (StatusCode::UNAUTHORIZED, StatusCode::UNAUTHORIZED)
    );
    app.stop().await;
  }
}

mod instant {
  use super::*;

  #[tokio::test]
  async fn meet_now_starts_immediately_without_calendar_entry() {
    let (app, calendar) = app!();
    let alice = owner();

    let reply = app.post(&alice, "/meetings/instant", json!({})).await;

    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert_eq!(reply.body["kind"], "instant");
    assert_eq!(reply.body["title"], "Instant meeting");
    assert!(reply.body["join_url"].as_str().unwrap().starts_with(BASE_URL));
    assert!(calendar.names().is_empty());
    let starts = reply.body["starts_at"].as_i64().unwrap();
    let ends = reply.body["ends_at"].as_i64().unwrap();
    assert!(ends - starts >= 3_600_000);
    let named = app
      .post(&alice, "/meetings/instant", json!({"title": "Quick sync"}))
      .await;
    assert_eq!(named.body["title"], "Quick sync");
    app.stop().await;
  }
}

mod retried_creation {
  use super::*;

  const KEY: &str = "ffeeddccbbaa99887766554433221100";

  #[tokio::test]
  async fn a_repeated_meeting_request_creates_one_meeting_and_one_invitation() {
    let (app, calendar) = app!();
    let alice = owner();
    let body = json!({
      "title": "Retried", "starts_at": 4_102_444_800_000_i64, "ends_at": 4_102_448_400_000_i64,
      "idempotency_key": KEY,
    });

    let first = app.post(&alice, "/meetings", body.clone()).await;
    let second = app.post(&alice, "/meetings", body).await;

    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
    assert_eq!(first.body["id"], second.body["id"]);
    assert_eq!(calendar.names().len(), 1);
    let meetings: i64 = sqlx::query_scalar("SELECT count(*) FROM meetings")
      .fetch_one(app.sql_pool())
      .await
      .unwrap();
    assert_eq!(meetings, 1);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_repeated_meet_now_request_returns_the_same_meeting() {
    let (app, _calendar) = app!();
    let alice = owner();
    let body = json!({"idempotency_key": KEY});

    let first = app.post(&alice, "/meetings/instant", body.clone()).await;
    let second = app.post(&alice, "/meetings/instant", body).await;

    assert_eq!(first.body["id"], second.body["id"]);
    app.stop().await;
  }
}

fn meeting_updates(events: &[teamcraft_proto::GatewayEvent]) -> Vec<String> {
  events
    .iter()
    .map(|event| serde_json::to_value(event).unwrap())
    .filter(|json| json["type"] == "meeting_update")
    .map(|json| json["meeting_id"].as_str().unwrap().to_owned())
    .collect()
}
