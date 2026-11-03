//! Meetings: scheduling sends iMIP invitations (caught by Mailpit), an external guest joins with the
//! mailed meeting link, waits in the lobby and is admitted; a personal one-time link works once.

use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::{Method, StatusCode};
use serde_json::json;
use teamcraft_core::api::{AccessTokenSource, ApiError, HttpServerApi};
use teamcraft_core::client::NewDeviceLink;
use teamcraft_core::clock::SystemClock;
use teamcraft_core::error::CoreError;
use teamcraft_core::store::MemoryStore;
use teamcraft_core::{ClientSettings, EnrollmentOptions, EnrollmentSecret, LinkOffer, PendingEnrollment};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_e2e::peer::enrollment_options;
use teamcraft_e2e::{Mailpit, Peer, Stack};
use teamcraft_proto::{ConversationId, UserId};

const MAIL_TIMEOUT: Duration = Duration::from_secs(20);
/// 2030-01-01T09:00:00Z and one hour later.
const STARTS_AT: i64 = 1_893_488_400_000;
const ENDS_AT: i64 = STARTS_AT + 3_600_000;

/// A guest session's token for a headless guest client.
#[derive(Clone)]
struct GuestToken(String);

impl AccessTokenSource for GuestToken {
  fn access_token(&self) -> Result<String, ApiError> {
    Ok(self.0.clone())
  }
}

/// A call started before the meeting chat's group is set up (#796), against the real server's
/// routes and guest scope: invitee and guest are told at once, and their retry gets in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_before_the_chat_is_set_up_is_refused_clearly_and_the_retry_gets_in() {
  let Some(stack) = Stack::builder().with_meetings().with_calls().start().await else {
    return;
  };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut carol = Peer::enroll(&stack, "carol").await;
  let meeting = stack
    .post(
      &alice.tokens,
      "/meetings",
      json!({
        "title": "Early call",
        "starts_at": STARTS_AT,
        "ends_at": ENDS_AT,
        "has_lobby": true,
        "invitees": [{"kind": "user", "user_id": carol.user_id().to_string()}],
      }),
    )
    .await
    .expect(StatusCode::CREATED);
  let meeting_id = meeting.id();
  let chat: ConversationId = meeting.body["conversation_id"].as_str().unwrap().parse().unwrap();
  let link_token = meeting.body["join_url"]
    .as_str()
    .and_then(|url| url.split("/meeting/").nth(1))
    .and_then(|rest| rest.split('#').next())
    .unwrap()
    .to_owned();

  let early = carol.client.prepare_call(chat).await;
  assert!(
    matches!(early, Err(CoreError::CallEncryptionPending(id)) if id == chat),
    "the invitee is told at once: {:?}",
    early.err()
  );

  let carol_id = carol.user_id();
  assert!(alice.client.set_up_meeting_chat(chat, &[carol_id]).await.unwrap());
  let prepared = carol.client.prepare_call(chat).await.unwrap();
  assert_eq!(prepared.devices.len(), 2, "the invitee's retry gets in");

  let joined = stack
    .request_with(
      None,
      Method::POST,
      "/meetings/join",
      Some(json!({"link_token": link_token, "display_name": "Gus"})),
    )
    .await
    .expect(StatusCode::OK);
  let guest_token = joined.body["guest_token"].as_str().unwrap().to_owned();
  let guest_id: UserId = joined.body["user_id"].as_str().unwrap().parse().unwrap();
  let mut guest = PendingEnrollment::complete_as_guest(
    HttpServerApi::new(stack.base_url.clone(), GuestToken(guest_token)),
    Arc::new(MemoryStore::new()),
    Box::new(SystemClock),
    ClientSettings::default(),
    "Browser".into(),
    Argon2Settings {
      memory_kib: 1024,
      iterations: 1,
      parallelism: 1,
    },
  )
  .await
  .unwrap();
  let anchor = alice.client.own_fingerprint();
  guest
    .set_guest_organizer_anchor(chat, meeting_id.parse().unwrap(), &anchor)
    .unwrap();
  stack
    .post(
      &alice.tokens,
      &format!("/meetings/{meeting_id}/lobby/{guest_id}/admit"),
      json!({}),
    )
    .await
    .expect(StatusCode::NO_CONTENT);

  // The server made the guest a member; the organizer's device hasn't let them in yet.
  let early = guest.prepare_call(chat).await;
  assert!(
    matches!(early, Err(CoreError::CallEncryptionPending(id)) if id == chat),
    "the guest is told at once: {:?}",
    early.err()
  );

  let code = guest.own_fingerprint();
  alice
    .client
    .let_guest_into_meeting_chat(chat, guest_id, &code, ENDS_AT)
    .await
    .unwrap();
  let prepared = guest.prepare_call(chat).await.unwrap();
  assert_eq!(prepared.devices.len(), 3, "the guest's retry gets in");
}

/// The organizer's phone founded the chat's group and waits for an invitee who hasn't set up the
/// app, and is closed. The laptop joins the group and starts the call; the phone keeps its seat when
/// it comes back (#796).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_founder_follows_its_other_device_while_an_invitee_has_no_device() {
  let Some(stack) = Stack::builder().with_meetings().with_calls().start().await else {
    return;
  };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let dave = stack
    .get(&stack.tokens("dave"), "/me")
    .await
    .expect(StatusCode::OK);
  let dave_id: UserId = dave.body["id"].as_str().unwrap().parse().unwrap();

  // The laptop waits in the relay first; the phone scans a code that is already shown.
  let laptop_api = HttpServerApi::new(stack.base_url.clone(), alice.tokens.clone());
  let offer = LinkOffer::generate();
  let code = offer.to_code();
  let approve = async {
    tokio::time::sleep(Duration::from_secs(1)).await;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
      match alice.client.start_link_approval(&code).await {
        Err(CoreError::LinkPeerGone) if Instant::now() < deadline => {
          tokio::time::sleep(Duration::from_millis(500)).await;
        },
        outcome => return outcome,
      }
    }
  };
  let (laptop_side, phone_side) = tokio::join!(NewDeviceLink::connect(&laptop_api, &offer), approve);
  let (laptop_side, phone_side) = (laptop_side.unwrap(), phone_side.unwrap());
  let laptop_store = Arc::new(MemoryStore::new());
  let (approved, received) = tokio::join!(
    alice.client.complete_link_approval(phone_side),
    laptop_side.receive_secrets(&laptop_store, "alice linked laptop")
  );
  approved.unwrap();
  let options = EnrollmentOptions {
    secret: EnrollmentSecret::Linked(received.unwrap()),
    ..enrollment_options("alice linked laptop")
  };
  let mut laptop = PendingEnrollment::new()
    .complete(
      laptop_api,
      laptop_store,
      Box::new(SystemClock),
      ClientSettings::default(),
      options,
    )
    .await
    .unwrap();

  let meeting = stack
    .post(
      &alice.tokens,
      "/meetings",
      json!({
        "title": "Early call",
        "starts_at": STARTS_AT,
        "ends_at": ENDS_AT,
        "has_lobby": true,
        "invitees": [{"kind": "user", "user_id": dave_id.to_string()}],
      }),
    )
    .await
    .expect(StatusCode::CREATED);
  let chat: ConversationId = meeting.body["conversation_id"].as_str().unwrap().parse().unwrap();
  alice.client.set_up_meeting_chat(chat, &[dave_id]).await.unwrap();
  assert!(alice.client.has_group(chat).unwrap());

  laptop.full_sync().await.unwrap();
  assert_eq!(laptop.prepare_call(chat).await.unwrap().devices.len(), 2);

  // The phone comes back: a sync, then "Start" (the setup again, then the call).
  alice.client.full_sync().await.unwrap();
  alice.client.set_up_meeting_chat(chat, &[dave_id]).await.unwrap();
  assert_eq!(alice.client.prepare_call(chat).await.unwrap().devices.len(), 2);
  assert_eq!(
    alice.client.mls_epoch(chat).unwrap(),
    laptop.mls_epoch(chat).unwrap(),
    "one group"
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_scheduled_meeting_invites_by_mail_and_an_external_guest_joins_through_the_lobby() {
  let Some(stack) = Stack::builder().with_meetings().start().await else {
    return;
  };
  let mailpit = Mailpit::new();
  let alice = Peer::enroll(&stack, "alice").await;
  let carol = Peer::enroll(&stack, "carol").await;
  // Unique per run: Mailpit keeps every mail of earlier runs.
  let guest_address = format!("gus-{}@example.org", alice.client.device_id());

  let meeting = stack
    .post(
      &alice.tokens,
      "/meetings",
      json!({
        "title": "Customer call",
        "starts_at": STARTS_AT,
        "ends_at": ENDS_AT,
        "has_lobby": true,
        "invitees": [
          {"kind": "user", "user_id": carol.user_id().to_string()},
          {"kind": "external", "email": guest_address, "name": "Gus"},
        ],
      }),
    )
    .await
    .expect(StatusCode::CREATED);
  let meeting_id = meeting.id();
  let chat = meeting.body["conversation_id"].as_str().unwrap().to_owned();

  // The invitation: an iMIP request with the calendar part and the meeting link (#987).
  let mail = mailpit.wait_for_mail_to(&guest_address, MAIL_TIMEOUT).await;
  assert_eq!(mail.subject, "Invitation: Customer call");
  assert!(mail.raw.contains("text/calendar"), "{}", mail.raw);
  assert!(
    mail.raw.contains("method=REQUEST") || mail.raw.contains("METHOD:REQUEST"),
    "{}",
    mail.raw
  );
  let link_token = mail
    .text
    .split("/meeting/")
    .nth(1)
    .and_then(|rest| rest.split(|c: char| c == '#' || c.is_whitespace()).next())
    .unwrap_or_else(|| panic!("no meeting link in the mail:\n{}", mail.text))
    .to_owned();
  assert!(
    !mail.text.contains(&format!("/meeting/{meeting_id}")),
    "the meeting ID is no public handle: {}",
    mail.text
  );

  // The internal invitee sees the meeting in her list and can answer.
  let listed = stack.get(&carol.tokens, "/meetings").await.expect(StatusCode::OK);
  assert!(listed.body.to_string().contains(&meeting_id), "{}", listed.body);
  stack
    .put(
      &carol.tokens,
      &format!("/meetings/{meeting_id}/rsvp"),
      json!({"status": "accepted"}),
    )
    .await
    .expect(StatusCode::NO_CONTENT);

  // The guest trades the link for a session and waits in the lobby.
  let joined = stack
    .request_with(
      None,
      Method::POST,
      "/meetings/join",
      Some(json!({"link_token": link_token, "display_name": "Gus (customer)"})),
    )
    .await
    .expect(StatusCode::OK);
  assert_eq!(joined.body["lobby_state"], "waiting", "{}", joined.body);
  let guest_token = joined.body["guest_token"].as_str().unwrap().to_owned();
  let guest_id = joined.body["user_id"].as_str().unwrap().to_owned();
  // A personal one-time link works once.
  let one_time = stack
    .post(
      &alice.tokens,
      &format!("/meetings/{meeting_id}/guest-links"),
      json!({}),
    )
    .await
    .expect(StatusCode::CREATED);
  let invite_token = one_time.body["join_url"]
    .as_str()
    .and_then(|url| url.split("#invite=").nth(1))
    .and_then(|rest| rest.split('&').next())
    .unwrap()
    .to_owned();
  for (name, should_work) in [("Ida", true), ("Somebody else", false)] {
    let join = stack
      .request_with(
        None,
        Method::POST,
        "/meetings/join",
        Some(json!({"invite_token": invite_token, "display_name": name})),
      )
      .await;
    assert_eq!(join.status.is_success(), should_work, "{join:?}");
  }

  // Until admitted the guest sees neither the chat nor the meeting.
  let before = stack
    .request_with(
      Some(&guest_token),
      Method::GET,
      &format!("/conversations/{chat}"),
      None,
    )
    .await;
  assert_eq!(before.status, StatusCode::NOT_FOUND, "{before:?}");
  let lobby = stack
    .get(&alice.tokens, &format!("/meetings/{meeting_id}/lobby"))
    .await
    .expect(StatusCode::OK);
  assert!(
    lobby.body["entries"]
      .as_array()
      .unwrap()
      .iter()
      .any(|entry| entry["user"]["id"] == guest_id.as_str() && entry["is_by_link"] == true),
    "{}",
    lobby.body
  );

  stack
    .post(
      &alice.tokens,
      &format!("/meetings/{meeting_id}/lobby/{guest_id}/admit"),
      json!({}),
    )
    .await
    .expect(StatusCode::NO_CONTENT);
  let after = stack
    .request_with(
      Some(&guest_token),
      Method::GET,
      &format!("/conversations/{chat}"),
      None,
    )
    .await;
  assert_eq!(after.status, StatusCode::OK, "{after:?}");
  let state = stack
    .request_with(
      Some(&guest_token),
      Method::GET,
      &format!("/meetings/{meeting_id}/lobby/me"),
      None,
    )
    .await;
  assert_eq!(state.body["state"], "admitted", "{}", state.body);

  // Cancelling sends the cancellation to the guest.
  let cancelled = stack
    .delete(&alice.tokens, &format!("/meetings/{meeting_id}"))
    .await
    .expect(StatusCode::OK);
  assert_eq!(cancelled.body["status"], "cancelled");
  let deadline = std::time::Instant::now() + MAIL_TIMEOUT;
  while mailpit.count_to(&guest_address).await < 2 {
    assert!(std::time::Instant::now() < deadline, "no cancellation mail");
    tokio::time::sleep(Duration::from_millis(200)).await;
  }

  stack.stop().await;
}
