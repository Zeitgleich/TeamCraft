//! Calls in the web client (#330): the browser joins a channel call with a native client through
//! the real server and SFU. The call module loads on demand, the page signs this device's DAVE key
//! and verifies the other's, and media flows both ways (Chromium's fake microphone and camera).

use std::time::{Duration, Instant};

use chromiumoxide::page::Page;
use teamcraft_e2e::call::join_call;
use teamcraft_media::call_engine::CallEngineEvent;
use teamcraft_proto::VideoTrackKind;

use crate::web::{Rig, sign_in_as_alice, wait_for_element, wait_for_text};

/// Fake devices that answer `getUserMedia` without a prompt, and the SFU's self-signed certificate
/// accepted (the native clients pin its hash).
const MEDIA_ARGS: [&str; 4] = [
  "--use-fake-device-for-media-stream",
  "--use-fake-ui-for-media-stream",
  "--ignore-certificate-errors",
  "--autoplay-policy=no-user-gesture-required",
];
const CALL_DURATION: Duration = Duration::from_secs(8);
const SETUP_TIMEOUT: Duration = Duration::from_secs(120);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_web_client_and_a_native_client_share_a_call() {
  let Some(mut rig) = Rig::start_with(true, &MEDIA_ARGS).await else {
    return;
  };
  let channel = rig.channel;
  let certificate_hash = rig.stack.maybe_sfu.as_ref().unwrap().certificate_hash;

  let page = rig.browser.page.clone();
  sign_in_as_alice(&page).await;
  wait_for_text(&page, "browser-channel").await;
  // The browser is a new device of Alice: her other device commits it into the channel, and Carol
  // must have verified it before the call admits it.
  let deadline = Instant::now() + SETUP_TIMEOUT;
  let mut round = 0_u32;
  loop {
    let verified = rig.carol.client.verified_call_devices(channel).await.unwrap();
    if verified.len() >= 3 {
      break;
    }
    assert!(
      Instant::now() < deadline,
      "the browser never joined the channel's group"
    );
    if round.is_multiple_of(50) {
      rig.alice.heartbeat().await;
      rig.carol.heartbeat().await;
    }
    round += 1;
    rig.alice.pump_reconnecting().await;
    rig.alice.client.apply_pending_changes().await.unwrap();
    rig.carol.pump_reconnecting().await;
    rig.carol.client.apply_pending_changes().await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
  }

  rig
    .browser
    .screenshot_on_failure("web-call", |page| async move {
      crate::web::click_text(&page, "browser-channel").await;
      // The video button: the browser joins with its (fake) camera on.
      click_selector(&page, "button[aria-label='Start video call']").await;
      wait_for_element(&page, ".prejoin").await;
      click_selector(&page, ".prejoin-actions .btn-primary").await;
      wait_for_element(&page, ".call-screen").await;
    })
    .await;

  let mut carol_call = join_call(&mut rig.carol, channel, certificate_hash, 550.0).await;
  let mut is_secured = false;
  let mut has_browser_video = false;
  let deadline = tokio::time::Instant::now() + CALL_DURATION;
  while let Ok(Some(event)) = tokio::time::timeout_at(deadline, carol_call.next_event()).await {
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
  let browser_code = privacy_code_on(&page).await;
  let stats = carol_call.leave().await.expect("leaving the call");

  assert!(
    is_secured,
    "the DAVE group with the browser was never established"
  );
  assert!(has_browser_video, "the browser's camera never reached the call");
  // The browser's fake microphone (a beep) reached Carol and decoded.
  assert!(stats.frames_received > 50, "{stats:?}");
  // Both sides derived the same privacy code from one DAVE group.
  let carol_code = stats.maybe_privacy_code.clone().expect("Carol's privacy code");
  assert!(
    browser_code.contains(&carol_code),
    "the browser shows {browser_code:?}, Carol has {carol_code:?}"
  );

  rig.finish().await;
}

/// The privacy code the call screen shows ("Code 12 34 56").
async fn privacy_code_on(page: &Page) -> String {
  page
    .evaluate("document.querySelector('.e2ee-pill .mono')?.innerText ?? ''")
    .await
    .ok()
    .and_then(|result| result.into_value::<String>().ok())
    .unwrap_or_default()
}

async fn click_selector(page: &Page, selector: &str) {
  wait_for_element(page, selector).await.click().await.unwrap();
}
