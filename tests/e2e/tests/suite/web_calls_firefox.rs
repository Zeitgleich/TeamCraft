//! Firefox for signed-in members (#1721): `meetings_firefox.rs` covers a guest by a meeting's link;
//! here a member signs in through Keycloak in Firefox, unlocks the account's keys on this new
//! browser, and shares a channel call with a native client, DAVE-secured, with Firefox's
//! microphone and camera reaching the other side (the flow of `web_calls.rs` in Chromium).
//!
//! And a Firefox without service workers (`dom.serviceWorkers.enabled` off, as Tor Browser and
//! hardened profiles have it) has no `navigator.serviceWorker`: the app called a method on it at
//! the start and never mounted, so a link opened a blank page.
//!
//! Needs a sound server, like `meetings_firefox.rs`.
use std::time::{Duration, Instant};

use teamcraft_e2e::browser::{lease_web_port, web_dir};
use teamcraft_e2e::call::{SfuIdentity, join_call};
use teamcraft_e2e::firefox::FirefoxSession;
use teamcraft_e2e::peer::PASSPHRASE;
use teamcraft_e2e::scenario::{channel_with, space_with};
use teamcraft_e2e::{Peer, Stack};
use teamcraft_media::call_engine::CallEngineEvent;
use teamcraft_proto::VideoTrackKind;

use crate::web::{TEST_CSP, WEB_ORIGIN};

const WAIT: Duration = Duration::from_secs(60);
const SETUP_TIMEOUT: Duration = Duration::from_secs(120);
const CALL_DURATION: Duration = Duration::from_secs(10);
const CHANNEL: &str = "firefox-channel";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_in_firefox_signs_in_and_shares_a_channel_call() {
  let Some(web_dir) = required_web_dir() else {
    return;
  };
  let SfuIdentity {
    certificate_pem,
    key_pem,
    sha256_hex,
  } = SfuIdentity::generate();
  // Keycloak sends the browser back only to the registered web ports; one test at a time owns it.
  let _turn = lease_web_port(8082).await;
  let Some(stack) = Stack::builder()
    .with_port(8082)
    .with_calls()
    .with_sfu_identity(certificate_pem, key_pem, sha256_hex)
    .with_web_dir(web_dir)
    .with_env("TEAMCRAFT_WEB__CONTENT_SECURITY_POLICY", TEST_CSP)
    .start()
    .await
  else {
    return;
  };
  let Some(mut firefox) = FirefoxSession::launch().await else {
    stack.stop().await;
    return;
  };
  let certificate_hash = stack.maybe_sfu.as_ref().unwrap().certificate_hash;
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut carol = Peer::enroll(&stack, "carol").await;
  let space = space_with(&stack, &alice, "Firefox Space", &[(&carol, "member")]).await;
  let channel = channel_with(&mut alice, space, CHANNEL, &mut [&mut carol]).await;
  // The browser enrolls later and takes the channel's key from the key bag, which must be there.
  alice.client.sync_account_data().await.unwrap();

  let console = firefox.console();
  let (alice_ref, carol_ref, firefox_ref) = (&mut alice, &mut carol, &mut firefox);
  let outcome = FirefoxSession::outcome(async move {
    let firefox = firefox_ref;
    sign_in_as_alice(firefox).await;
    firefox.wait_for_text(CHANNEL, WAIT).await;
    // The browser is a new device of Alice: her other device commits it into the channel, and
    // Carol must have verified it before the call admits it.
    let deadline = Instant::now() + SETUP_TIMEOUT;
    let mut round = 0_u32;
    while carol_ref
      .client
      .verified_call_devices(channel)
      .await
      .unwrap()
      .len()
      < 3
    {
      assert!(
        Instant::now() < deadline,
        "the browser never joined the channel's group"
      );
      if round.is_multiple_of(50) {
        alice_ref.heartbeat().await;
        carol_ref.heartbeat().await;
      }
      round += 1;
      alice_ref.pump_reconnecting().await;
      alice_ref.client.apply_pending_changes().await.unwrap();
      carol_ref.pump_reconnecting().await;
      carol_ref.client.apply_pending_changes().await.unwrap();
      tokio::time::sleep(Duration::from_millis(200)).await;
    }
    // The channel in the sidebar (a list item, not a button).
    firefox
      .wait_until(
        "the channel in the sidebar to take a click",
        &format!(
          "(() => {{ const el = [...document.querySelectorAll('button, a, li, [role=button], \
             [role=link], [role=treeitem], [role=option]')].find(e => e.innerText.includes({CHANNEL:?})); \
             if (!el) return false; el.click(); return true; }})()"
        ),
        WAIT,
      )
      .await;
    // The video button: Firefox joins with its fake camera and microphone on.
    firefox
      .wait_for_element("button[aria-label='Start video call']", WAIT)
      .await;
    firefox
      .click_selector("button[aria-label='Start video call']")
      .await;
    firefox.wait_for_element(".prejoin", WAIT).await;
    firefox.click_selector(".prejoin-actions .btn-primary").await;
    firefox.wait_for_element(".call-screen", WAIT).await;
  })
  .await;
  firefox
    .screenshot_if_failed("web-call-member-firefox", outcome)
    .await;

  let mut call = join_call(&mut carol, channel, certificate_hash, 550.0).await;
  let mut is_secured = false;
  let mut has_browser_video = false;
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
    has_browser_video |=
      matches!(&event, CallEngineEvent::VideoPublished(track) if track.kind == VideoTrackKind::Camera);
  }
  let browser_code = firefox
    .evaluate("document.querySelector('.call-screen .e2ee-pill .mono')?.innerText ?? ''")
    .await;
  let stats = call.leave().await.expect("leaving the call");
  if !is_secured || !has_browser_video || stats.frames_received <= 50 {
    for line in console.lock().unwrap().iter() {
      eprintln!("console: {line}");
    }
  }
  assert!(
    is_secured,
    "the DAVE group with the Firefox member was never established"
  );
  assert!(stats.frames_received > 50, "Firefox's audio: {stats:?}");
  assert!(has_browser_video, "Firefox's camera never reached the call");
  let carol_code = stats.maybe_privacy_code.clone().expect("Carol's privacy code");
  assert!(
    browser_code
      .as_str()
      .is_some_and(|code| code.contains(&carol_code)),
    "Firefox shows {browser_code}, Carol has {carol_code:?}"
  );

  firefox.close().await;
  stack.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_web_client_starts_in_a_firefox_without_service_workers() {
  let Some(web_dir) = required_web_dir() else {
    return;
  };
  let Some(stack) = Stack::builder()
    .with_web_dir(web_dir)
    .with_env("TEAMCRAFT_WEB__CONTENT_SECURITY_POLICY", TEST_CSP)
    .start()
    .await
  else {
    return;
  };
  let Some(mut firefox) = FirefoxSession::launch_with(&[("dom.serviceWorkers.enabled", "false")]).await
  else {
    stack.stop().await;
    return;
  };
  let origin = stack.base_url.clone();
  let firefox_ref = &mut firefox;
  let outcome = FirefoxSession::outcome(async move {
    let firefox = firefox_ref;
    firefox.goto(&format!("{origin}/healthz")).await;
    assert_eq!(
      firefox.evaluate("'serviceWorker' in navigator").await,
      serde_json::Value::Bool(false),
      "the profile should have no service workers"
    );
    firefox
      .evaluate("localStorage.setItem('teamcraft.locale', 'en')")
      .await;
    firefox.goto(&format!("{origin}/")).await;
    firefox.wait_for_text("Sign in", WAIT).await;
  })
  .await;
  firefox
    .screenshot_if_failed("web-start-firefox-no-service-worker", outcome)
    .await;
  firefox.close().await;
  stack.stop().await;
}

fn required_web_dir() -> Option<std::path::PathBuf> {
  let maybe_dir = web_dir();
  if maybe_dir.is_none() {
    let is_required = std::env::var("E2E_REQUIRED").is_ok_and(|value| value == "1");
    assert!(!is_required, "no web build: run tests/e2e/web/build.sh");
    eprintln!("skipping: no web build (tests/e2e/web/build.sh)");
  }
  maybe_dir
}

/// Signs in through Keycloak's login page in Firefox and unlocks the account's keys on this new
/// browser with the recovery passphrase (`web::sign_in_as_alice` for Chromium).
async fn sign_in_as_alice(firefox: &mut FirefoxSession) {
  // The UI follows the browser language; pin English before the app loads (same origin).
  firefox.goto(&format!("{WEB_ORIGIN}/healthz")).await;
  firefox
    .evaluate("localStorage.setItem('teamcraft.locale', 'en')")
    .await;
  firefox.goto(&format!("{WEB_ORIGIN}/")).await;
  firefox.wait_for_text("Sign in", WAIT).await;
  firefox.click_text("Sign in").await;

  // Keycloak's login form.
  firefox.wait_for_element("input[name=username]", WAIT).await;
  firefox.fill("input[name=username]", "alice").await;
  firefox.fill("input[name=password]", "alice-dev-pass").await;
  firefox
    .evaluate(
      "(() => { const form = document.querySelector('input[name=password]').form; \
         form.requestSubmit ? form.requestSubmit() : form.submit(); return true; })()",
    )
    .await;

  // The account already has keys: pick the recovery kit among the ways to bring the key and type
  // the passphrase. The setup screen renders again while the core settles, which loses a click or
  // the typed text (#444), so both repeat until the text stays (`web::enter_recovery_key`).
  let deadline = Instant::now() + WAIT;
  loop {
    let has_field = firefox
      .evaluate("!!document.querySelector('#enroll-secret')")
      .await;
    if has_field != serde_json::Value::Bool(true) {
      firefox
        .evaluate(
          "(() => { const el = [...document.querySelectorAll('button')]\
             .find(e => e.innerText.includes('Use my recovery kit')); \
             if (el) el.click(); return !!el; })()",
        )
        .await;
      tokio::time::sleep(Duration::from_millis(300)).await;
    } else {
      firefox.fill("#enroll-secret", PASSPHRASE).await;
      tokio::time::sleep(Duration::from_millis(300)).await;
      let kept = firefox
        .evaluate("(document.querySelector('#enroll-secret') || {}).value")
        .await;
      if kept.as_str() == Some(PASSPHRASE) {
        break;
      }
    }
    assert!(
      Instant::now() < deadline,
      "the recovery key field never kept the typed text"
    );
  }
  firefox.click_text("Unlock").await;
}
