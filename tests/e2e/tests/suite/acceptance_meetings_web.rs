//! v1 acceptance, PLAN.md §9 and §12: an external guest follows a personal link in a browser,
//! waits in the lobby and is let in by the host, whose device then adds the guest to the meeting
//! chat's encryption (e2ee.md §4.6, "Meeting guests").
use std::time::{Duration, Instant};

use reqwest::StatusCode;
use serde_json::json;
use teamcraft_e2e::browser::{BrowserSession, web_dir};
use teamcraft_e2e::{Mailpit, Peer, Stack};
use teamcraft_proto::{Base64Bytes, ConversationId, MeetingId, UserId};

use crate::web::{click_text, type_until_kept, wait_for_element, wait_for_text};

const STARTS_AT: i64 = 1_893_488_400_000;
const ENDS_AT: i64 = STARTS_AT + 3_600_000;
/// How long after the meeting's end the guest keeps access (the server's `guest_grace_secs`, as the
/// host's app passes it).
const GUEST_ACCESS_GRACE_MS: i64 = 2 * 60 * 60_000;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_guest_joins_a_meeting_through_the_web_lobby() {
  let Some(web_dir) = web_dir() else {
    let is_required = std::env::var("E2E_REQUIRED").is_ok_and(|value| value == "1");
    assert!(!is_required, "no web build: run tests/e2e/web/build.sh");
    eprintln!("skipping: no web build (tests/e2e/web/build.sh)");
    return;
  };
  let Some(stack) = Stack::builder()
    .with_meetings()
    .with_web_dir(web_dir)
    .start()
    .await
  else {
    return;
  };
  let Some(browser) = BrowserSession::launch().await else {
    return;
  };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let address = format!("gus-{}@example.org", alice.client.device_id());
  let meeting = stack
    .post(
      &alice.tokens,
      "/meetings",
      json!({
        "title": "Customer call", "starts_at": STARTS_AT, "ends_at": ENDS_AT,
        "has_lobby": true,
        "invitees": [{"kind": "external", "email": address, "name": "Gus"}],
      }),
    )
    .await
    .expect(StatusCode::CREATED);
  let meeting_id: MeetingId = meeting.id().parse().unwrap();
  let chat: ConversationId = meeting.body["conversation_id"].as_str().unwrap().parse().unwrap();
  let mail = Mailpit::new()
    .wait_for_mail_to(&address, Duration::from_secs(20))
    .await;
  let invite = mail
    .text
    .split_whitespace()
    .find(|word| word.contains("#invite="))
    .unwrap_or_else(|| panic!("no personal link in the mail:\n{}", mail.text))
    .to_owned();
  assert!(invite.contains(&format!("/meeting/{meeting_id}")), "{invite}");

  // The organizer's app sets up the meeting chat and hands out the link with its device's link
  // secret and the organizer's key fingerprint in the fragment, which no server sees.
  alice.client.set_up_meeting_chat(chat, &[]).await.unwrap();
  alice.client.prepare_guest_link(chat, meeting_id).await.unwrap();
  let fragment = teamcraft_core::client::guest_link_fragment(alice.store.as_ref(), meeting_id).unwrap();
  let link = format!("{invite}&{fragment}");

  let origin = stack.base_url.clone();
  let tokens = alice.tokens.clone();
  let lobby_path = format!("/meetings/{meeting_id}/lobby");
  let stack_ref = &stack;
  let alice_ref = &mut alice;
  browser
    .screenshot_on_failure("meeting-guest-lobby", |page| async move {
      // The UI follows the browser language; pin English (same origin, before the app loads).
      page.goto(format!("{origin}/healthz")).await.unwrap();
      page
        .evaluate("localStorage.setItem('teamcraft.locale', 'en')")
        .await
        .unwrap();
      page.goto(link).await.unwrap();
      wait_for_text(&page, "Join the meeting").await;
      wait_for_element(&page, "input.input").await;
      type_until_kept(&page, "input.input", "Gus (customer)").await;
      click_text(&page, "Ask to join").await;
      wait_for_text(&page, "Waiting for the host").await;

      // The host sees the guest in the lobby, with the proof of the link secret once the guest's
      // page has set up its device.
      let deadline = Instant::now() + Duration::from_secs(60);
      let entry = loop {
        let lobby = stack_ref.get(&tokens, &lobby_path).await.expect(StatusCode::OK);
        let entry = lobby.body["entries"][0].clone();
        if entry.get("proof").is_some() {
          break entry;
        }
        assert!(
          Instant::now() < deadline,
          "the guest never proved the link: {}",
          lobby.body
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
      };
      assert_eq!(entry["user"]["display_name"], "Gus (customer)");
      let guest: UserId = entry["user"]["id"].as_str().unwrap().parse().unwrap();
      let proof: Base64Bytes = serde_json::from_value(entry["proof"].clone()).unwrap();

      // Admitting makes the guest a member; the host's device checks the proof and adds the guest's
      // device to the chat's group, and the guest's page opens the meeting.
      stack_ref
        .post(&tokens, &format!("{lobby_path}/{guest}/admit"), json!({}))
        .await
        .expect(StatusCode::NO_CONTENT);
      alice_ref
        .client
        .admit_guest(
          chat,
          meeting_id,
          guest,
          "Gus (customer)",
          Some(proof.into_vec()),
          ENDS_AT + GUEST_ACCESS_GRACE_MS,
        )
        .await
        .unwrap();
      wait_for_text(&page, "Join the call").await;
    })
    .await;

  browser.close().await;
  stack.stop().await;
}
