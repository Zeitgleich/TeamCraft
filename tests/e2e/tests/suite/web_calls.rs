//! Calls in the web client (#330): the browser joins a channel call with a native client through
//! the real server and SFU. The call module loads in the background, the page signs this device's DAVE key
//! and verifies the other's, and media flows both ways (Chromium's fake microphone and camera).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chromiumoxide::cdp::browser_protocol::browser::{
  PermissionDescriptor, PermissionSetting, SetPermissionParams,
};
use chromiumoxide::cdp::browser_protocol::emulation::{
  MediaFeature, SetDeviceMetricsOverrideParams, SetEmulatedMediaParams,
};
use chromiumoxide::page::{Page, ScreenshotParams};
use futures_util::StreamExt;
use teamcraft_e2e::browser::artifact_dir;
use teamcraft_e2e::call::{SfuIdentity, join_call};
use teamcraft_e2e::netem::{Condition, PortConditions};
use teamcraft_media::call_engine::CallEngineEvent;
use teamcraft_media::call_native::NativeCall;
use teamcraft_media::call_resilience::CallTransport;
use teamcraft_proto::{MediaStreamId, VideoTrackKind};

use crate::web::{
  Rig, WEB_ORIGIN, click_text, sign_in_again, sign_in_as_alice, wait_for_element, wait_for_text,
  wait_for_text_gone,
};

/// Fake devices that answer `getUserMedia` without a prompt.
pub(crate) const MEDIA_ARGS: [&str; 3] = [
  "--use-fake-device-for-media-stream",
  "--use-fake-ui-for-media-stream",
  "--autoplay-policy=no-user-gesture-required",
];
const CALL_DURATION: Duration = Duration::from_secs(8);
const SETUP_TIMEOUT: Duration = Duration::from_secs(120);
/// A call on the fallback tries QUIC about a minute after it got there (48 to 72 s, #1338), and the
/// move itself takes a handshake and a round trip.
const BACK_TO_QUIC_TIMEOUT: Duration = Duration::from_secs(110);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_web_client_and_a_native_client_share_a_call() {
  let Some(mut rig) = start_rig().await else {
    return;
  };
  let channel = rig.channel;
  let certificate_hash = rig.stack.maybe_sfu.as_ref().unwrap().certificate_hash;

  let page = rig.browser.page.clone();
  let console = console_log(&page).await;
  sign_in_and_join_the_channel(&mut rig, &page).await;

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

/// A network that drops all UDP (#123), with the real browser: its WebTransport never connects, it
/// moves to the WebSocket fallback, says so, and media flows with a native client on the same
/// network. Everything of the call stays as above: the same DAVE group, the same privacy code.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the network namespace: scripts/e2e-network.sh"]
async fn the_web_client_falls_back_on_a_network_without_udp() {
  let Some(mut rig) = start_rig_with_fallback().await else {
    return;
  };
  let Some(conditions) = PortConditions::take(1) else {
    rig.finish().await;
    return;
  };
  conditions.set(Condition::NoUdp);
  let channel = rig.channel;
  let certificate_hash = rig.stack.maybe_sfu.as_ref().unwrap().certificate_hash;
  let page = rig.browser.page.clone();
  let console = console_log(&page).await;
  sign_in_and_join_the_channel(&mut rig, &page).await;
  rig
    .browser
    .screenshot_on_failure("web-call-no-udp", |page| async move {
      crate::web::click_text(&page, "browser-channel").await;
      click_selector(&page, "button[aria-label='Start video call']").await;
      wait_for_element(&page, ".prejoin").await;
      click_selector(&page, ".prejoin-actions .btn-primary").await;
      wait_for_element(&page, ".call-screen").await;
      wait_for_text(&page, "Limited network, call quality may be lower").await;
    })
    .await;

  let mut carol_call = join_call(&mut rig.carol, channel, certificate_hash, 550.0).await;
  let (mut is_secured, mut has_browser_video, mut maybe_transport) = (false, false, None);
  let deadline = tokio::time::Instant::now() + CALL_DURATION;
  while let Ok(Some(event)) = tokio::time::timeout_at(deadline, carol_call.next_event()).await {
    is_secured |= matches!(event, CallEngineEvent::Secured { .. });
    has_browser_video |=
      matches!(&event, CallEngineEvent::VideoPublished(track) if track.kind == VideoTrackKind::Camera);
    if let CallEngineEvent::TransportChanged(transport) = event {
      maybe_transport = Some(transport);
    }
  }
  let browser_code = privacy_code_on(&page).await;
  let stats = carol_call.leave().await.expect("leaving the call");
  for line in console
    .lock()
    .unwrap()
    .iter()
    .filter(|line| line.contains("fallback"))
  {
    eprintln!("console: {line}");
  }
  eprintln!("the native client on the same network: {stats:?}");
  assert_eq!(maybe_transport, Some(CallTransport::Fallback));
  assert!(
    is_secured,
    "the DAVE group with the browser was never established"
  );
  assert!(has_browser_video, "the browser's camera never reached the call");
  assert!(stats.frames_received > 50, "{stats:?}");
  let carol_code = stats.maybe_privacy_code.clone().expect("Carol's privacy code");
  assert!(
    browser_code.contains(&carol_code),
    "the browser shows {browser_code:?}, Carol has {carol_code:?}"
  );
  drop(conditions);
  rig.finish().await;
}

/// UDP comes back during a call that started on the fallback (#1338): the browser tries QUIC about
/// a minute in, moves its session there without a rejoin and stops saying the network is limited.
/// The native client in the call (on the same network, so on the fallback too until it moves as
/// well) sees no reconnect, no leave and no new DAVE epoch, and the browser's camera goes on
/// reaching it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the network namespace: scripts/e2e-network.sh"]
async fn the_web_client_moves_back_to_quic_when_udp_comes_back() {
  let Some(mut rig) = start_rig_with_fallback().await else {
    return;
  };
  let Some(conditions) = PortConditions::take(1) else {
    rig.finish().await;
    return;
  };
  conditions.set(Condition::NoUdp);
  let channel = rig.channel;
  let certificate_hash = rig.stack.maybe_sfu.as_ref().unwrap().certificate_hash;
  let page = rig.browser.page.clone();
  let console = console_log(&page).await;
  sign_in_and_join_the_channel(&mut rig, &page).await;
  rig
    .browser
    .screenshot_on_failure("web-call-back-to-quic", |page| async move {
      crate::web::click_text(&page, "browser-channel").await;
      click_selector(&page, "button[aria-label='Start video call']").await;
      wait_for_element(&page, ".prejoin").await;
      click_selector(&page, ".prejoin-actions .btn-primary").await;
      wait_for_element(&page, ".call-screen").await;
      wait_for_text(&page, "Limited network, call quality may be lower").await;
    })
    .await;
  let mut carol_call = join_call(&mut rig.carol, channel, certificate_hash, 550.0).await;
  let (epoch, camera) = wait_for_secured_camera(&mut carol_call).await;
  // Carol takes the browser's camera herself: its frames show that media goes on after the move.
  carol_call.subscribe_video(camera, Some(0));

  let logged_before = console.lock().unwrap().len();
  conditions.set(Condition::Clear);
  let has_moved = || {
    console
      .lock()
      .unwrap()
      .iter()
      .any(|line| line.contains("the call moved to QUIC"))
  };
  let deadline = tokio::time::Instant::now() + BACK_TO_QUIC_TIMEOUT;
  while !has_moved() {
    if tokio::time::Instant::now() >= deadline {
      dump_console(&console, &["QUIC", "fallback", "switch", "rejoin"]);
      panic!("the browser never moved to QUIC");
    }
    if let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(200), carol_call.next_event()).await {
      assert_undisturbed(&event, epoch);
    }
  }
  rig
    .browser
    .screenshot_on_failure("web-call-back-to-quic-notice", |page| async move {
      wait_for_text_gone(&page, "Limited network").await;
    })
    .await;

  let mut camera_frames = 0_u32;
  let deadline = tokio::time::Instant::now() + CALL_DURATION;
  while let Ok(Some(event)) = tokio::time::timeout_at(deadline, carol_call.next_event()).await {
    assert_undisturbed(&event, epoch);
    if matches!(&event, CallEngineEvent::VideoFrame(frame) if frame.stream_id == camera) {
      camera_frames += 1;
    }
  }
  let browser_code = privacy_code_on(&page).await;
  let stats = carol_call.leave().await.expect("leaving the call");
  dump_console(&console, &["QUIC", "fallback", "switch"]);
  assert!(
    camera_frames > 10,
    "{camera_frames} frames of the browser's camera after the move"
  );
  assert!(
    !console.lock().unwrap()[logged_before..]
      .iter()
      .any(|line| line.contains("rejoining")),
    "the browser rejoined"
  );
  assert!(stats.frames_received > 50, "{stats:?}");
  let carol_code = stats.maybe_privacy_code.clone().expect("Carol's privacy code");
  assert!(
    browser_code.contains(&carol_code),
    "the browser shows {browser_code:?}, Carol has {carol_code:?}"
  );
  drop(conditions);
  rig.finish().await;
}

/// A full SFU turns a browser away after the upgrade, which to the browser is a session that opened.
/// It retries at the pace of any failure, not as fast as the network allows (#123).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the network namespace: scripts/e2e-network.sh"]
async fn the_web_client_turned_away_from_a_full_sfu_retries_with_backoff() {
  let SfuIdentity {
    certificate_pem: certificate,
    key_pem: key,
    sha256_hex: sha256,
  } = SfuIdentity::generate();
  let Some(mut rig) = Rig::start_with(
    |builder| {
      builder
        .with_one_call_fallback_slot()
        .with_sfu_identity(certificate, key, sha256)
    },
    &MEDIA_ARGS,
  )
  .await
  else {
    return;
  };
  let Some(conditions) = PortConditions::take(1) else {
    rig.finish().await;
    return;
  };
  conditions.set(Condition::NoUdp);
  let channel = rig.channel;
  let certificate_hash = rig.stack.maybe_sfu.as_ref().unwrap().certificate_hash;
  let page = rig.browser.page.clone();
  let console = console_log(&page).await;
  sign_in_and_join_the_channel(&mut rig, &page).await;
  // Carol takes the one slot; she stays connected while the browser is turned away.
  let carol_call = join_call(&mut rig.carol, channel, certificate_hash, 550.0).await;
  rig
    .browser
    .screenshot_on_failure("web-call-full-sfu", |page| async move {
      crate::web::click_text(&page, "browser-channel").await;
      click_selector(&page, "button[aria-label='Start video call']").await;
      wait_for_element(&page, ".prejoin").await;
      click_selector(&page, ".prejoin-actions .btn-primary").await;
    })
    .await;
  let lost = || {
    console
      .lock()
      .unwrap()
      .iter()
      .filter(|line| line.contains("lost the connection to the call"))
      .count()
  };
  // The first socket waits for QUIC's fallback delay; from the first loss on it is about pacing.
  let deadline = tokio::time::Instant::now() + SETUP_TIMEOUT;
  while lost() == 0 && tokio::time::Instant::now() < deadline {
    tokio::time::sleep(Duration::from_millis(50)).await;
  }
  let first = lost();
  tokio::time::sleep(Duration::from_secs(8)).await;
  let attempts = lost() - first;
  eprintln!("the browser's attempts in 8 s against a full SFU: {attempts}");
  assert!(first > 0, "the browser never reached the full SFU");
  // With 250 ms doubling to 2 s, jittered down to half: about 5 to 13 in 8 s. Hundreds without.
  assert!((2..=20).contains(&attempts), "{attempts} attempts in 8 s");
  assert!(!carol_call.is_finished(), "the call on the one slot goes on");
  carol_call.leave().await.expect("leaving the call");
  drop(conditions);
  rig.finish().await;
}

/// The other way round: with UDP working and the fallback on offer, the browser and the native client
/// stay on QUIC and nothing says the network is limited (#123).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_web_client_stays_on_quic_when_udp_works() {
  let Some(mut rig) = start_rig_with_fallback().await else {
    return;
  };
  let channel = rig.channel;
  let certificate_hash = rig.stack.maybe_sfu.as_ref().unwrap().certificate_hash;
  let page = rig.browser.page.clone();
  let console = console_log(&page).await;
  sign_in_and_join_the_channel(&mut rig, &page).await;
  rig
    .browser
    .screenshot_on_failure("web-call-quic-preferred", |page| async move {
      crate::web::click_text(&page, "browser-channel").await;
      click_selector(&page, "button[aria-label='Start video call']").await;
      wait_for_element(&page, ".prejoin").await;
      click_selector(&page, ".prejoin-actions .btn-primary").await;
      wait_for_element(&page, ".call-screen").await;
    })
    .await;
  let mut carol_call = join_call(&mut rig.carol, channel, certificate_hash, 550.0).await;
  let mut maybe_transport = None;
  let mut has_browser_video = false;
  let deadline = tokio::time::Instant::now() + CALL_DURATION;
  while let Ok(Some(event)) = tokio::time::timeout_at(deadline, carol_call.next_event()).await {
    has_browser_video |=
      matches!(&event, CallEngineEvent::VideoPublished(track) if track.kind == VideoTrackKind::Camera);
    if let CallEngineEvent::TransportChanged(transport) = event {
      maybe_transport = Some(transport);
    }
  }
  let stats = carol_call.leave().await.expect("leaving the call");
  assert_eq!(maybe_transport, Some(CallTransport::Quic));
  assert!(has_browser_video, "the browser's camera never reached the call");
  assert!(stats.frames_received > 50, "{stats:?}");
  assert!(
    !crate::web::body_text(&page).await.contains("Limited network"),
    "the browser says the network is limited"
  );
  assert!(
    !console
      .lock()
      .unwrap()
      .iter()
      .any(|line| line.contains("runs on the fallback")),
    "the browser moved to the fallback"
  );
  rig.finish().await;
}

/// A browser without WebTransport (Safari 26.0 to 26.3) calls over the fallback (#1227): the call
/// buttons take it, it goes straight to the WebSocket, says the network is limited, and media flows
/// with a native client that stays on QUIC.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_browser_without_webtransport_calls_over_the_fallback() {
  let Some(mut rig) = start_rig_with_fallback().await else {
    return;
  };
  let channel = rig.channel;
  let certificate_hash = rig.stack.maybe_sfu.as_ref().unwrap().certificate_hash;
  let page = rig.browser.page.clone();
  // Before any of the app's documents load.
  page
    .evaluate_on_new_document("delete globalThis.WebTransport")
    .await
    .unwrap();
  sign_in_and_join_the_channel(&mut rig, &page).await;
  rig
    .browser
    .screenshot_on_failure("web-call-no-webtransport", |page| async move {
      crate::web::click_text(&page, "browser-channel").await;
      click_selector(&page, "button[aria-label='Start video call']").await;
      wait_for_element(&page, ".prejoin").await;
      click_selector(&page, ".prejoin-actions .btn-primary").await;
      wait_for_element(&page, ".call-screen").await;
      wait_for_text(&page, "Limited network, call quality may be lower").await;
    })
    .await;
  let mut carol_call = join_call(&mut rig.carol, channel, certificate_hash, 550.0).await;
  let (mut is_secured, mut has_browser_video) = (false, false);
  let deadline = tokio::time::Instant::now() + CALL_DURATION;
  while let Ok(Some(event)) = tokio::time::timeout_at(deadline, carol_call.next_event()).await {
    is_secured |= matches!(event, CallEngineEvent::Secured { .. });
    has_browser_video |=
      matches!(&event, CallEngineEvent::VideoPublished(track) if track.kind == VideoTrackKind::Camera);
  }
  let stats = carol_call.leave().await.expect("leaving the call");
  assert!(
    !page
      .evaluate("'WebTransport' in globalThis")
      .await
      .unwrap()
      .into_value::<bool>()
      .unwrap(),
    "the page still had WebTransport"
  );
  assert!(
    is_secured,
    "the DAVE group with the browser was never established"
  );
  assert!(has_browser_video, "the browser's camera never reached the call");
  assert!(stats.frames_received > 50, "{stats:?}");
  rig.finish().await;
}

/// The person sharing sees what goes out (#1175): small, live, from the capture, and gone with the
/// share or when collapsed. Chromium's fake capture stands in for the screen.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_the_screen_shows_a_small_live_preview_of_it() {
  let Some(mut rig) = start_rig().await else {
    return;
  };
  let page = rig.browser.page.clone();
  sign_in_and_join_the_channel(&mut rig, &page).await;
  rig
    .browser
    .screenshot_on_failure("web-share-preview", |page| async move {
      crate::web::click_text(&page, "browser-channel").await;
      click_selector(&page, "button[aria-label='Start video call']").await;
      wait_for_element(&page, ".prejoin").await;
      click_selector(&page, ".prejoin-actions .btn-primary").await;
      wait_for_element(&page, ".call-screen").await;

      // Real clicks: the browser opens its screen picker only for a user's gesture.
      click_for_real(&page, "button[aria-label='Share screen']").await;
      click_for_real(&page, ".share-popover .btn").await;
      let first = wait_for_preview_picture(&page).await;
      assert!(
        first["cssWidth"].as_f64().unwrap() <= 320.0,
        "the preview is small: {first}"
      );
      assert!(
        first["frameRate"].as_f64().unwrap() <= 5.0,
        "the preview runs at a low frame rate: {first}"
      );
      assert!(!has_element(&page, ".mock-window").await);
      wait_until_it_plays_on(&page, first["time"].as_f64().unwrap()).await;

      // Collapsed: the share goes on, and nothing keeps running for the preview.
      page.evaluate(KEEP_PREVIEW_TRACK).await.unwrap();
      click_for_real(&page, ".share-preview .btn").await;
      wait_until_true(&page, "window.previewTrack.readyState === 'ended'").await;
      assert!(!has_element(&page, ".share-preview video").await);
      assert!(has_element(&page, ".share-banner .btn").await);

      // Back: the capture still runs, so the picture comes at once.
      click_for_real(&page, ".share-preview .btn").await;
      let again = wait_for_preview_picture(&page).await;
      wait_until_it_plays_on(&page, again["time"].as_f64().unwrap()).await;

      page
        .execute(SetDeviceMetricsOverrideParams::new(1280, 800, 1.0, false))
        .await
        .unwrap();
      for scheme in ["light", "dark"] {
        screenshot(&page, scheme, &format!("share-preview-{scheme}")).await;
      }
      page
        .execute(SetDeviceMetricsOverrideParams::new(390, 844, 2.0, true))
        .await
        .unwrap();
      let on_phone = wait_for_preview_picture(&page).await;
      assert!(
        on_phone["cssWidth"].as_f64().unwrap() <= 358.0,
        "within the phone's gutters: {on_phone}"
      );
      screenshot(&page, "dark", "share-preview-phone").await;
      click_for_real(&page, ".share-preview .btn").await;
      screenshot(&page, "dark", "share-preview-collapsed-phone").await;
      click_for_real(&page, ".share-preview .btn").await;
      wait_for_preview_picture(&page).await;

      // The share ends: the preview ends, and its hold on the capture with it.
      page.evaluate(KEEP_PREVIEW_TRACK).await.unwrap();
      click_for_real(&page, ".share-banner .btn").await;
      wait_until_true(&page, "window.previewTrack.readyState === 'ended'").await;
      wait_until_true(&page, "!document.querySelector('.share-preview')").await;
    })
    .await;
  rig.finish().await;
}

/// #440 (e2ee.md §8, "Calls and browser tabs"): calls run in the account's active tab (#744) and
/// move with it, without a reload. A second tab that took the app over calls with the same device;
/// the first tab, which stopped its call module when it handed the app away, calls again after
/// taking it back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn calls_move_with_the_active_tab() {
  let Some(mut rig) = start_rig().await else {
    return;
  };
  let certificate_hash = rig.stack.maybe_sfu.as_ref().unwrap().certificate_hash;
  let first = rig.browser.page.clone();
  sign_in_and_join_the_channel(&mut rig, &first).await;
  // The call module started in the first tab (it loads a moment after the app).
  wait_until_true(&first, "navigator.mediaDevices.ondevicechange !== null").await;

  let second = rig
    .browser
    .new_page(&format!("{WEB_ORIGIN}/?backend=server"))
    .await;
  wait_for_text(&second, "TeamCraft is already open in another tab").await;
  click_text(&second, "Use here").await;
  wait_for_text(&first, "TeamCraft was opened in another tab").await;
  // The tab that handed the app away stopped its call module, listeners included.
  wait_until_true(&first, "navigator.mediaDevices.ondevicechange === null").await;
  sign_in_again(&second).await;
  wait_for_text(&second, "browser-channel").await;
  call_and_leave(&mut rig, &second, certificate_hash).await;

  first.bring_to_front().await.unwrap();
  click_text(&first, "Use here").await;
  wait_for_text(&second, "TeamCraft was opened in another tab").await;
  wait_for_text(&first, "browser-channel").await;
  call_and_leave(&mut rig, &first, certificate_hash).await;
  rig.finish().await;
}

/// The browser in `page` starts a video call in the test channel, Carol joins it natively until
/// the DAVE group with the browser stands, then both leave.
async fn call_and_leave(rig: &mut Rig, page: &Page, certificate_hash: [u8; 32]) {
  // Input goes to the tab in front.
  page.bring_to_front().await.unwrap();
  click_text(page, "browser-channel").await;
  click_selector(page, "button[aria-label='Start video call']").await;
  wait_for_element(page, ".prejoin").await;
  click_selector(page, ".prejoin-actions .btn-primary").await;
  wait_for_element(page, ".call-screen").await;

  let mut carol_call = join_call(&mut rig.carol, rig.channel, certificate_hash, 550.0).await;
  let mut is_secured = false;
  let deadline = tokio::time::Instant::now() + SETUP_TIMEOUT;
  while let Ok(Some(event)) = tokio::time::timeout_at(deadline, carol_call.next_event()).await {
    assert!(
      !matches!(
        event,
        CallEngineEvent::UntrustedRoster | CallEngineEvent::UnauthenticatedKeys | CallEngineEvent::Error(_)
      ),
      "{event:?}"
    );
    if matches!(event, CallEngineEvent::Secured { .. }) {
      is_secured = true;
      break;
    }
  }
  carol_call.leave().await.expect("leaving the call");
  assert!(
    is_secured,
    "the DAVE group with the browser was never established"
  );
  click_selector(page, ".call-screen .leave-btn").await;
  wait_until_true(page, "document.querySelector('.call-screen') === null").await;
}

/// A rig with calls, whose browser has fake devices.
async fn start_rig() -> Option<Rig> {
  // The SFU's certificate is self-signed and short-lived; the server hands out its hash with join
  // tokens, which browsers accept (`serverCertificateHashes`), like the native clients' pin.
  let SfuIdentity {
    certificate_pem: certificate,
    key_pem: key,
    sha256_hex: sha256,
  } = SfuIdentity::generate();
  Rig::start_with(
    |builder| builder.with_calls().with_sfu_identity(certificate, key, sha256),
    &MEDIA_ARGS,
  )
  .await
}

/// [`start_rig`] with the SFU's WebSocket fallback on offer.
async fn start_rig_with_fallback() -> Option<Rig> {
  let SfuIdentity {
    certificate_pem: certificate,
    key_pem: key,
    sha256_hex: sha256,
  } = SfuIdentity::generate();
  Rig::start_with(
    |builder| {
      builder
        .with_call_fallback()
        .with_sfu_identity(certificate, key, sha256)
    },
    &MEDIA_ARGS,
  )
  .await
}

/// Signs the browser in as a new device of Alice, which her other device commits into the channel;
/// Carol must have verified it before a call admits it.
async fn sign_in_and_join_the_channel(rig: &mut Rig, page: &Page) {
  let channel = rig.channel;
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
  sign_in_as_alice(page).await;
  wait_for_text(page, "browser-channel").await;
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
}

/// Remembers the track the preview plays, to look at it after it left the page.
const KEEP_PREVIEW_TRACK: &str =
  "window.previewTrack = document.querySelector('.share-preview video').srcObject.getVideoTracks()[0]";
const PREVIEW_STATE: &str = r"(() => {
  const video = document.querySelector('.share-preview video');
  if (!video || !video.srcObject || video.videoWidth === 0) return null;
  return {
    time: video.currentTime,
    cssWidth: video.getBoundingClientRect().width,
    frameRate: video.srcObject.getVideoTracks()[0].getSettings().frameRate,
  };
})()";

/// A look at the page for reviewing the design, in `target/e2e-artifacts`.
async fn screenshot(page: &Page, scheme: &str, name: &str) {
  page
    .execute(
      SetEmulatedMediaParams::builder()
        .feature(MediaFeature::new("prefers-color-scheme", scheme))
        .build(),
    )
    .await
    .unwrap();
  let dir = artifact_dir();
  std::fs::create_dir_all(&dir).unwrap();
  page
    .save_screenshot(
      ScreenshotParams::builder().build(),
      dir.join(format!("{name}.png")),
    )
    .await
    .unwrap();
}

/// A click with the mouse, not through the DOM: it counts as the user's gesture.
async fn click_for_real(page: &Page, selector: &str) {
  wait_for_element(page, selector).await.click().await.unwrap();
}

async fn has_element(page: &Page, selector: &str) -> bool {
  page.find_element(selector).await.is_ok()
}

async fn wait_for_preview_picture(page: &Page) -> serde_json::Value {
  let deadline = Instant::now() + SETUP_TIMEOUT;
  loop {
    let state: serde_json::Value = page
      .evaluate(PREVIEW_STATE)
      .await
      .unwrap()
      .into_value()
      .unwrap_or(serde_json::Value::Null);
    if !state.is_null() {
      return state;
    }
    assert!(Instant::now() < deadline, "the preview never showed a picture");
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
}

async fn wait_until_it_plays_on(page: &Page, time: f64) {
  wait_until_true(
    page,
    &format!("document.querySelector('.share-preview video').currentTime > {time}"),
  )
  .await;
}

async fn wait_until_true(page: &Page, expression: &str) {
  let deadline = Instant::now() + Duration::from_secs(20);
  loop {
    let is_true = page
      .evaluate(expression)
      .await
      .ok()
      .and_then(|result| result.into_value::<bool>().ok())
      .unwrap_or(false);
    if is_true {
      return;
    }
    assert!(Instant::now() < deadline, "never true: {expression}");
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
}

/// Waits until the call with the browser is secured and its camera is published; returns the DAVE
/// epoch once it settled, and the camera's stream.
async fn wait_for_secured_camera(call: &mut NativeCall) -> (u64, MediaStreamId) {
  let (mut maybe_epoch, mut maybe_camera) = (None, None);
  let deadline = tokio::time::Instant::now() + SETUP_TIMEOUT;
  while maybe_epoch.is_none() || maybe_camera.is_none() {
    let event = tokio::time::timeout_at(deadline, call.next_event())
      .await
      .expect("the call with the browser was never secured with its camera")
      .expect("the call ended");
    match event {
      CallEngineEvent::Secured { epoch, .. } => maybe_epoch = Some(epoch),
      CallEngineEvent::VideoPublished(track) if track.kind == VideoTrackKind::Camera => {
        maybe_camera = Some(track.stream_id);
      },
      _ => {},
    }
  }
  // Commits that were on their way still count.
  let settled = tokio::time::Instant::now() + Duration::from_secs(3);
  while let Ok(Some(event)) = tokio::time::timeout_at(settled, call.next_event()).await {
    if let CallEngineEvent::Secured { epoch, .. } = event {
      maybe_epoch = Some(epoch);
    }
  }
  (maybe_epoch.unwrap(), maybe_camera.unwrap())
}

/// Fails on anything that shows a device leaving the call or the group re-keying: a move to another
/// connection is neither.
fn assert_undisturbed(event: &CallEngineEvent, epoch: u64) {
  match event {
    CallEngineEvent::ParticipantReconnecting(_)
    | CallEngineEvent::ParticipantLeft(_)
    | CallEngineEvent::ParticipantJoined(_)
    | CallEngineEvent::Reconnecting
    | CallEngineEvent::UntrustedRoster
    | CallEngineEvent::UnauthenticatedKeys
    | CallEngineEvent::Error(_) => panic!("the call was disturbed: {event:?}"),
    CallEngineEvent::Secured { epoch: new, .. } if *new != epoch => {
      panic!("re-keyed from epoch {epoch} to {new}")
    },
    _ => {},
  }
}

/// Prints the page's console lines that contain one of `needles`.
fn dump_console(console: &Mutex<Vec<String>>, needles: &[&str]) {
  for line in console
    .lock()
    .unwrap()
    .iter()
    .filter(|line| needles.iter().any(|needle| line.contains(needle)))
  {
    eprintln!("console: {line}");
  }
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
