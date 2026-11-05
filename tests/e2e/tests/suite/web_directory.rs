//! The directory in the web client (issue #298): the people grid, the Channels tab, and the
//! invitation actions, which talk to the server from an async task and so only prove themselves in a
//! real page.

use std::time::Duration;

use chromiumoxide::page::Page;
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_e2e::browser::{BrowserSession, lease_web_port, prefer_mouse, web_dir};
use teamcraft_e2e::{Peer, Stack};
use tokio::time::{Instant, sleep};

use crate::perf::sign_in_and_unlock;
use crate::web::{click_text, shot, wait_for_element, wait_for_text};

const PORT: u16 = 8084;
const WEB_ORIGIN: &str = "http://127.0.0.1:8084";
const WAIT: Duration = Duration::from_secs(30);
const INVITED: &str = "invited@example.org";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_directory_lists_people_and_channels_and_manages_invitations() {
  let Some(web_dir) = web_dir() else {
    let is_required = std::env::var("E2E_REQUIRED").is_ok_and(|value| value == "1");
    assert!(!is_required, "no web build: run tests/e2e/web/build.sh");
    eprintln!("skipping: no web build (tests/e2e/web/build.sh)");
    return;
  };
  let _turn = lease_web_port(PORT).await;
  let Some(stack) = Stack::builder()
    .with_port(PORT)
    .with_web_dir(web_dir)
    .with_mail()
    // The invitation's own mail has just gone out: the test resends at once.
    .with_env("TEAMCRAFT_LIMITS__INVITE_RESEND_INTERVAL_SECS", "0")
    .start()
    .await
  else {
    return;
  };
  let Some(browser) = BrowserSession::launch().await else {
    return;
  };
  let alice = Peer::enroll(&stack, "alice").await;
  let space = stack.create_space(&alice.tokens, "Project A", "open").await;
  let reply = stack
    .post(
      &alice.tokens,
      &format!("/spaces/{space}/invites"),
      json!({"email": INVITED, "role": "member"}),
    )
    .await
    .expect(StatusCode::CREATED);
  drop(reply);
  stack
    .post(
      &alice.tokens,
      &format!("/spaces/{space}/channels"),
      json!({"name": "open-channel", "visibility": "public"}),
    )
    .await
    .expect(StatusCode::CREATED);

  let (stack_ref, tokens) = (&stack, &alice.tokens);
  browser
    .screenshot_on_failure("directory", |page| async move {
      prefer_mouse(&page).await;
      sign_in_and_unlock(&page, "alice").await;
      page.goto(format!("{WEB_ORIGIN}/people")).await.unwrap();

      wait_for_element(&page, ".person-card").await;
      shot(&page, "directory-people").await;

      click_tab(&page, "Channels").await;
      wait_for_text(&page, "open-channel").await;
      shot(&page, "directory-channels").await;

      click_tab(&page, "Invitations").await;
      wait_for_text(&page, INVITED).await;
      shot(&page, "directory-invitations").await;

      let mails = sent_to(INVITED).await;
      click_text(&page, "Send again").await;
      wait_for_text(&page, "Sent again").await;
      assert_eq!(sent_to(INVITED).await, mails + 1, "the mail went out again");

      click_text(&page, "Revoke").await;
      wait_for_text(&page, "They won't join").await;
      click_confirm_revoke(&page).await;
      wait_until_gone(&page, INVITED).await;
      let left = stack_ref.get(tokens, "/directory/invitations").await;
      assert_eq!(left.body["invitations"], json!([]), "{}", left.body);
    })
    .await;
  browser.close().await;
  stack.stop().await;
}

async fn click_tab(page: &Page, label: &str) {
  let script = format!(
    "[...document.querySelectorAll('[role=tab]')].find(tab => tab.innerText.includes({label:?})).click()"
  );
  page.evaluate(script).await.unwrap();
}

/// The second "Revoke" of the row: the one next to the question.
async fn click_confirm_revoke(page: &Page) {
  page
    .evaluate("document.querySelector('.directory-invitation-actions .btn-danger').click()")
    .await
    .unwrap();
}

async fn wait_until_gone(page: &Page, needle: &str) {
  let deadline = Instant::now() + WAIT;
  loop {
    let text: String = page
      .evaluate("document.body.innerText")
      .await
      .unwrap()
      .into_value()
      .unwrap();
    if !text.contains(needle) {
      return;
    }
    assert!(Instant::now() < deadline, "{needle:?} stays on the page");
    sleep(Duration::from_millis(100)).await;
  }
}

/// How many mails Mailpit caught for the address.
async fn sent_to(address: &str) -> u64 {
  let found: serde_json::Value = reqwest::Client::new()
    .get("http://127.0.0.1:8025/api/v1/search")
    .query(&[("query", format!("to:{address}"))])
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
  found["messages_count"].as_u64().unwrap_or(0)
}
