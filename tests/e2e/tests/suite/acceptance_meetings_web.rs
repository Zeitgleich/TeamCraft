//! v1 acceptance, PLAN.md §9 and §12, issue #987: an external guest follows the link from the
//! invitation mail in a browser, waits in the lobby, the organizer's device lets the guest's own
//! device into the meeting chat's encryption, and the guest's browser and the organizer share the
//! meeting's call (e2ee.md §4.6, "Meeting guests"); and (#1231) a member, not the organizer, lets
//! the guest in; and (#1720) a co-host lets the guest in while the organizer is gone.
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chromiumoxide::cdp::browser_protocol::browser::{
  PermissionDescriptor, PermissionSetting, SetPermissionParams,
};
use chromiumoxide::page::Page;
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_core::client::{HostsChange, admits_link_arrivals};
use teamcraft_e2e::browser::{BrowserSession, web_dir};
use teamcraft_e2e::call::{SfuIdentity, join_call};
use teamcraft_e2e::peer::settle;
use teamcraft_e2e::{Mailpit, Peer, Stack};
use teamcraft_media::call_engine::CallEngineEvent;
use teamcraft_media::call_native::NativeCall;
use teamcraft_proto::{ConversationId, MeetingId, UserId};

use crate::web::{TEST_CSP, click_text, type_until_kept, wait_for_element, wait_for_text};
use crate::web_calls::{MEDIA_ARGS, click_selector, console_log};

/// Ends a few minutes from now, so the call may start (a meeting that is over can't be joined).
const MEETING_LENGTH_MS: i64 = 60 * 60_000;
/// How long after the meeting's end the guest keeps access (the server's `guest_grace_secs`, as the
/// host's app passes it).
const GUEST_ACCESS_GRACE_MS: i64 = 2 * 60 * 60_000;
const SETUP_TIMEOUT: Duration = Duration::from_secs(60);
const CALL_DURATION: Duration = Duration::from_secs(8);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_guest_joins_a_meeting_with_the_mailed_link_and_shares_its_call() {
  let Some(Env {
    stack,
    browser,
    certificate_hash,
  }) = Env::start().await
  else {
    return;
  };
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
  grant_media(&page, &stack.base_url).await;
  browser
    .screenshot_on_failure("meeting-guest-mail-link", |page| async move {
      knock(&page, &origin, link, "Gus (customer)").await;
      // Nobody let the guest in yet: the normal case, said plainly.
      wait_for_text(&page, "Waiting for someone to let you in").await;
      wait_for_element(&page, ".guest-waiting-devices .prejoin-preview").await;
      expect_no_panic(&page).await;
      guest_shots(&page, "waiting").await;

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
      join_call_in_browser(&page).await;
    })
    .await;

  // The organizer joins natively: both verify each other's DAVE keys, media flows both ways.
  let call = join_call(&mut alice, chat, certificate_hash, 550.0).await;
  expect_secured_call(call, &console, "the organizer").await;

  // The guest hangs up: their part is over, said warmly, with a way back in (#1933).
  browser
    .screenshot_on_failure("meeting-guest-end", |page| async move {
      click_selector(&page, ".call-screen .leave-btn").await;
      wait_for_text(&page, "Thanks for joining.").await;
      wait_for_text(&page, "Rejoin").await;
      expect_no_panic(&page).await;
      guest_shots(&page, "end").await;
    })
    .await;

  browser.close().await;
  stack.stop().await;
}

/// #1231: the organizer scheduled without a lobby and left; a member stays and his app, as the
/// organizer's signed admitters allow, lets the browser guest in, who then shares the call with
/// him, DAVE-secured. (As in the app, the lobby is read and answered by the member's own device;
/// "while in the meeting's call" is the app's gate, the server and core accept the member any time.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_admits_a_browser_guest_when_the_organizer_has_left_and_they_share_the_call() {
  let Some(Env {
    stack,
    browser,
    certificate_hash,
  }) = Env::start().await
  else {
    return;
  };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut bob = Peer::enroll(&stack, "bob").await;
  let bob_id = bob.user_id();
  let starts_at = now_ms() - 60_000;
  let ends_at = starts_at + MEETING_LENGTH_MS;
  let meeting = stack
    .post(
      &alice.tokens,
      "/meetings",
      json!({
        "title": "Open call", "starts_at": starts_at, "ends_at": ends_at,
        "has_lobby": false,
        "invitees": [{"kind": "user", "user_id": bob_id.to_string()}],
      }),
    )
    .await
    .expect(StatusCode::CREATED);
  let meeting_id: MeetingId = meeting.id().parse().unwrap();
  let chat: ConversationId = meeting.body["conversation_id"].as_str().unwrap().parse().unwrap();
  let link = meeting.body["join_url"].as_str().unwrap().to_owned();

  // The organizer's app sets the chat's group up with Bob in it, chooses "no lobby", signs Bob
  // as an admitter, and is gone from here on.
  alice.client.set_up_meeting_chat(chat, &[bob_id]).await.unwrap();
  alice.client.note_meeting_lobby(meeting_id, false).unwrap();
  let access_ends_at = ends_at + GUEST_ACCESS_GRACE_MS;
  assert!(
    alice
      .client
      .sync_meeting_admitters(meeting_id, chat, access_ends_at)
      .await
      .unwrap()
  );
  settle(&mut [&mut alice, &mut bob], Duration::from_secs(1)).await;
  bob.client.full_sync().await.unwrap();
  assert!(bob.client.has_group(chat).unwrap());
  bob.client.check_meeting_admission(chat).await.unwrap();
  assert!(admits_link_arrivals(bob.store.as_ref(), chat, now_ms()));
  drop(alice);

  let origin = stack.base_url.clone();
  let lobby_path = format!("/meetings/{meeting_id}/lobby");
  let page = browser.page.clone();
  let console = console_log(&page).await;
  grant_media(&page, &origin).await;
  let (stack_ref, bob_ref) = (&stack, &mut bob);
  browser
    .screenshot_on_failure("meeting-member-admits-guest", |page| async move {
      knock(&page, &origin, link, "Gus (customer)").await;
      wait_for_text(&page, "Waiting for someone to let you in").await;

      // Bob's app sees the guest, who came with the link, once the guest's device is ready.
      let deadline = Instant::now() + SETUP_TIMEOUT;
      let entry = loop {
        let lobby = stack_ref
          .get(&bob_ref.tokens, &lobby_path)
          .await
          .expect(StatusCode::OK);
        assert_eq!(lobby.body["is_host"], false);
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
      assert_eq!(entry["is_by_link"], true);
      let guest: UserId = entry["user"]["id"].as_str().unwrap().parse().unwrap();
      let fingerprint = entry["knock_fingerprint"].as_str().unwrap().to_owned();

      // Bob's app admits on the server and adds the guest's device to the chat's group.
      stack_ref
        .post(&bob_ref.tokens, &format!("{lobby_path}/{guest}/admit"), json!({}))
        .await
        .expect(StatusCode::NO_CONTENT);
      bob_ref
        .client
        .let_guest_into_meeting_chat(chat, guest, &fingerprint, access_ends_at)
        .await
        .unwrap();
      join_call_in_browser(&page).await;
    })
    .await;

  // Bob joins natively: he and the guest verify each other's DAVE keys, media flows to him.
  let call = join_call(&mut bob, chat, certificate_hash, 550.0).await;
  expect_secured_call(call, &console, "the admitting member").await;

  browser.close().await;
  stack.stop().await;
}

/// #1720: the organizer named a co-host and left the meeting; a guest knocks in the lobby, the
/// co-host's device lets the guest in, and the guest's browser accepts it by the host list the
/// organizer signed (checked from the organizer's key in the link, `v`), not by the server's word.
/// Then the guest shares the call with the co-host, DAVE-secured.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_co_host_admits_a_browser_guest_when_the_organizer_has_left() {
  let Some(Env {
    stack,
    browser,
    certificate_hash,
  }) = Env::start().await
  else {
    return;
  };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut bob = Peer::enroll(&stack, "bob").await;
  let bob_id = bob.user_id();
  let starts_at = now_ms() - 60_000;
  let ends_at = starts_at + MEETING_LENGTH_MS;
  let meeting = stack
    .post(
      &alice.tokens,
      "/meetings",
      json!({
        "title": "Hosted call", "starts_at": starts_at, "ends_at": ends_at,
        "has_lobby": true,
        "invitees": [{"kind": "user", "user_id": bob_id.to_string()}],
      }),
    )
    .await
    .expect(StatusCode::CREATED);
  let meeting_id: MeetingId = meeting.id().parse().unwrap();
  let chat: ConversationId = meeting.body["conversation_id"].as_str().unwrap().parse().unwrap();
  let link = meeting.body["join_url"].as_str().unwrap().to_owned();

  // The organizer's app sets the chat's group up with Bob in it, names Bob co-host (a host list
  // signed with Alice's identity key) and is gone from here on. No admitters are signed: only the
  // host list makes Bob's device one the guest accepts.
  alice.client.set_up_meeting_chat(chat, &[bob_id]).await.unwrap();
  alice
    .client
    .change_meeting_hosts(&HostsChange {
      meeting: meeting_id,
      conversation: chat,
      co_hosts: vec![bob_id],
      maybe_new_organizer: None,
    })
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob], Duration::from_secs(1)).await;
  bob.client.full_sync().await.unwrap();
  assert!(bob.client.has_group(chat).unwrap());
  drop(alice);

  let origin = stack.base_url.clone();
  let lobby_path = format!("/meetings/{meeting_id}/lobby");
  let page = browser.page.clone();
  let console = console_log(&page).await;
  grant_media(&page, &origin).await;
  let access_ends_at = ends_at + GUEST_ACCESS_GRACE_MS;
  let (stack_ref, bob_ref) = (&stack, &mut bob);
  browser
    .screenshot_on_failure("meeting-co-host-admits-guest", |page| async move {
      knock(&page, &origin, link, "Gus (customer)").await;
      wait_for_text(&page, "Waiting for someone to let you in").await;

      // The co-host hosts the lobby: the server lists the guest to Bob as to a host.
      let deadline = Instant::now() + SETUP_TIMEOUT;
      let entry = loop {
        let lobby = stack_ref
          .get(&bob_ref.tokens, &lobby_path)
          .await
          .expect(StatusCode::OK);
        assert_eq!(lobby.body["is_host"], true, "{}", lobby.body);
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

      // Bob admits on the server and adds the guest's device to the chat's group; the guest's
      // page checks Bob's device against the organizer's signed host list and opens the meeting.
      stack_ref
        .post(&bob_ref.tokens, &format!("{lobby_path}/{guest}/admit"), json!({}))
        .await
        .expect(StatusCode::NO_CONTENT);
      bob_ref
        .client
        .let_guest_into_meeting_chat(chat, guest, &fingerprint, access_ends_at)
        .await
        .unwrap();
      join_call_in_browser(&page).await;
    })
    .await;

  let call = join_call(&mut bob, chat, certificate_hash, 550.0).await;
  expect_secured_call(call, &console, "the co-host").await;

  browser.close().await;
  stack.stop().await;
}

/// The stack with its SFU and web build, and a Chromium for the guest.
struct Env {
  stack: Stack,
  browser: BrowserSession,
  certificate_hash: [u8; 32],
}

impl Env {
  async fn start() -> Option<Self> {
    let Some(web_dir) = web_dir() else {
      let is_required = std::env::var("E2E_REQUIRED").is_ok_and(|value| value == "1");
      assert!(!is_required, "no web build: run tests/e2e/web/build.sh");
      eprintln!("skipping: no web build (tests/e2e/web/build.sh)");
      return None;
    };
    let SfuIdentity {
      certificate_pem: certificate,
      key_pem: key,
      sha256_hex: sha256,
    } = SfuIdentity::generate();
    let stack = Stack::builder()
      .with_meetings()
      .with_calls()
      .with_sfu_identity(certificate, key, sha256)
      .with_web_dir(web_dir)
      // The SFU listens on a random port of its own: the test policy lets the page reach it.
      .with_env("TEAMCRAFT_WEB__CONTENT_SECURITY_POLICY", TEST_CSP)
      .start()
      .await?;
    let browser = BrowserSession::launch_with(&MEDIA_ARGS).await?;
    let certificate_hash = stack.maybe_sfu.as_ref().unwrap().certificate_hash;
    Some(Self {
      stack,
      browser,
      certificate_hash,
    })
  }
}

/// Headless Chromium denies the microphone and camera unless the origin has them.
async fn grant_media(page: &Page, origin: &str) {
  for name in ["microphone", "camera"] {
    page
      .execute(
        SetPermissionParams::builder()
          .permission(PermissionDescriptor::new(name))
          .setting(PermissionSetting::Granted)
          .origin(origin)
          .build()
          .unwrap(),
      )
      .await
      .unwrap();
  }
}

/// The guest opens the meeting link and asks to join under `name`.
async fn knock(page: &Page, origin: &str, link: String, name: &str) {
  // The UI follows the browser language; pin English (same origin, before the app loads).
  page.goto(format!("{origin}/healthz")).await.unwrap();
  page
    .evaluate("localStorage.setItem('teamcraft.locale', 'en')")
    .await
    .unwrap();
  page.goto(link).await.unwrap();
  // One screen (#1928): the name under the preview, the devices turned on, "Join now" knocks.
  wait_for_text(page, "Join the meeting").await;
  wait_for_element(page, "#guest-name").await;
  type_until_kept(page, "#guest-name", name).await;
  click_text(page, "Turn on camera and microphone").await;
  wait_for_element(page, ".prejoin .meter").await;
  // The camera's picture is there too: the preview is live, not only the placeholder.
  wait_for_element(page, ".prejoin-preview .live-video").await;
  expect_no_panic(page).await;
  guest_shots(page, "setup").await;
  click_text(page, "Join now").await;
}

/// The guest, let in, is in the meeting's call: admission joins it at once with the devices the
/// guest set up before knocking (#1929, #1928), without a second pre-join screen.
async fn join_call_in_browser(page: &Page) {
  wait_for_element(page, ".call-screen").await;
  expect_no_panic(page).await;
  // The chat behind the call offers a guest no second way to start a call.
  let call_starts = page
    .evaluate(r#"document.querySelectorAll('.channel-header [aria-label="Start audio call"]').length"#)
    .await
    .unwrap()
    .into_value::<u64>()
    .unwrap();
  assert_eq!(call_starts, 0, "the guest's chat header offers to start a call");
}

/// The page never showed the reload overlay of a panic (`panic_overlay.rs`).
async fn expect_no_panic(page: &Page) {
  let overlays = page
    .evaluate("document.querySelectorAll('.panic-overlay').length")
    .await
    .unwrap()
    .into_value::<u64>()
    .unwrap();
  assert_eq!(overlays, 0, "the guest's page panicked");
}

/// With `E2E_GUEST_SHOTS` set, pictures of the guest's screen for a pull request: desktop and
/// the narrowest phone, light and dark, in `target/e2e-artifacts/guest-<name>-<width>-<scheme>.png`.
async fn guest_shots(page: &Page, name: &str) {
  use chromiumoxide::cdp::browser_protocol::emulation::{
    ClearDeviceMetricsOverrideParams, MediaFeature, SetDeviceMetricsOverrideParams, SetEmulatedMediaParams,
  };
  use chromiumoxide::page::ScreenshotParams;
  if std::env::var_os("E2E_GUEST_SHOTS").is_none() {
    return;
  }
  let dir = teamcraft_e2e::browser::artifact_dir();
  let _ = std::fs::create_dir_all(&dir);
  for (width, height, is_phone) in [(1440, 900, false), (320, 640, true)] {
    page
      .execute(SetDeviceMetricsOverrideParams::new(width, height, 2.0, is_phone))
      .await
      .unwrap();
    for scheme in ["light", "dark"] {
      page
        .execute(
          SetEmulatedMediaParams::builder()
            .feature(MediaFeature::new("prefers-color-scheme", scheme))
            .build(),
        )
        .await
        .unwrap();
      // Let the layout and the theme's colors settle (no transition is longer than this).
      tokio::time::sleep(Duration::from_millis(600)).await;
      let _ = page
        .save_screenshot(
          ScreenshotParams::builder().build(),
          dir.join(format!("guest-{name}-{width}-{scheme}.png")),
        )
        .await;
    }
  }
  page
    .execute(ClearDeviceMetricsOverrideParams::default())
    .await
    .unwrap();
}

/// The native peer listens for [`CALL_DURATION`], then leaves: the DAVE group with the guest was
/// established and the guest's fake microphone (a beep) reached it and decoded.
async fn expect_secured_call(mut call: NativeCall, console: &Arc<Mutex<Vec<String>>>, who: &str) {
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
  assert!(
    is_secured,
    "the DAVE group with the guest was never established ({who})"
  );
  assert!(stats.frames_received > 50, "{who}: {stats:?}");
}

fn now_ms() -> i64 {
  let now = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default();
  i64::try_from(now.as_millis()).unwrap_or(i64::MAX)
}
