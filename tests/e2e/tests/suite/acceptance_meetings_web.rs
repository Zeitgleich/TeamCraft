//! v1 acceptance, PLAN.md §9 and §12, issue #987: an external guest follows the link from the
//! invitation mail in a browser, waits in the lobby, the organizer's device lets the guest's own
//! device into the meeting chat's encryption, and the guest's browser and the organizer share the
//! meeting's call (e2ee.md §4.6, "Meeting guests").
use std::time::{Duration, Instant};

use chromiumoxide::cdp::browser_protocol::browser::{
  PermissionDescriptor, PermissionSetting, SetPermissionParams,
};
use reqwest::StatusCode;
use serde_json::json;
use sha2::{Digest, Sha256};
use teamcraft_e2e::browser::{BrowserSession, web_dir};
use teamcraft_e2e::call::join_call;
use teamcraft_e2e::{Mailpit, Peer, Stack};
use teamcraft_media::call_engine::CallEngineEvent;
use teamcraft_proto::{ConversationId, MeetingId, UserId};

use crate::web::{TEST_CSP, click_text, type_until_kept, wait_for_element, wait_for_text};
use crate::web_calls::{MEDIA_ARGS, click_selector, console_log, sfu_identity};

/// Ends a few minutes from now, so the call may start (a meeting that is over can't be joined).
const MEETING_LENGTH_MS: i64 = 60 * 60_000;
/// How long after the meeting's end the guest keeps access (the server's `guest_grace_secs`, as the
/// host's app passes it).
const GUEST_ACCESS_GRACE_MS: i64 = 2 * 60 * 60_000;
const SETUP_TIMEOUT: Duration = Duration::from_secs(60);
const CALL_DURATION: Duration = Duration::from_secs(8);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_guest_joins_a_meeting_with_the_mailed_link_and_shares_its_call() {
  let Some(web_dir) = web_dir() else {
    let is_required = std::env::var("E2E_REQUIRED").is_ok_and(|value| value == "1");
    assert!(!is_required, "no web build: run tests/e2e/web/build.sh");
    eprintln!("skipping: no web build (tests/e2e/web/build.sh)");
    return;
  };
  let identity = sfu_identity();
  let sha256: String = Sha256::digest(identity.cert.der())
    .iter()
    .map(|byte| format!("{byte:02x}"))
    .collect();
  let (certificate, key) = (identity.cert.pem(), identity.signing_key.serialize_pem());
  let Some(stack) = Stack::builder()
    .with_meetings()
    .with_calls()
    .with_sfu_identity(certificate, key, sha256)
    .with_web_dir(web_dir)
    // The SFU listens on a random port of its own: the test policy lets the page reach it.
    .with_env("TEAMCRAFT_WEB__CONTENT_SECURITY_POLICY", TEST_CSP)
    .start()
    .await
  else {
    return;
  };
  let Some(browser) = BrowserSession::launch_with(&MEDIA_ARGS).await else {
    return;
  };
  let certificate_hash = stack.maybe_sfu.as_ref().unwrap().certificate_hash;
  let mut alice = Peer::enroll(&stack, "alice").await;
  let address = format!("gus-{}@example.org", alice.client.device_id());
  let starts_at = now_ms() - 60_000;
  let ends_at = starts_at + MEETING_LENGTH_MS;
  let meeting = stack
    .post(
      &alice.tokens,
      "/meetings",
      json!({
        "title": "Customer call", "starts_at": starts_at, "ends_at": ends_at,
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
  // The mail carries the meeting link: an unguessable token, the organizer's fingerprint, no
  // secret and no meeting ID; the same link "Copy link" gives.
  let link = mail
    .text
    .split_whitespace()
    .find(|word| word.contains("/meeting/"))
    .unwrap_or_else(|| panic!("no meeting link in the mail:\n{}", mail.text))
    .to_owned();
  assert!(link.contains("#v="), "{link}");
  assert!(!link.contains("#invite=") && !link.contains("k="), "{link}");
  assert!(!link.contains(&meeting_id.to_string()), "{link}");
  assert_eq!(meeting.body["join_url"], link.as_str());

  // The organizer's app sets up the meeting chat's encryption group.
  alice.client.set_up_meeting_chat(chat, &[]).await.unwrap();

  let origin = stack.base_url.clone();
  let tokens = alice.tokens.clone();
  let lobby_path = format!("/meetings/{meeting_id}/lobby");
  let stack_ref = &stack;
  let alice_ref = &mut alice;
  let page = browser.page.clone();
  let console = console_log(&page).await;
  // Headless Chromium denies the microphone and camera unless the origin has them.
  for name in ["microphone", "camera"] {
    page
      .execute(
        SetPermissionParams::builder()
          .permission(PermissionDescriptor::new(name))
          .setting(PermissionSetting::Granted)
          .origin(stack.base_url.clone())
          .build()
          .unwrap(),
      )
      .await
      .unwrap();
  }
  browser
    .screenshot_on_failure("meeting-guest-mail-link", |page| async move {
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
      // Nobody let the guest in yet: the normal case, said plainly.
      wait_for_text(&page, "Waiting for the organizer").await;

      // The organizer's app sees the guest in the lobby once the guest's page set up its device.
      let deadline = Instant::now() + SETUP_TIMEOUT;
      let entry = loop {
        let lobby = stack_ref.get(&tokens, &lobby_path).await.expect(StatusCode::OK);
        let entry = lobby.body["entries"][0].clone();
        if entry["has_device"] == true && entry["knock_fingerprint"].is_string() {
          break entry;
        }
        assert!(
          Instant::now() < deadline,
          "the guest's device never got ready: {}",
          lobby.body
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
      };
      assert_eq!(entry["user"]["display_name"], "Gus (customer)");
      assert_eq!(entry["is_by_link"], true);
      let guest: UserId = entry["user"]["id"].as_str().unwrap().parse().unwrap();
      let fingerprint = entry["knock_fingerprint"].as_str().unwrap().to_owned();
      // The host's lobby entry shows the same key code as the guest's page.
      alice_ref.client.prepare_guest_key_code(guest).await.unwrap();
      let code = teamcraft_core::client::guest_key_code(alice_ref.store.as_ref(), guest).unwrap();
      wait_for_text(&page, &code).await;

      // Admitting makes the guest a member; the organizer's device adds the guest's own device to
      // the chat's group (a new epoch, the welcome sealed to the guest's key package), and the
      // guest's page, having checked the admitting device against `v`, opens the meeting.
      stack_ref
        .post(&tokens, &format!("{lobby_path}/{guest}/admit"), json!({}))
        .await
        .expect(StatusCode::NO_CONTENT);
      alice_ref
        .client
        .let_guest_into_meeting_chat(chat, guest, &fingerprint, ends_at + GUEST_ACCESS_GRACE_MS)
        .await
        .unwrap();
      wait_for_text(&page, "Join the call").await;

      // The guest joins the meeting's call from the browser.
      click_text(&page, "Join the call").await;
      wait_for_element(&page, ".prejoin").await;
      click_selector(&page, ".prejoin-actions .btn-primary").await;
      wait_for_element(&page, ".call-screen").await;
    })
    .await;

  // The organizer joins natively: both verify each other's DAVE keys, media flows both ways.
  let mut call = join_call(&mut alice, chat, certificate_hash, 550.0).await;
  let mut is_secured = false;
  let deadline = tokio::time::Instant::now() + CALL_DURATION;
  while let Ok(Some(event)) = tokio::time::timeout_at(deadline, call.next_event()).await {
    assert!(
      !matches!(
        event,
        CallEngineEvent::UntrustedRoster | CallEngineEvent::UnauthenticatedKeys | CallEngineEvent::Error(_)
      ),
      "{event:?}"
    );
    is_secured |= matches!(event, CallEngineEvent::Secured { .. });
  }
  let stats = call.leave().await.expect("leaving the call");
  if !is_secured || stats.frames_received <= 50 {
    for line in console.lock().unwrap().iter() {
      eprintln!("console: {line}");
    }
  }
  assert!(is_secured, "the DAVE group with the guest was never established");
  // The guest's fake microphone (a beep) reached the organizer and decoded.
  assert!(stats.frames_received > 50, "{stats:?}");

  browser.close().await;
  stack.stop().await;
}

fn now_ms() -> i64 {
  let now = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default();
  i64::try_from(now.as_millis()).unwrap_or(i64::MAX)
}
