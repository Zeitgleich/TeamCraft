//! A planned meeting's reminder and start notice: a normal push at the scheduled time, not a ring
//! (#1421, docs/meetings.md "Starting").
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_proto::{PushKind, PushProvider, PushWakeUp};
use teamcraft_server::auth::BoxFuture;
use teamcraft_server::config::{MailTexts, MeetingsConfig};
use teamcraft_server::push::crypto::open_wakeup;
use teamcraft_server::push::{PushDispatcher, PushError, PushMessage, PushSender, PushTarget};
use teamcraft_server::services::meeting_notices;
use teamcraft_server::testing::{FakeCalendar, FakeMailer, Reply, TestApp, TestUser};
use time::OffsetDateTime;

const MINUTE: i64 = 60_000;
const PUSH_KEY_BYTES: [u8; 32] = [9; 32];

fn config() -> MeetingsConfig {
  MeetingsConfig {
    public_base_url: "https://teamcraft.example.com".into(),
    organizer_fallback_address: "meetings@example.com".into(),
    caldav: None,
    max_invitees: 5,
    guest_token_secret: "guest-secret-for-the-tests-only-012345".to_owned().into(),
    guest_grace_secs: 7200,
    max_shared_link_guests: 50,
    start_ring_lead_secs: 3600,
    maybe_inbox_token: None,
    smtp: None,
    mail_texts: MailTexts::default(),
  }
}

#[derive(Clone, Default)]
struct Recorder {
  seen: Arc<Mutex<Vec<(String, PushWakeUp)>>>,
}

impl PushSender for Recorder {
  fn send<'a>(
    &'a self,
    target: &'a PushTarget,
    message: &'a PushMessage,
  ) -> BoxFuture<'a, Result<(), PushError>> {
    Box::pin(async move {
      let wake_up = open_wakeup(&target.push_key, &message.sealed)?;
      self
        .seen
        .lock()
        .unwrap()
        .push((target.device.to_string(), wake_up));
      Ok(())
    })
  }
}

impl Recorder {
  /// The pushes that arrived so far, after a moment for the ones on their way.
  async fn taken(&self) -> Vec<(String, PushWakeUp)> {
    tokio::time::sleep(Duration::from_millis(200)).await;
    std::mem::take(&mut *self.seen.lock().unwrap())
  }
}

async fn start(recorder: &Recorder) -> Option<TestApp> {
  TestApp::start_with_meetings_and_push(
    &config(),
    Arc::new(FakeCalendar::default()),
    Arc::new(FakeMailer::default()),
    PushDispatcher::new(8)
      .with_sender(&PushProvider::Apns, Arc::new(recorder.clone()))
      .with_sender(&PushProvider::Fcm, Arc::new(recorder.clone())),
  )
  .await
}

fn now_ms() -> i64 {
  OffsetDateTime::now_utc().unix_timestamp() * 1000
}

/// `run_due` at a chosen moment: the test doesn't wait for the time.
async fn run_at(app: &TestApp, at_ms: i64) -> usize {
  let at = OffsetDateTime::from_unix_timestamp(at_ms / 1000).unwrap();
  meeting_notices::run_due(&app.state, at).await.unwrap()
}

async fn phone(app: &TestApp, user: &TestUser) -> String {
  let device = register_device(app, user).await;
  let reply = app
    .put(
      user,
      &format!("/devices/{device}/push"),
      json!({"provider": "fcm", "token": "t", "push_key": STANDARD.encode(PUSH_KEY_BYTES)}),
    )
    .await;
  assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
  device
}

struct Scene {
  app: TestApp,
  alice: TestUser,
  bob: TestUser,
  carol: TestUser,
  bob_phone: String,
  carol_phone: String,
  bob_id: String,
  carol_id: String,
  recorder: Recorder,
}

async fn scene() -> Option<Scene> {
  let recorder = Recorder::default();
  let app = start(&recorder).await?;
  let (alice, bob, carol) = (owner(), member("bob"), member("carol"));
  let (bob_id, carol_id) = (user_id(&app, &bob).await, user_id(&app, &carol).await);
  let bob_phone = phone(&app, &bob).await;
  let carol_phone = phone(&app, &carol).await;
  Some(Scene {
    app,
    alice,
    bob,
    carol,
    bob_phone,
    carol_phone,
    bob_id,
    carol_id,
    recorder,
  })
}

impl Scene {
  /// A meeting of Alice's with Bob and Carol starting at `starts_at`, 30 minutes long.
  async fn schedule(&self, starts_at: i64, maybe_recurrence: Option<Value>) -> (String, String) {
    let mut body = json!({
      "title": "Sprint review", "starts_at": starts_at, "ends_at": starts_at + 30 * MINUTE,
      "invitees": [{"kind": "user", "user_id": self.bob_id}, {"kind": "user", "user_id": self.carol_id}],
    });
    if let Some(recurrence) = maybe_recurrence {
      body["recurrence"] = recurrence;
    }
    let reply = self.app.post(&self.alice, "/meetings", body).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    (
      reply.id(),
      reply.body["conversation_id"].as_str().unwrap().to_owned(),
    )
  }

  async fn set_reminder(&self, user: &TestUser, body: Value) {
    let current = self.app.get(user, "/me/notification-settings").await;
    let mut settings = current.body;
    for (key, value) in body.as_object().unwrap() {
      settings[key] = value.clone();
    }
    let reply = self.app.put(user, "/me/notification-settings", settings).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  }
}

fn kinds(pushes: &[(String, PushWakeUp)]) -> Vec<(String, PushKind)> {
  let mut kinds: Vec<_> = pushes
    .iter()
    .map(|(device, wake_up)| (device.clone(), wake_up.kind.clone()))
    .collect();
  kinds.sort_by(|a, b| a.0.cmp(&b.0));
  kinds
}

mod timing {
  use super::*;

  #[tokio::test]
  async fn the_reminder_and_the_start_notice_come_at_their_time_for_who_did_not_decline() {
    let Some(scene) = scene().await else { return };
    let starts_at = now_ms() + 24 * 60 * MINUTE;
    let (id, chat) = scene.schedule(starts_at, None).await;
    let declined = scene
      .app
      .put(
        &scene.carol,
        &format!("/meetings/{id}/rsvp"),
        json!({"status": "declined"}),
      )
      .await;
    assert_eq!(declined.status, StatusCode::NO_CONTENT, "{}", declined.body);

    assert_eq!(run_at(&scene.app, starts_at - 6 * MINUTE).await, 0);
    // Bob and Alice, the organizer; only Bob has a phone here.
    assert_eq!(run_at(&scene.app, starts_at - 5 * MINUTE).await, 2);
    let pushes = scene.recorder.taken().await;
    assert_eq!(
      kinds(&pushes),
      [(scene.bob_phone.clone(), PushKind::MeetingReminder)]
    );
    let wake_up = &pushes[0].1;
    assert_eq!(wake_up.conversation_id.to_string(), chat);
    assert_eq!(wake_up.maybe_meeting.unwrap().starts_at.0, starts_at);
    assert_eq!(wake_up.maybe_call_id, None, "no ring");

    assert_eq!(run_at(&scene.app, starts_at).await, 2);
    let pushes = scene.recorder.taken().await;
    assert_eq!(
      kinds(&pushes),
      [(scene.bob_phone.clone(), PushKind::MeetingStart)]
    );

    // Too late for a start notice, however long the server was down.
    assert_eq!(run_at(&scene.app, starts_at + 20 * MINUTE).await, 0);
  }

  #[tokio::test]
  async fn the_lead_time_is_per_user_and_can_be_off() {
    let Some(scene) = scene().await else { return };
    let starts_at = now_ms() + 2 * 24 * 60 * MINUTE;
    scene.schedule(starts_at, None).await;
    scene
      .set_reminder(&scene.bob, json!({"meeting_reminder_minutes": 0}))
      .await;
    scene
      .set_reminder(&scene.carol, json!({"meeting_reminder_minutes": 15}))
      .await;

    assert_eq!(run_at(&scene.app, starts_at - 15 * MINUTE).await, 1);
    let pushes = scene.recorder.taken().await;
    assert_eq!(
      kinds(&pushes),
      [(scene.carol_phone.clone(), PushKind::MeetingReminder)]
    );
    assert_eq!(
      run_at(&scene.app, starts_at - 5 * MINUTE).await,
      1,
      "only the organizer's default"
    );
    assert!(scene.recorder.taken().await.is_empty(), "Bob has it off");

    // The start notice can be turned off apart from the reminder.
    scene
      .set_reminder(&scene.carol, json!({"notifies_meeting_start": false}))
      .await;
    run_at(&scene.app, starts_at).await;
    let pushes = scene.recorder.taken().await;
    assert_eq!(
      kinds(&pushes),
      [(scene.bob_phone.clone(), PushKind::MeetingStart)]
    );
  }

  #[tokio::test]
  async fn a_lead_time_beyond_the_limit_is_refused() {
    let Some(scene) = scene().await else { return };
    let current = scene.app.get(&scene.bob, "/me/notification-settings").await;
    let mut settings = current.body;
    settings["meeting_reminder_minutes"] = json!(61);
    let reply = scene
      .app
      .put(&scene.bob, "/me/notification-settings", settings)
      .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
  }
}

mod not_twice {
  use super::*;

  #[tokio::test]
  async fn a_second_pass_or_instance_sends_nothing_again() {
    let Some(scene) = scene().await else { return };
    let starts_at = now_ms() + 3 * 24 * 60 * MINUTE;
    scene.schedule(starts_at, None).await;

    // Two instances at the same moment, then one more pass after a restart.
    let at = starts_at - 5 * MINUTE;
    let (first, second) = tokio::join!(run_at(&scene.app, at), run_at(&scene.app, at));
    assert_eq!(first + second, 3, "one reminder each for Alice, Bob and Carol");
    assert_eq!(run_at(&scene.app, at + MINUTE).await, 0);
    assert_eq!(scene.recorder.taken().await.len(), 2, "the two phones");
  }
}

mod changes {
  use super::*;

  #[tokio::test]
  async fn a_moved_meeting_reminds_only_at_its_new_time() {
    let Some(scene) = scene().await else { return };
    let starts_at = now_ms() + 4 * 24 * 60 * MINUTE;
    let (id, _) = scene.schedule(starts_at, None).await;
    let moved_to = starts_at + 3 * 60 * MINUTE;
    let moved = scene
      .app
      .patch(
        &scene.alice,
        &format!("/meetings/{id}"),
        json!({"starts_at": moved_to, "ends_at": moved_to + 30 * MINUTE}),
      )
      .await;
    assert_eq!(moved.status, StatusCode::OK, "{}", moved.body);

    assert_eq!(
      run_at(&scene.app, starts_at - 5 * MINUTE).await,
      0,
      "the old time"
    );
    assert_eq!(run_at(&scene.app, moved_to - 5 * MINUTE).await, 3, "the new time");
  }

  #[tokio::test]
  async fn a_cancelled_meeting_stays_quiet() {
    let Some(scene) = scene().await else { return };
    let starts_at = now_ms() + 5 * 24 * 60 * MINUTE;
    let (id, _) = scene.schedule(starts_at, None).await;
    let cancelled = scene.app.delete(&scene.alice, &format!("/meetings/{id}")).await;
    assert_eq!(cancelled.status, StatusCode::OK, "{}", cancelled.body);

    assert_eq!(run_at(&scene.app, starts_at - 5 * MINUTE).await, 0);
    assert_eq!(run_at(&scene.app, starts_at).await, 0);
    assert!(scene.recorder.taken().await.is_empty());
  }

  #[tokio::test]
  async fn a_series_reminds_per_occurrence_but_not_the_cancelled_one() {
    let Some(scene) = scene().await else { return };
    let day = 24 * 60 * MINUTE;
    let first = now_ms() + day;
    let (id, _) = scene
      .schedule(first, Some(json!({"frequency": "daily", "interval": 1})))
      .await;
    assert_eq!(run_at(&scene.app, first - 5 * MINUTE).await, 3);
    assert_eq!(
      run_at(&scene.app, first + day - 5 * MINUTE).await,
      3,
      "the next day's"
    );

    let second = first + 2 * day;
    let cancelled = scene
      .app
      .delete(
        &scene.alice,
        &format!("/meetings/{id}?scope=occurrence&occurrence={second}"),
      )
      .await;
    assert_eq!(cancelled.status, StatusCode::OK, "{}", cancelled.body);
    assert_eq!(run_at(&scene.app, second - 5 * MINUTE).await, 0);
  }
}

mod dismissal {
  use super::*;

  async fn report(scene: &Scene, user: &TestUser, meeting: &str, starts_at: i64) -> Reply {
    scene
      .app
      .post(
        user,
        &format!("/meetings/{meeting}/notices/dismiss"),
        json!({"starts_at": starts_at}),
      )
      .await
  }

  /// The withdrawals among the pushes, as (device, meeting, occurrence start).
  fn withdrawals(pushes: &[(String, PushWakeUp)]) -> Vec<(String, String, i64)> {
    let mut found: Vec<_> = pushes
      .iter()
      .filter(|(_, wake_up)| wake_up.kind == PushKind::Dismiss)
      .map(|(device, wake_up)| {
        let meeting = wake_up.maybe_meeting.expect("names the meeting");
        (
          device.clone(),
          meeting.meeting_id.to_string(),
          meeting.starts_at.0,
        )
      })
      .collect();
    found.sort();
    found
  }

  #[tokio::test]
  async fn joining_on_one_phone_takes_the_notice_down_on_the_users_phones_and_nobody_elses() {
    let Some(scene) = scene().await else { return };
    let second_phone = phone(&scene.app, &scene.bob).await;
    let starts_at = now_ms() + 6 * 24 * 60 * MINUTE;
    let (id, chat) = scene.schedule(starts_at, None).await;
    run_at(&scene.app, starts_at - 5 * MINUTE).await;
    assert_eq!(
      scene.recorder.taken().await.len(),
      3,
      "Bob's two phones and Carol's"
    );

    let reply = report(&scene, &scene.bob, &id, starts_at).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
    let pushes = scene.recorder.taken().await;
    let mut bob_phones = vec![scene.bob_phone.clone(), second_phone.clone()];
    bob_phones.sort();
    assert_eq!(
      withdrawals(&pushes),
      bob_phones
        .iter()
        .map(|device| (device.clone(), id.clone(), starts_at))
        .collect::<Vec<_>>(),
      "Bob's phones, not Carol's"
    );
    // It names the meeting and nothing else: no conversation, no text.
    for (_, wake_up) in &pushes {
      assert_ne!(wake_up.conversation_id.to_string(), chat);
      assert_eq!(wake_up.maybe_message_id, None);
      assert_eq!(wake_up.maybe_sender_id, None);
    }

    // Carol's report is hers: it doesn't touch Bob's notice and only goes to her phones.
    let carols = report(&scene, &scene.carol, &id, starts_at).await;
    assert_eq!(carols.status, StatusCode::NO_CONTENT, "{}", carols.body);
    assert_eq!(
      withdrawals(&scene.recorder.taken().await),
      [(scene.carol_phone.clone(), id.clone(), starts_at)]
    );
  }

  #[tokio::test]
  async fn reporting_again_does_nothing_more() {
    let Some(scene) = scene().await else { return };
    let starts_at = now_ms() + 7 * 24 * 60 * MINUTE;
    let (id, _) = scene.schedule(starts_at, None).await;
    run_at(&scene.app, starts_at - 5 * MINUTE).await;
    scene.recorder.taken().await;

    let (first, second) = tokio::join!(
      report(&scene, &scene.bob, &id, starts_at),
      report(&scene, &scene.bob, &id, starts_at)
    );
    assert_eq!(first.status, StatusCode::NO_CONTENT, "{}", first.body);
    assert_eq!(second.status, StatusCode::NO_CONTENT, "{}", second.body);
    assert_eq!(
      withdrawals(&scene.recorder.taken().await).len(),
      1,
      "one withdrawal"
    );

    let third = report(&scene, &scene.bob, &id, starts_at).await;
    assert_eq!(third.status, StatusCode::NO_CONTENT, "{}", third.body);
    assert!(scene.recorder.taken().await.is_empty());
  }

  #[tokio::test]
  async fn a_notice_that_comes_later_can_be_dismissed_in_its_turn() {
    let Some(scene) = scene().await else { return };
    let starts_at = now_ms() + 8 * 24 * 60 * MINUTE;
    let (id, _) = scene.schedule(starts_at, None).await;
    run_at(&scene.app, starts_at - 5 * MINUTE).await;
    report(&scene, &scene.bob, &id, starts_at).await;
    scene.recorder.taken().await;

    // The start notice is a new row, still open: it shows, and dismissing it works.
    run_at(&scene.app, starts_at).await;
    assert_eq!(
      kinds(&scene.recorder.taken().await),
      [(scene.bob_phone.clone(), PushKind::MeetingStart)]
    );
    let reply = report(&scene, &scene.bob, &id, starts_at).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
    assert_eq!(withdrawals(&scene.recorder.taken().await).len(), 1);
  }

  #[tokio::test]
  async fn only_an_occurrence_the_user_was_notified_of_can_be_named() {
    let Some(scene) = scene().await else { return };
    let dave = member("dave");
    phone(&scene.app, &dave).await;
    let starts_at = now_ms() + 9 * 24 * 60 * MINUTE;
    let (id, _) = scene.schedule(starts_at, None).await;

    // Nothing was sent yet, not even to the invited.
    let early = report(&scene, &scene.bob, &id, starts_at).await;
    assert_eq!(early.status, StatusCode::NOT_FOUND, "{}", early.body);

    run_at(&scene.app, starts_at - 5 * MINUTE).await;
    scene.recorder.taken().await;
    // Somebody who is not invited, and an occurrence that never was.
    let stranger = report(&scene, &dave, &id, starts_at).await;
    assert_eq!(stranger.status, StatusCode::NOT_FOUND, "{}", stranger.body);
    let other_time = report(&scene, &scene.bob, &id, starts_at + MINUTE).await;
    assert_eq!(other_time.status, StatusCode::NOT_FOUND, "{}", other_time.body);
    let other_meeting = report(&scene, &scene.bob, "1", starts_at).await;
    assert_eq!(
      other_meeting.status,
      StatusCode::NOT_FOUND,
      "{}",
      other_meeting.body
    );
    assert!(scene.recorder.taken().await.is_empty(), "nothing went out");
  }

  #[tokio::test]
  async fn a_bad_body_is_refused() {
    let Some(scene) = scene().await else { return };
    let starts_at = now_ms() + 10 * 24 * 60 * MINUTE;
    let (id, _) = scene.schedule(starts_at, None).await;
    run_at(&scene.app, starts_at - 5 * MINUTE).await;
    let reply = scene
      .app
      .post(
        &scene.bob,
        &format!("/meetings/{id}/notices/dismiss"),
        json!({"starts_at": "soon"}),
      )
      .await;
    assert!(reply.status.is_client_error(), "{}", reply.body);
  }
}
