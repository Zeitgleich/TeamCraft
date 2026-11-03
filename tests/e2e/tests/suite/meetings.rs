//! Meetings: scheduling sends iMIP invitations (caught by Mailpit), an external guest joins with the
//! mailed meeting link, waits in the lobby and is admitted; a personal one-time link works once.

use std::time::Duration;

use reqwest::{Method, StatusCode};
use serde_json::json;
use teamcraft_e2e::{Mailpit, Peer, Stack};

const MAIL_TIMEOUT: Duration = Duration::from_secs(20);
/// 2030-01-01T09:00:00Z and one hour later.
const STARTS_AT: i64 = 1_893_488_400_000;
const ENDS_AT: i64 = STARTS_AT + 3_600_000;

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
