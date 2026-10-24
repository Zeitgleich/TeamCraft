//! The web client in headless Chromium: sign in through the real Keycloak login page and see the
//! conversations of the account. Failures leave screenshots in `target/e2e-artifacts/`.
//!
//! The web build is served by the server under test at `http://127.0.0.1:8082`, the origin the dev
//! realm registers as redirect URI; these tests therefore need that port and run one at a time.

use std::time::{Duration, Instant};

use chromiumoxide::page::Page;
use teamcraft_e2e::browser::{BrowserSession, WebPortLease, lease_web_port, web_dir};
use teamcraft_e2e::peer::PASSPHRASE;
use teamcraft_e2e::scenario::{channel_with, space_with};
use teamcraft_e2e::{Peer, Stack};
use teamcraft_proto::ConversationId;

pub(crate) const WEB_ORIGIN: &str = "http://127.0.0.1:8082";
/// The default policy only lets pages talk to https: and wss: origins; the dev Keycloak is plain http.
pub(crate) const TEST_CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; \
  style-src 'self'; img-src 'self' data: blob:; media-src 'self' blob:; font-src 'self'; \
  connect-src 'self' http://127.0.0.1:8080 ws: wss: https:; worker-src 'self' blob:; \
  frame-ancestors 'none'; base-uri 'none'; form-action 'self'";
const WAIT: Duration = Duration::from_secs(30);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_web_client_signs_in_through_keycloak_and_shows_the_accounts_channels() {
  let Some(rig) = Rig::start().await else { return };
  rig
    .browser
    .screenshot_on_failure("web-sign-in", |page| async move {
      sign_in_as_alice(&page).await;
      // The sidebar lists the channel.
      wait_for_text(&page, "browser-channel").await;
    })
    .await;
  rig.finish().await;
}

/// The v1 UI on the real core: mention picker, slash commands, emoticons, bookmarks, profile and the
/// people directory. Screenshots of each step land in `target/e2e-artifacts/v1-*.png`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_web_client_offers_the_v1_chat_features() {
  let Some(rig) = Rig::start().await else { return };
  rig
    .browser
    .screenshot_on_failure("web-v1-features", |page| async move {
      sign_in_as_alice(&page).await;
      wait_for_text(&page, "browser-channel").await;
      click_text(&page, "browser-channel").await;
      let composer = wait_for_element(&page, "textarea").await;

      // The mention picker completes people (Carol is in the channel).
      composer
        .click()
        .await
        .unwrap()
        .type_str("hello @car")
        .await
        .unwrap();
      wait_for_text(&page, "@carol").await;
      shot(&page, "v1-mention-picker").await;
      composer.press_key("Enter").await.unwrap();
      assert_eq!(textarea_value(&page).await, "hello @carol ");

      // Emoticons become emoji on send.
      composer
        .type_str(":)")
        .await
        .unwrap()
        .press_key("Enter")
        .await
        .unwrap();
      wait_for_text(&page, "hello @carol 🙂").await;

      // Slash commands run on the client.
      composer
        .type_str("/shrug")
        .await
        .unwrap()
        .press_key("Enter")
        .await
        .unwrap();
      wait_for_text(&page, "¯\\_(ツ)_/¯").await;
      composer
        .type_str("/remind in 5m stand up")
        .await
        .unwrap()
        .press_key("Enter")
        .await
        .unwrap();
      wait_for_text(&page, "Reminder set").await;
      shot(&page, "v1-slash-commands").await;

      // Bookmarks: add a link through the editor.
      click_text(&page, "Edit bookmarks").await;
      wait_for_element(&page, ".bookmark-edit-list").await;
      click_text(&page, "Add bookmark").await;
      set_value(&page, "input[aria-label=Title]", "Runbook").await;
      set_value(&page, "input[aria-label=Link]", "https://example.com/runbook").await;
      shot(&page, "v1-bookmark-editor").await;
      click_selector(&page, ".modal-foot button[type=submit]").await;
      wait_for_text(&page, "Runbook").await;
      shot(&page, "v1-bookmarks-bar").await;

      // The profile form saves, and the directory shows it.
      page
        .evaluate("document.querySelector('.avatar-btn').click()")
        .await
        .unwrap();
      click_text(&page, "Edit profile").await;
      let title = wait_for_element(&page, "#profile-title-field").await;
      title
        .click()
        .await
        .unwrap()
        .type_str("Release engineer")
        .await
        .unwrap();
      shot(&page, "v1-profile-form").await;
      click_selector(&page, ".modal-foot button[type=submit]").await;
      click_text(&page, "People").await;
      wait_for_text(&page, "Release engineer").await;
      shot(&page, "v1-people").await;
    })
    .await;
  rig.finish().await;
}

/// The service worker carries the Rust push code: it loads in the browser and instantiates.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_service_worker_loads_its_push_code() {
  let Some(rig) = Rig::start().await else { return };
  rig
    .browser
    .screenshot_on_failure("web-service-worker", |page| async move {
      sign_in_as_alice(&page).await;
      wait_for_text(&page, "browser-channel").await;
      let script = r#"(async () => {
        const registration = await navigator.serviceWorker.ready;
        return await new Promise((resolve) => {
          navigator.serviceWorker.addEventListener('message', (event) => {
            if (event.data && event.data.type === 'push') resolve(JSON.stringify(event.data));
          });
          registration.active.postMessage({ type: 'push_probe' });
        });
      })()"#;
      let answer: String = page.evaluate(script).await.unwrap().into_value().unwrap();
      assert!(
        answer.contains("\"ready\":true"),
        "the push code did not load: {answer}"
      );
    })
    .await;
  rig.finish().await;
}

/// The goal of the browser e2e: Alice writes in the web client, Carol's client receives it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_message_written_in_the_web_client_reaches_a_second_client() {
  let Some(mut rig) = Rig::start().await else { return };
  let channel = rig.channel;
  let alice = &mut rig.alice;
  let carol = &mut rig.carol;
  let browser = &rig.browser;
  let outcome = async {
    // The peers stay connected while the browser signs in: the new device is announced on their
    // gateways, and the first-ranked other device of Alice commits it (a staggered commit that
    // needs a nudge). Meanwhile the browser's queued message waits in its outbox.
    let console_log = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let console_sink = console_log.clone();
    let page_work = browser.screenshot_on_failure("web-send-message", |page| async move {
      let mut events = page
        .event_listener::<chromiumoxide::cdp::js_protocol::runtime::EventConsoleApiCalled>()
        .await
        .unwrap();
      tokio::spawn(async move {
        use futures_util::StreamExt;
        while let Some(event) = events.next().await {
          let text: Vec<String> = event
            .args
            .iter()
            .map(|arg| {
              arg
                .value
                .as_ref()
                .map_or_else(|| arg.description.clone().unwrap_or_default(), |v| v.to_string())
            })
            .collect();
          console_sink.lock().unwrap().push(text.join(" "));
        }
      });
      sign_in_as_alice(&page).await;
      wait_for_text(&page, "browser-channel").await;
      click_text(&page, "browser-channel").await;
      let composer = wait_for_element(&page, "textarea").await;
      composer
        .click()
        .await
        .unwrap()
        .type_str("hello from the browser")
        .await
        .unwrap()
        .press_key("Enter")
        .await
        .unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(120);
    let delivery = async {
      let mut round = 0_u32;
      while !carol
        .texts(channel)
        .iter()
        .any(|text| text == "hello from the browser")
      {
        if Instant::now() >= deadline {
          for line in console_log.lock().unwrap().iter() {
            eprintln!("console: {line}");
          }
          panic!("the message of the browser never reached Carol");
        }
        if round.is_multiple_of(50) {
          alice.heartbeat().await;
          carol.heartbeat().await;
        }
        round += 1;
        alice.pump_reconnecting().await;
        alice.client.apply_pending_changes().await.unwrap();
        carol.pump_reconnecting().await;
        carol.client.apply_pending_changes().await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
      }
    };
    let (_, ()) = tokio::join!(page_work, delivery);
  };
  outcome.await;
  rig.finish().await;
}

/// e2ee.md §9.4: two tabs of the same account share one core. Messages sent alternately from both
/// tabs each reach Carol exactly once and decrypt (one MLS state, one outbox), and when the owning tab
/// closes, the other one takes the store over and keeps working.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_tabs_share_one_core_and_the_second_takes_over() {
  let Some(mut rig) = Rig::start().await else { return };
  let channel = rig.channel;
  let first = rig.browser.page.clone();
  rig
    .browser
    .screenshot_on_failure("web-two-tabs-first", |page| async move {
      sign_in_as_alice(&page).await;
      wait_for_text(&page, "browser-channel").await;
    })
    .await;
  rig
    .deliver_until(|texts| texts.iter().any(|text| text == "hello from carol"))
    .await;

  let second = rig
    .browser
    .new_page(&format!("{WEB_ORIGIN}/?backend=server"))
    .await;
  sign_in_again(&second).await;
  wait_for_text(&second, "browser-channel").await;
  let follows = second
    .evaluate("navigator.locks.query().then(state => state.held.length + ':' + state.pending.length)")
    .await
    .unwrap()
    .into_value::<String>()
    .unwrap();
  assert_eq!(
    follows, "1:1",
    "one tab holds the store lock, the other waits for it"
  );

  let sent = ["tab one: 1", "tab two: 1", "tab one: 2", "tab two: 2"];
  for (index, text) in sent.iter().enumerate() {
    let page = if index % 2 == 0 { &first } else { &second };
    send_in_channel(page, text).await;
    rig
      .deliver_until(|texts| texts.iter().any(|seen| seen == text))
      .await;
  }
  // Every message arrived once, in order, and Carol could decrypt each of them.
  let texts = rig.carol.texts(channel);
  let seen: Vec<&str> = texts
    .iter()
    .map(String::as_str)
    .filter(|text| text.starts_with("tab "))
    .collect();
  assert_eq!(seen, sent);
  assert_eq!(
    seen,
    sent
      .iter()
      .map(|text| (*text).to_owned())
      .collect::<Vec<_>>()
      .iter()
      .collect::<Vec<_>>()
  );

  // The owner goes away: the follower opens the store and sends on its own.
  first.close().await.unwrap();
  send_in_channel(&second, "tab two after the first closed").await;
  rig
    .deliver_until(|texts| texts.iter().any(|text| text == "tab two after the first closed"))
    .await;
  let texts = rig.carol.texts(channel);
  assert_eq!(
    texts
      .iter()
      .filter(|text| *text == "tab two after the first closed")
      .count(),
    1
  );
  rig.finish().await;
}

/// e2ee.md §9.4: when the browser refuses writes for lack of space, the user is told that data is not
/// saved, and the warning goes away once the writes went through. Chrome's DevTools quota override
/// does not reach IndexedDB reliably, so the page's `put` refuses like a full disk does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_quota_is_shown_and_recovers() {
  let Some(rig) = Rig::start().await else { return };
  rig
    .browser
    .screenshot_on_failure("web-quota", |page| async move {
      sign_in_as_alice(&page).await;
      wait_for_text(&page, "browser-channel").await;
      page
        .evaluate(
          "(() => { const put = IDBObjectStore.prototype.put; window.__teamcraftPut = put; \
           IDBObjectStore.prototype.put = function () { \
             throw new DOMException('The quota has been exceeded.', 'QuotaExceededError'); }; })()",
        )
        .await
        .unwrap();
      send_in_channel(&page, "written without space").await;
      wait_for_text(&page, "no storage space left").await;
      shot(&page, "web-quota-banner").await;

      // Space is free again: the kept writes go through on the next flush.
      page
        .evaluate("IDBObjectStore.prototype.put = window.__teamcraftPut")
        .await
        .unwrap();
      wait_for_text_gone(&page, "no storage space left").await;
    })
    .await;
  rig.finish().await;
}

/// The stack with the web build, the browser and the data the browser will show: Alice owns a
/// channel she shares with Carol.
pub(crate) struct Rig {
  stack: Stack,
  pub(crate) browser: BrowserSession,
  alice: Peer,
  carol: Peer,
  channel: ConversationId,
  _turn: WebPortLease,
}

impl Rig {
  pub(crate) async fn start() -> Option<Self> {
    let Some(web_dir) = web_dir() else {
      let is_required = std::env::var("E2E_REQUIRED").is_ok_and(|value| value == "1");
      assert!(!is_required, "no web build: run tests/e2e/web/build.sh");
      eprintln!("skipping: no web build (tests/e2e/web/build.sh)");
      return None;
    };
    let builder = Stack::builder()
      .with_port(8082)
      .with_web_dir(web_dir)
      .with_env("TEAMCRAFT_WEB__CONTENT_SECURITY_POLICY", TEST_CSP);
    // One test at a time owns the port.
    let turn = lease_web_port(8082).await;
    let stack = builder.start().await?;
    let browser = BrowserSession::launch().await?;

    let mut alice = Peer::enroll(&stack, "alice").await;
    let mut carol = Peer::enroll(&stack, "carol").await;
    let space = space_with(&stack, &alice, "Browser Space", &[(&carol, "member")]).await;
    let channel = channel_with(&mut alice, space, "browser-channel", &mut [&mut carol]).await;
    // A device that enrolls later (the browser) gets the channel's content key from the key bag, so
    // the key bag must be on the server by then; the app's runner does this on its tick.
    alice.client.sync_account_data().await.unwrap();
    carol
      .client
      .send_text(channel, None, "hello from carol")
      .await
      .unwrap();
    Some(Self {
      stack,
      browser,
      alice,
      carol,
      channel,
      _turn: turn,
    })
  }

  pub(crate) async fn finish(self) {
    self.browser.close().await;
    self.stack.stop().await;
  }

  /// Pumps Alice's and Carol's clients (Alice's other device commits the browser into the channel)
  /// until Carol's texts satisfy `is_done`.
  async fn deliver_until(&mut self, is_done: impl Fn(&[String]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut round = 0_u32;
    while !is_done(&self.carol.texts(self.channel)) {
      assert!(
        Instant::now() < deadline,
        "Carol did not get the expected messages; she has {:?}",
        self.carol.texts(self.channel)
      );
      if round.is_multiple_of(50) {
        self.alice.heartbeat().await;
        self.carol.heartbeat().await;
      }
      round += 1;
      self.alice.pump_reconnecting().await;
      self.alice.client.apply_pending_changes().await.unwrap();
      self.carol.pump_reconnecting().await;
      self.carol.client.apply_pending_changes().await.unwrap();
      tokio::time::sleep(Duration::from_millis(200)).await;
    }
  }
}

/// Signs in through Keycloak's login page and unlocks the account's keys on this new browser.
pub(crate) async fn sign_in_as_alice(page: &Page) {
  // The UI follows the browser language; pin English before the app loads (same origin).
  page.goto(format!("{WEB_ORIGIN}/healthz")).await.unwrap();
  page
    .evaluate("localStorage.setItem('teamcraft.locale', 'en')")
    .await
    .unwrap();
  page.goto(format!("{WEB_ORIGIN}/")).await.unwrap();
  wait_for_text(page, "Sign in").await;
  click_text(page, "Sign in").await;

  // Keycloak's login form.
  let username = wait_for_element(page, "input[name=username]").await;
  username.click().await.unwrap().type_str("alice").await.unwrap();
  let password = page.find_element("input[name=password]").await.unwrap();
  password
    .click()
    .await
    .unwrap()
    .type_str("alice-dev-pass")
    .await
    .unwrap()
    .press_key("Enter")
    .await
    .unwrap();

  // The account already has keys (the test enrolled it): this browser is a new device and unlocks
  // them with the recovery passphrase.
  let passphrase = wait_for_element(page, "#enroll-passphrase").await;
  passphrase
    .click()
    .await
    .unwrap()
    .type_str(PASSPHRASE)
    .await
    .unwrap();
  click_text(page, "Unlock").await;
}

/// Signs in again in another tab, if the tab did not restore the session itself; Keycloak still has
/// it, so no form comes up.
async fn sign_in_again(page: &Page) {
  let deadline = Instant::now() + WAIT;
  loop {
    let text = body_text(page).await;
    if text.contains("browser-channel") {
      return;
    }
    if text.contains("Sign in") {
      click_text(page, "Sign in").await;
      return;
    }
    assert!(
      Instant::now() < deadline,
      "the second tab shows neither the app nor sign-in:\n{text}"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
}

/// Opens the test channel and sends `text` from the composer.
async fn send_in_channel(page: &Page, text: &str) {
  // Input goes to the tab in front; a background tab never gets it.
  page.bring_to_front().await.unwrap();
  click_text(page, "browser-channel").await;
  let composer = wait_for_element(page, "textarea").await;
  composer
    .click()
    .await
    .unwrap()
    .type_str(text)
    .await
    .unwrap()
    .press_key("Enter")
    .await
    .unwrap();
  wait_for_text(page, text).await;
}

async fn wait_for_text_gone(page: &Page, needle: &str) {
  let deadline = Instant::now() + WAIT;
  while body_text(page).await.contains(needle) {
    assert!(
      Instant::now() < deadline,
      "{needle:?} is still shown after {WAIT:?}"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
}

/// Saves a screenshot as `target/e2e-artifacts/<name>.png` (kept after passing runs too).
pub(crate) async fn shot(page: &Page, name: &str) {
  use chromiumoxide::page::ScreenshotParams;
  let dir = teamcraft_e2e::browser::artifact_dir();
  let _ = std::fs::create_dir_all(&dir);
  let _ = page
    .save_screenshot(
      ScreenshotParams::builder().full_page(true).build(),
      dir.join(format!("{name}.png")),
    )
    .await;
}

async fn textarea_value(page: &Page) -> String {
  page
    .evaluate("document.querySelector('textarea').value")
    .await
    .ok()
    .and_then(|result| result.into_value::<String>().ok())
    .unwrap_or_default()
}

async fn click_selector(page: &Page, selector: &str) {
  let script = format!(
    "(() => {{ const el = document.querySelector({selector:?}); if (!el) return false; el.click(); return true; }})()"
  );
  let clicked = page
    .evaluate(script)
    .await
    .unwrap()
    .into_value::<bool>()
    .unwrap_or(false);
  assert!(clicked, "nothing to click for {selector}");
}

/// Sets an input's value the way a user edit would (the app listens for `change`).
async fn set_value(page: &Page, selector: &str, value: &str) {
  let script = format!(
    "(() => {{ const el = document.querySelector({selector:?}); if (!el) return false; el.value = {value:?}; el.dispatchEvent(new Event('input', {{bubbles: true}})); el.dispatchEvent(new Event('change', {{bubbles: true}})); return true; }})()"
  );
  let done = page
    .evaluate(script)
    .await
    .unwrap()
    .into_value::<bool>()
    .unwrap_or(false);
  assert!(done, "no input {selector}");
}

pub(crate) async fn wait_for_element(page: &Page, selector: &str) -> chromiumoxide::element::Element {
  let deadline = Instant::now() + WAIT;
  loop {
    if let Ok(element) = page.find_element(selector).await {
      return element;
    }
    assert!(Instant::now() < deadline, "no element {selector} within {WAIT:?}");
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
}

pub(crate) async fn body_text(page: &Page) -> String {
  page
    .evaluate("document.body ? document.body.innerText : ''")
    .await
    .ok()
    .and_then(|result| result.into_value::<String>().ok())
    .unwrap_or_default()
}

pub(crate) async fn wait_for_text(page: &Page, needle: &str) {
  let deadline = Instant::now() + WAIT;
  loop {
    if body_text(page).await.contains(needle) {
      return;
    }
    assert!(
      Instant::now() < deadline,
      "{needle:?} did not appear within {WAIT:?}; the page says:\n{}",
      body_text(page).await
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
}

/// Clicks the first button, link or list entry whose text contains `text`.
pub(crate) async fn click_text(page: &Page, text: &str) {
  let script = format!(
    "(() => {{ const el = [...document.querySelectorAll('button, a, li, [role=button], [role=link], [role=treeitem], [role=option]')].find(e => e.innerText.includes({text:?})); if (!el) return false; el.click(); return true; }})()"
  );
  let clicked = page
    .evaluate(script)
    .await
    .unwrap()
    .into_value::<bool>()
    .unwrap_or(false);
  assert!(clicked, "no button or link with the text {text:?}");
}
