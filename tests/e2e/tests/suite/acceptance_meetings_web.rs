//! v1 acceptance, PLAN.md §9 and §12: an external guest follows the personal link from the
//! invitation mail in a browser, waits in the lobby and is let in by the host.
use std::time::Duration;

use reqwest::StatusCode;
use serde_json::json;
use teamcraft_e2e::browser::{BrowserSession, web_dir};
use teamcraft_e2e::{Mailpit, Peer, Stack};

use crate::web::{click_text, wait_for_element, wait_for_text};

const STARTS_AT: i64 = 1_893_488_400_000;

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
  let alice = Peer::enroll(&stack, "alice").await;
  let address = format!("gus-{}@example.org", alice.client.device_id());
  let meeting = stack
    .post(
      &alice.tokens,
      "/meetings",
      json!({
        "title": "Customer call", "starts_at": STARTS_AT, "ends_at": STARTS_AT + 3_600_000,
        "has_lobby": true,
        "invitees": [{"kind": "external", "email": address, "name": "Gus"}],
      }),
    )
    .await
    .expect(StatusCode::CREATED);
  let meeting_id = meeting.id();
  let mail = Mailpit::new()
    .wait_for_mail_to(&address, Duration::from_secs(20))
    .await;
  let link = mail
    .text
    .split_whitespace()
    .find(|word| word.contains("#invite="))
    .unwrap_or_else(|| panic!("no personal link in the mail:\n{}", mail.text))
    .to_owned();
  assert!(link.contains(&format!("/meeting/{meeting_id}")), "{link}");

  let origin = stack.base_url.clone();
  let tokens = alice.tokens.clone();
  let admit_path = format!("/meetings/{meeting_id}/lobby");
  let stack_ref = &stack;
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
      let name = wait_for_element(&page, "input.input").await;
      name
        .click()
        .await
        .unwrap()
        .type_str("Gus (customer)")
        .await
        .unwrap();
      click_text(&page, "Ask to join").await;
      wait_for_text(&page, "Waiting for the host").await;

      // The host sees the guest in the lobby and lets them in.
      let lobby = stack_ref.get(&tokens, &admit_path).await.expect(StatusCode::OK);
      let guest_id = lobby.body["entries"][0]["user"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
      assert_eq!(lobby.body["entries"][0]["user"]["display_name"], "Gus (customer)");
      stack_ref
        .post(&tokens, &format!("{admit_path}/{guest_id}/admit"), json!({}))
        .await
        .expect(StatusCode::NO_CONTENT);
      wait_for_text(&page, "You are in").await;
    })
    .await;

  browser.close().await;
  stack.stop().await;
}
