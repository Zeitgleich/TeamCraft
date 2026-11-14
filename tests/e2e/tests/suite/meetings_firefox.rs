//! Firefox (#1721): an external guest opens a meeting's link in Firefox, waits in the lobby, the
//! organizer's device lets the guest's own device into the meeting chat's encryption, and the
//! guest's Firefox and the organizer share the meeting's call, DAVE-secured, with the guest's
//! microphone and camera reaching the organizer (the flow of `acceptance_meetings_web.rs` in
//! Chromium). Firefox has no `MediaStreamTrackProcessor` and takes the call module's AudioWorklet
//! and `<video>` paths (ARCHITECTURE.md §6.1).
//!
//! The guest's sound device runs at 44.1 kHz, as many headsets do: Firefox before 148 connects
//! such a microphone only to an audio graph at that rate, and the join failed (#1721). Run with
//! `FIREFOX=/path/to/firefox-140` to see that path; a current Firefox takes the 48 kHz graph.
use std::time::{Duration, Instant};

use reqwest::StatusCode;
use serde_json::json;
use teamcraft_e2e::browser::web_dir;
use teamcraft_e2e::call::{SfuIdentity, join_call};
use teamcraft_e2e::firefox::FirefoxSession;
use teamcraft_e2e::{Peer, Stack};
use teamcraft_media::call_engine::CallEngineEvent;
use teamcraft_proto::{ConversationId, MeetingId, UserId, VideoTrackKind};

use crate::web::TEST_CSP;

const MEETING_LENGTH_MS: i64 = 60 * 60_000;
const GUEST_ACCESS_GRACE_MS: i64 = 2 * 60 * 60_000;
const WAIT: Duration = Duration::from_secs(60);
const CALL_DURATION: Duration = Duration::from_secs(10);
/// The rate of Firefox's (fake) sound devices: a 44.1 kHz headset.
const DEVICE_RATE: (&str, &str) = ("media.cubeb.force_sample_rate", "44100");

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_guest_in_firefox_joins_a_meeting_by_its_link_and_shares_the_call() {
  let Some(web_dir) = web_dir() else {
    let is_required = std::env::var("E2E_REQUIRED").is_ok_and(|value| value == "1");
    assert!(!is_required, "no web build: run tests/e2e/web/build.sh");
    eprintln!("skipping: no web build (tests/e2e/web/build.sh)");
    return;
  };
  let SfuIdentity {
    certificate_pem,
    key_pem,
    sha256_hex,
  } = SfuIdentity::generate();
  let Some(stack) = Stack::builder()
    .with_meetings()
    .with_calls()
    .with_sfu_identity(certificate_pem, key_pem, sha256_hex)
    .with_web_dir(web_dir)
    .with_env("TEAMCRAFT_WEB__CONTENT_SECURITY_POLICY", TEST_CSP)
    .start()
    .await
  else {
    return;
  };
  let Some(mut firefox) = FirefoxSession::launch_with(&[DEVICE_RATE]).await else {
    stack.stop().await;
    return;
  };
  let certificate_hash = stack.maybe_sfu.as_ref().unwrap().certificate_hash;
  let mut alice = Peer::enroll(&stack, "alice").await;
  let starts_at = now_ms() - 60_000;
  let ends_at = starts_at + MEETING_LENGTH_MS;
  let meeting = stack
    .post(
      &alice.tokens,
      "/meetings",
      json!({
        "title": "Customer call", "starts_at": starts_at, "ends_at": ends_at,
        "has_lobby": true, "invitees": [],
      }),
    )
    .await
    .expect(StatusCode::CREATED);
  let meeting_id: MeetingId = meeting.id().parse().unwrap();
  let chat: ConversationId = meeting.body["conversation_id"].as_str().unwrap().parse().unwrap();
  let link = meeting.body["join_url"].as_str().unwrap().to_owned();
  alice.client.set_up_meeting_chat(chat, &[]).await.unwrap();

  let origin = stack.base_url.clone();
  let lobby_path = format!("/meetings/{meeting_id}/lobby");
  let console = firefox.console();
  let (stack_ref, alice_ref, firefox_ref) = (&stack, &mut alice, &mut firefox);
  let outcome = FirefoxSession::outcome(async move {
    let firefox = firefox_ref;
    // The UI follows the browser language; pin English (same origin, before the app loads).
    firefox.goto(&format!("{origin}/healthz")).await;
    firefox
      .evaluate("localStorage.setItem('teamcraft.locale', 'en')")
      .await;
    firefox.goto(&link).await;
    firefox.wait_for_text("Join the meeting", WAIT).await;
    firefox.wait_for_element("input.input", WAIT).await;
    firefox.fill("input.input", "Gus (Firefox)").await;
    firefox.click_text("Ask to join").await;
    firefox
      .wait_for_text("Waiting for someone in the meeting to let you in", WAIT)
      .await;

    let deadline = Instant::now() + WAIT;
    let entry = loop {
      let lobby = stack_ref
        .get(&alice_ref.tokens, &lobby_path)
        .await
        .expect(StatusCode::OK);
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
    let guest: UserId = entry["user"]["id"].as_str().unwrap().parse().unwrap();
    let fingerprint = entry["knock_fingerprint"].as_str().unwrap().to_owned();
    stack_ref
      .post(
        &alice_ref.tokens,
        &format!("{lobby_path}/{guest}/admit"),
        json!({}),
      )
      .await
      .expect(StatusCode::NO_CONTENT);
    alice_ref
      .client
      .let_guest_into_meeting_chat(chat, guest, &fingerprint, ends_at + GUEST_ACCESS_GRACE_MS)
      .await
      .unwrap();

    // Admission opens the call's pre-join screen by itself (#1929).
    firefox.wait_for_element(".prejoin", WAIT).await;
    firefox.click_selector(".prejoin-actions .btn-primary").await;
    firefox.wait_for_element(".call-screen", WAIT).await;
  })
  .await;
  firefox
    .screenshot_if_failed("meeting-guest-firefox", outcome)
    .await;

  // The organizer joins natively: both verify each other's DAVE keys, the guest's fake
  // microphone and camera reach the organizer, and both show the same privacy code.
  let mut call = join_call(&mut alice, chat, certificate_hash, 550.0).await;
  let mut is_secured = false;
  let mut has_guest_video = false;
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
    has_guest_video |=
      matches!(&event, CallEngineEvent::VideoPublished(track) if track.kind == VideoTrackKind::Camera);
  }
  let guest_code = firefox
    .evaluate("document.querySelector('.call-screen .e2ee-pill .mono')?.innerText ?? ''")
    .await;
  let stats = call.leave().await.expect("leaving the call");
  if !is_secured || !has_guest_video || stats.frames_received <= 50 {
    for line in console.lock().unwrap().iter() {
      eprintln!("console: {line}");
    }
  }
  assert!(
    is_secured,
    "the DAVE group with the Firefox guest was never established"
  );
  assert!(stats.frames_received > 50, "the guest's audio: {stats:?}");
  assert!(has_guest_video, "the guest's camera never reached the call");
  let organizer_code = stats
    .maybe_privacy_code
    .clone()
    .expect("the organizer's privacy code");
  assert!(
    guest_code
      .as_str()
      .is_some_and(|code| code.contains(&organizer_code)),
    "the guest shows {guest_code}, the organizer has {organizer_code:?}"
  );

  firefox.close().await;
  stack.stop().await;
}

fn now_ms() -> i64 {
  let now = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default();
  i64::try_from(now.as_millis()).unwrap_or(i64::MAX)
}
