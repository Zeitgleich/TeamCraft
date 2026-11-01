//! Calls in the web client (#330): the browser joins a channel call with a native client through
//! the real server and SFU. The call module loads in the background, the page signs this device's DAVE key
//! and verifies the other's, and media flows both ways (Chromium's fake microphone and camera).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chromiumoxide::cdp::browser_protocol::browser::{
  PermissionDescriptor, PermissionSetting, SetPermissionParams,
};
use chromiumoxide::page::Page;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use teamcraft_e2e::call::join_call;
use teamcraft_media::call_engine::CallEngineEvent;
use teamcraft_proto::VideoTrackKind;

use crate::web::{Rig, WEB_ORIGIN, sign_in_as_alice, wait_for_element, wait_for_text};

/// Fake devices that answer `getUserMedia` without a prompt.
pub(crate) const MEDIA_ARGS: [&str; 3] = [
  "--use-fake-device-for-media-stream",
  "--use-fake-ui-for-media-stream",
  "--autoplay-policy=no-user-gesture-required",
];
const CALL_DURATION: Duration = Duration::from_secs(8);
const SETUP_TIMEOUT: Duration = Duration::from_secs(120);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_web_client_and_a_native_client_share_a_call() {
  // The SFU's certificate is self-signed and short-lived; the server hands out its hash with join
  // tokens, which browsers accept (`serverCertificateHashes`), like the native clients' pin.
  let identity = sfu_identity();
  let sha256: String = Sha256::digest(identity.cert.der())
    .iter()
    .map(|byte| format!("{byte:02x}"))
    .collect();
  let (certificate, key) = (identity.cert.pem(), identity.signing_key.serialize_pem());
  let Some(mut rig) = Rig::start_with(
    |builder| builder.with_calls().with_sfu_identity(certificate, key, sha256),
    &MEDIA_ARGS,
  )
  .await
  else {
    return;
  };
  let channel = rig.channel;
  let certificate_hash = rig.stack.maybe_sfu.as_ref().unwrap().certificate_hash;

  let page = rig.browser.page.clone();
  let console = console_log(&page).await;
  // Headless Chromium denies the microphone and camera unless the origin has them.
  for name in ["microphone", "camera"] {
    page
      .execute(
        SetPermissionParams::builder()
          .permission(PermissionDescriptor::new(name))
          .setting(PermissionSetting::Granted)
          .origin(WEB_ORIGIN)
          .build()
          .unwrap(),
      )
      .await
      .unwrap();
  }
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
  if !is_secured || !has_browser_video {
    let resources = page
      .evaluate("performance.getEntriesByType('resource').map(e => e.name).filter(n => !n.includes('/api/')).join('\\n')")
      .await
      .ok()
      .and_then(|result| result.into_value::<String>().ok())
      .unwrap_or_default();
    eprintln!("resources:\n{resources}");
    let devices = page
      .evaluate(
        "navigator.mediaDevices.enumerateDevices().then(d => d.map(x => x.kind + ':' + x.label).join(', '))",
      )
      .await
      .ok()
      .and_then(|result| result.into_value::<String>().ok())
      .unwrap_or_default();
    eprintln!("devices: {devices}");
    eprintln!("page:\n{}", crate::web::body_text(&page).await);
    for line in console.lock().unwrap().iter() {
      eprintln!("console: {line}");
    }
  }

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

/// A self-signed certificate valid for ten days: certificate hash pins (the native clients) accept
/// at most two weeks.
pub(crate) fn sfu_identity() -> rcgen::CertifiedKey<rcgen::KeyPair> {
  let mut params =
    rcgen::CertificateParams::new(vec!["localhost".to_owned(), "::1".to_owned()]).expect("certificate names");
  let now = time::OffsetDateTime::now_utc();
  params.not_before = now - time::Duration::days(1);
  params.not_after = now + time::Duration::days(9);
  let signing_key = rcgen::KeyPair::generate().expect("a key pair");
  let cert = params
    .self_signed(&signing_key)
    .expect("a self-signed certificate");
  rcgen::CertifiedKey { cert, signing_key }
}

/// Everything the page logs, for the failure output.
pub(crate) async fn console_log(page: &Page) -> Arc<Mutex<Vec<String>>> {
  let log = Arc::new(Mutex::new(Vec::new()));
  let sink = log.clone();
  let mut events = page
    .event_listener::<chromiumoxide::cdp::js_protocol::runtime::EventConsoleApiCalled>()
    .await
    .unwrap();
  tokio::spawn(async move {
    while let Some(event) = events.next().await {
      let text: Vec<String> = event
        .args
        .iter()
        .map(|arg| {
          arg.value.as_ref().map_or_else(
            || arg.description.clone().unwrap_or_default(),
            ToString::to_string,
          )
        })
        .collect();
      let line = text.join(" ");
      if std::env::var("E2E_CONSOLE").is_ok() {
        eprintln!("console: {line}");
      }
      sink.lock().unwrap().push(line);
    }
  });
  log
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

/// Clicks through the DOM: at some window widths the header's tabs cover the call buttons (#365).
pub(crate) async fn click_selector(page: &Page, selector: &str) {
  wait_for_element(page, selector).await;
  page
    .evaluate(format!("document.querySelector({selector:?}).click()"))
    .await
    .unwrap();
}
