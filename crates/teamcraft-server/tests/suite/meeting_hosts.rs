//! Co-hosts and handing a meeting over (#1720): the organizer's signed host lists, what each role
//! may do with them, and the handover's calendar and invitee changes, against a real server and
//! Postgres (`--features integration`, needs `DATABASE_URL`).
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use common::*;
use rand_core::OsRng;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_crypto::identity::IdentityKeyPair;
use teamcraft_crypto::ids::{ConversationId, MeetingId, UserId};
use teamcraft_crypto::meeting_hosts::{Host, HostsDraft};
use teamcraft_proto::{PushKind, PushProvider, PushWakeUp};
use teamcraft_server::auth::BoxFuture;
use teamcraft_server::config::{MailTexts, MeetingsConfig};
use teamcraft_server::push::crypto::open_wakeup;
use teamcraft_server::push::{PushDispatcher, PushError, PushMessage, PushSender, PushTarget};
use teamcraft_server::testing::{FakeCalendar, FakeMailer, TestApp, TestUser};

const BASE_URL: &str = "https://chat.example.com";
const STARTS_AT: i64 = 1_893_488_400_000;
const ENDS_AT: i64 = STARTS_AT + 3_600_000;
const PUSH_KEY_BYTES: [u8; 32] = [9; 32];

struct Rig {
  app: TestApp,
  calendar: Arc<FakeCalendar>,
  mailer: Arc<FakeMailer>,
}

async fn start() -> Option<Rig> {
  let calendar = Arc::new(FakeCalendar::default());
  let mailer = Arc::new(FakeMailer::default());
  let app = TestApp::start_with_meetings(&config(), calendar.clone(), mailer.clone()).await?;
  Some(Rig {
    app,
    calendar,
    mailer,
  })
}

fn config() -> MeetingsConfig {
  MeetingsConfig {
    public_base_url: BASE_URL.into(),
    organizer_fallback_address: "meetings@example.com".into(),
    caldav: None,
    max_invitees: 20,
    guest_token_secret: "0123456789abcdef0123456789abcdef".to_owned().into(),
    guest_grace_secs: 7200,
    max_shared_link_guests: 5,
    start_ring_lead_secs: 3600,
    maybe_inbox_token: None,
    smtp: None,
    mail_texts: MailTexts::default(),
  }
}

macro_rules! rig {
  () => {
    match start().await {
      Some(rig) => rig,
      None => return,
    }
  };
}

/// A meeting of Alice with Bob and Carol, and their keys and IDs.
struct Meeting {
  id: String,
  chat: String,
  alice: Person,
  bob: Person,
  carol: Person,
}

struct Person {
  user: TestUser,
  id: String,
  key: IdentityKeyPair,
}

#[tokio::test]
async fn each_role_may_do_what_the_host_list_gives_it() {
  let rig = rig!();
  let app = &rig.app;
  let meeting = rig.meeting().await;
  let (alice, bob, carol) = (&meeting.alice, &meeting.bob, &meeting.carol);
  let eve = member("eve");
  let path = format!("/conversations/{}/meeting-hosts", meeting.chat);
  let empty = app.get(&bob.user, &path).await;
  assert_eq!(empty.status, StatusCode::OK, "{}", empty.body);
  assert_eq!(empty.body["statements"], json!([]));

  // The organizer names Bob co-host.
  let list = signed(&meeting, alice, alice, &[bob], 10);
  let put = app.put(&alice.user, &path, json!({"statement": list})).await;
  assert_eq!(put.status, StatusCode::NO_CONTENT, "{}", put.body);
  let shown = app.get(&carol.user, &format!("/meetings/{}", meeting.id)).await;
  assert_eq!(shown.body["co_host_user_ids"], json!([bob.id]));

  // A guest waits.
  let token = rig.link_of(&alice.user, &meeting.id).await;
  let joined = rig.join(&token, "Gus").await;
  let guest = joined["user_id"].as_str().unwrap().to_owned();
  let lobby = format!("/meetings/{}/lobby", meeting.id);

  // Organizer and co-host: the lobby, admitting, guest links, a new link.
  for host in [alice, bob] {
    let list = app.get(&host.user, &lobby).await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.body);
    assert_eq!(list.body["is_host"], true);
    let link = app
      .post(
        &host.user,
        &format!("/meetings/{}/guest-links", meeting.id),
        json!({}),
      )
      .await;
    assert_eq!(link.status, StatusCode::CREATED, "{}", link.body);
    let shared = app
      .put(
        &host.user,
        &format!("/meetings/{}/shared-guest-link", meeting.id),
        json!({}),
      )
      .await;
    assert_eq!(shared.status, StatusCode::OK, "{}", shared.body);
  }
  let renewed = app
    .post(&bob.user, &format!("/meetings/{}/link", meeting.id), json!({}))
    .await;
  assert_eq!(renewed.status, StatusCode::OK, "{}", renewed.body);
  let listed = app.get(&bob.user, &format!("/meetings?from={STARTS_AT}")).await;
  assert_eq!(listed.body["meetings"][0]["can_host_lobby"], true);

  // The co-host admits; but changing, cancelling and naming hosts stay with the organizer.
  let admit = app
    .post(&bob.user, &format!("{lobby}/{guest}/admit"), json!({}))
    .await;
  assert_eq!(admit.status, StatusCode::NO_CONTENT, "{}", admit.body);
  let patch = app
    .patch(
      &bob.user,
      &format!("/meetings/{}", meeting.id),
      json!({"title": "Mine"}),
    )
    .await;
  assert_eq!(patch.status, StatusCode::FORBIDDEN);
  assert_eq!(
    app
      .delete(&bob.user, &format!("/meetings/{}", meeting.id))
      .await
      .status,
    StatusCode::FORBIDDEN
  );
  let own_list = signed(&meeting, bob, bob, &[carol], 20);
  assert_eq!(
    app
      .put(&bob.user, &path, json!({"statement": own_list}))
      .await
      .status,
    StatusCode::FORBIDDEN
  );

  // A member: neither the lobby nor links nor host lists.
  assert_eq!(app.get(&carol.user, &lobby).await.status, StatusCode::FORBIDDEN);
  assert_eq!(
    app
      .post(
        &carol.user,
        &format!("/meetings/{}/guest-links", meeting.id),
        json!({})
      )
      .await
      .status,
    StatusCode::FORBIDDEN
  );
  assert_eq!(
    app
      .post(&carol.user, &format!("/meetings/{}/link", meeting.id), json!({}))
      .await
      .status,
    StatusCode::FORBIDDEN
  );
  let carol_list = signed(&meeting, carol, carol, &[], 30);
  assert_eq!(
    app
      .put(&carol.user, &path, json!({"statement": carol_list}))
      .await
      .status,
    StatusCode::FORBIDDEN
  );
  // A stranger learns nothing.
  assert_eq!(app.get(&eve, &path).await.status, StatusCode::NOT_FOUND);

  // The admitted guest reads the lists (to check a co-host's welcome), never writes them.
  let guest_token = joined["guest_token"].as_str().unwrap();
  let (status, body) = rig.as_guest(guest_token, reqwest::Method::GET, &path, None).await;
  assert_eq!(status, StatusCode::OK, "{body}");
  assert_eq!(body["statements"], json!([list]));
  let (status, _) = rig
    .as_guest(
      guest_token,
      reqwest::Method::PUT,
      &path,
      Some(json!({"statement": list})),
    )
    .await;
  assert_eq!(status, StatusCode::FORBIDDEN);
  let (status, _) = rig
    .as_guest(
      guest_token,
      reqwest::Method::POST,
      &format!("/meetings/{}/guest-links", meeting.id),
      Some(json!({})),
    )
    .await;
  assert_eq!(status, StatusCode::FORBIDDEN);

  // Taken off the list, Bob hosts nothing any more.
  let without_bob = signed(&meeting, alice, alice, &[], 40);
  assert_eq!(
    app
      .put(&alice.user, &path, json!({"statement": without_bob}))
      .await
      .status,
    StatusCode::NO_CONTENT
  );
  assert_eq!(app.get(&bob.user, &lobby).await.status, StatusCode::FORBIDDEN);
  let audit: Vec<String> = teamcraft_db::testing::sqlx::query_scalar(
    "SELECT action FROM audit_log WHERE target_kind = 'meeting' AND target_id = $1 ORDER BY id",
  )
  .bind(meeting.id.parse::<i64>().unwrap())
  .fetch_all(app.sql_pool())
  .await
  .unwrap();
  assert_eq!(audit, vec!["meeting_hosts", "meeting_hosts"]);
  rig.app.stop().await;
}

#[tokio::test]
async fn the_server_keeps_the_handovers_and_the_newest_and_refuses_bad_lists() {
  let rig = rig!();
  let app = &rig.app;
  let meeting = rig.meeting().await;
  let (alice, bob, carol) = (&meeting.alice, &meeting.bob, &meeting.carol);
  let path = format!("/conversations/{}/meeting-hosts", meeting.chat);
  let put = |statement: String| {
    let (app, alice, path) = (&rig.app, alice.user.clone(), path.clone());
    async move {
      app
        .put(&alice, &path, json!({"statement": statement}))
        .await
        .status
    }
  };
  assert_eq!(
    put(signed(&meeting, alice, alice, &[bob], 5)).await,
    StatusCode::NO_CONTENT
  );
  for sequence in [5, 4] {
    assert_eq!(
      put(signed(&meeting, alice, alice, &[carol], sequence)).await,
      StatusCode::CONFLICT,
      "{sequence}"
    );
  }

  // Signed for another user, naming a stranger, a broken signature, garbage.
  let eve = member("eve");
  let eve_id = user_id(app, &eve).await;
  let stranger = Person {
    user: eve,
    id: eve_id,
    key: IdentityKeyPair::generate(&mut OsRng),
  };
  let mut tampered = STANDARD
    .decode(signed(&meeting, alice, alice, &[bob], 9))
    .unwrap();
  let last = tampered.len() - 1;
  tampered[last] ^= 1;
  for (bad, why) in [
    (signed(&meeting, bob, bob, &[], 9), "signed as Bob"),
    (signed(&meeting, alice, alice, &[&stranger], 9), "a stranger"),
    (signed(&meeting, alice, &stranger, &[], 9), "handed to a stranger"),
    (STANDARD.encode(tampered), "a broken signature"),
    (STANDARD.encode([1, 2, 3]), "garbage"),
  ] {
    assert_eq!(put(bad).await, StatusCode::BAD_REQUEST, "{why}");
  }

  // A handover is kept beside the newest list; plain older lists are not.
  let handover = signed(&meeting, alice, carol, &[alice], 20);
  assert_eq!(put(handover.clone()).await, StatusCode::NO_CONTENT);
  let carol_put = |statement: String| {
    let (app, carol, path) = (&rig.app, carol.user.clone(), path.clone());
    async move {
      app
        .put(&carol, &path, json!({"statement": statement}))
        .await
        .status
    }
  };
  assert_eq!(
    carol_put(signed(&meeting, carol, carol, &[alice], 21)).await,
    StatusCode::NO_CONTENT
  );
  let newest = signed(&meeting, carol, carol, &[alice, bob], 22);
  assert_eq!(carol_put(newest.clone()).await, StatusCode::NO_CONTENT);
  let read = app.get(&bob.user, &path).await;
  assert_eq!(read.body["statements"], json!([handover, newest]));
  rig.app.stop().await;
}

#[tokio::test]
async fn a_handover_makes_the_new_organizer_and_keeps_the_former_as_co_host() {
  let rig = rig!();
  let app = &rig.app;
  let meeting = rig.meeting().await;
  let (alice, bob, carol) = (&meeting.alice, &meeting.bob, &meeting.carol);
  let path = format!("/conversations/{}/meeting-hosts", meeting.chat);
  let sequence_before = rig.calendar_sequence();
  let mails_before = rig.mailer.sent().len();

  let handover = signed(&meeting, alice, bob, &[alice], 7);
  let put = app.put(&alice.user, &path, json!({"statement": handover})).await;
  assert_eq!(put.status, StatusCode::NO_CONTENT, "{}", put.body);

  let shown = app.get(&carol.user, &format!("/meetings/{}", meeting.id)).await;
  assert_eq!(shown.body["organizer_user_id"], bob.id.as_str());
  assert_eq!(shown.body["co_host_user_ids"], json!([alice.id]));
  let attendees: Vec<Value> = shown.body["attendees"].as_array().unwrap().clone();
  assert!(
    attendees
      .iter()
      .any(|attendee| attendee["user_id"] == alice.id.as_str() && attendee["rsvp"] == "accepted"),
    "the former organizer is an invitee who accepted: {attendees:?}"
  );
  assert!(
    !attendees
      .iter()
      .any(|attendee| attendee["user_id"] == bob.id.as_str()),
    "the organizer is no invitee"
  );
  // The calendar names the new organizer with a higher sequence; the invitees hear of it.
  assert!(rig.calendar_sequence() > sequence_before);
  assert!(rig.calendar_event().contains("bob@example.com"));
  assert!(rig.mailer.sent().len() > mails_before);

  // Bob changes the meeting now, Alice doesn't, but she still hosts the lobby.
  let patch = app
    .patch(
      &bob.user,
      &format!("/meetings/{}", meeting.id),
      json!({"title": "Bob's"}),
    )
    .await;
  assert_eq!(patch.status, StatusCode::OK, "{}", patch.body);
  assert_eq!(
    app
      .patch(
        &alice.user,
        &format!("/meetings/{}", meeting.id),
        json!({"title": "Alice's"})
      )
      .await
      .status,
    StatusCode::FORBIDDEN
  );
  assert_eq!(
    app
      .put(
        &alice.user,
        &path,
        json!({"statement": signed(&meeting, alice, alice, &[], 8)})
      )
      .await
      .status,
    StatusCode::FORBIDDEN
  );
  let lobby = app
    .get(&alice.user, &format!("/meetings/{}/lobby", meeting.id))
    .await;
  assert_eq!(lobby.body["is_host"], true);
  let audit: Vec<String> = teamcraft_db::testing::sqlx::query_scalar(
    "SELECT action FROM audit_log WHERE target_kind = 'meeting' AND target_id = $1 ORDER BY id",
  )
  .bind(meeting.id.parse::<i64>().unwrap())
  .fetch_all(app.sql_pool())
  .await
  .unwrap();
  assert!(audit.contains(&"meeting_handover".to_owned()), "{audit:?}");
  rig.app.stop().await;
}

#[tokio::test]
async fn new_hosts_hear_of_it_by_push_and_the_organizer_does_not() {
  let recorder = Recorder::default();
  let Some(app) = TestApp::start_with_meetings_and_push(
    &config(),
    Arc::new(FakeCalendar::default()),
    Arc::new(FakeMailer::default()),
    PushDispatcher::new(8).with_sender(&PushProvider::Fcm, Arc::new(recorder.clone())),
  )
  .await
  else {
    return;
  };
  let rig = Rig {
    app,
    calendar: Arc::new(FakeCalendar::default()),
    mailer: Arc::new(FakeMailer::default()),
  };
  let meeting = rig.meeting().await;
  let (alice, bob, carol) = (&meeting.alice, &meeting.bob, &meeting.carol);
  let alice_phone = phone(&rig.app, &alice.user).await;
  let bob_phone = phone(&rig.app, &bob.user).await;
  let carol_phone = phone(&rig.app, &carol.user).await;
  let path = format!("/conversations/{}/meeting-hosts", meeting.chat);

  let named = signed(&meeting, alice, alice, &[carol], 1);
  let put = rig.app.put(&alice.user, &path, json!({"statement": named})).await;
  assert_eq!(put.status, StatusCode::NO_CONTENT, "{}", put.body);
  let pushes = recorder.taken().await;
  assert_eq!(
    kinds(&pushes),
    [(carol_phone.clone(), PushKind::MeetingCoHost)],
    "the new co-host only"
  );
  let (_, wake_up) = &pushes[0];
  assert_eq!(wake_up.conversation_id.to_string(), meeting.chat);
  assert_eq!(
    wake_up.maybe_meeting.map(|notice| notice.meeting_id.to_string()),
    Some(meeting.id.clone())
  );
  assert_eq!(wake_up.maybe_message_id, None, "no text, no message");

  // The same co-host again: nothing new to tell.
  let again = signed(&meeting, alice, alice, &[carol], 2);
  rig.app.put(&alice.user, &path, json!({"statement": again})).await;
  assert!(recorder.taken().await.is_empty());

  // A handover: the new organizer hears of it; the former one, now co-host, made it.
  let handover = signed(&meeting, alice, bob, &[alice, carol], 3);
  let put = rig
    .app
    .put(&alice.user, &path, json!({"statement": handover}))
    .await;
  assert_eq!(put.status, StatusCode::NO_CONTENT, "{}", put.body);
  let kinds = kinds(&recorder.taken().await);
  assert_eq!(kinds, [(bob_phone, PushKind::MeetingHandover)]);
  assert!(!kinds.iter().any(|(device, _)| *device == alice_phone));
  rig.app.stop().await;
}

/// Records the wake-ups the server sends, opened.
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

impl Rig {
  /// Alice schedules a meeting with Bob and Carol.
  async fn meeting(&self) -> Meeting {
    let (alice, bob, carol) = (owner(), member("bob"), member("carol"));
    let person = |user: TestUser| {
      let app = &self.app;
      async move {
        let id = user_id(app, &user).await;
        Person {
          user,
          id,
          key: IdentityKeyPair::generate(&mut OsRng),
        }
      }
    };
    let (alice, bob, carol) = (person(alice).await, person(bob).await, person(carol).await);
    let reply = self
      .app
      .post(
        &alice.user,
        "/meetings",
        json!({
          "title": "Planning", "starts_at": STARTS_AT, "ends_at": ENDS_AT, "has_lobby": true,
          "invitees": [{"kind": "user", "user_id": bob.id}, {"kind": "user", "user_id": carol.id}],
        }),
      )
      .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    Meeting {
      id: reply.id(),
      chat: reply.body["conversation_id"].as_str().unwrap().to_owned(),
      alice,
      bob,
      carol,
    }
  }

  async fn link_of(&self, organizer: &TestUser, meeting: &str) -> String {
    let reply = self.app.get(organizer, &format!("/meetings/{meeting}")).await;
    let url = reply.body["join_url"].as_str().unwrap();
    url
      .split("/meeting/")
      .nth(1)
      .unwrap()
      .split('#')
      .next()
      .unwrap()
      .to_owned()
  }

  async fn join(&self, token: &str, name: &str) -> Value {
    let (status, body) = self
      .as_guest(
        "",
        reqwest::Method::POST,
        "/meetings/join",
        Some(json!({"link_token": token, "display_name": name})),
      )
      .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
  }

  /// A request with a guest's session token (none when empty).
  async fn as_guest(
    &self,
    token: &str,
    method: reqwest::Method,
    path: &str,
    maybe_body: Option<Value>,
  ) -> (StatusCode, Value) {
    let mut request = self
      .app
      .http
      .request(method, format!("{}/api/v1{path}", self.app.base_url));
    if !token.is_empty() {
      request = request.bearer_auth(token);
    }
    if let Some(body) = maybe_body {
      request = request.json(&body);
    }
    let response = request.send().await.unwrap();
    let status = response.status();
    (status, response.json().await.unwrap_or(Value::Null))
  }

  fn calendar_event(&self) -> String {
    let name = self.calendar.names().into_iter().next().unwrap();
    self.calendar.event(&name).unwrap()
  }

  fn calendar_sequence(&self) -> u32 {
    self
      .calendar_event()
      .lines()
      .find_map(|line| line.strip_prefix("SEQUENCE:"))
      .and_then(|value| value.trim().parse().ok())
      .unwrap_or(0)
  }
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

fn kinds(pushes: &[(String, PushWakeUp)]) -> Vec<(String, PushKind)> {
  let mut kinds: Vec<_> = pushes
    .iter()
    .map(|(device, wake_up)| (device.clone(), wake_up.kind.clone()))
    .collect();
  kinds.sort_by(|a, b| a.0.cmp(&b.0));
  kinds
}

/// A host list for the meeting, signed by `signer`'s key as `signer`, naming `organizer` and the
/// co-hosts, base64.
fn signed(
  meeting: &Meeting,
  signer: &Person,
  organizer: &Person,
  co_hosts: &[&Person],
  sequence: u64,
) -> String {
  let host = |person: &Person| Host {
    user_id: UserId::new(person.id.parse().unwrap()),
    identity_key: person.key.public_key(),
  };
  let list = HostsDraft {
    meeting_id: MeetingId::new(meeting.id.parse().unwrap()),
    conversation_id: ConversationId::new(meeting.chat.parse().unwrap()),
    sequence,
    signer_user_id: UserId::new(signer.id.parse().unwrap()),
    organizer: host(organizer),
    co_hosts: co_hosts.iter().map(|person| host(person)).collect(),
  }
  .sign(&signer.key)
  .unwrap();
  STANDARD.encode(list.to_bytes().unwrap())
}
