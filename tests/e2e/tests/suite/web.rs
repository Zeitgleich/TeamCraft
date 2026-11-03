//! The web client in headless Chromium: sign in through the real Keycloak login page and see the
//! conversations of the account. Failures leave screenshots in `target/e2e-artifacts/`.
//!
//! The web build is served by the server under test at `http://127.0.0.1:8082`, the origin the dev
//! realm registers as redirect URI; these tests therefore need that port and run one at a time.

use std::time::{Duration, Instant};

use chromiumoxide::page::Page;
use teamcraft_core::api::HttpServerApi;
use teamcraft_core::client::{LinkAnnouncement, NewDeviceLink};
use teamcraft_core::{CoreError, LinkOffer};
use teamcraft_e2e::browser::{BrowserSession, WebPortLease, lease_web_port, web_dir};
use teamcraft_e2e::peer::PASSPHRASE;
use teamcraft_e2e::scenario::{channel_with, space_with};
use teamcraft_e2e::{Peer, Stack, StackBuilder};
use teamcraft_proto::{ConversationId, DeviceLinkPlatform};

pub(crate) const WEB_ORIGIN: &str = "http://127.0.0.1:8082";
/// The default policy only lets pages talk to https: and wss: origins; the dev Keycloak is plain http.
pub(crate) const TEST_CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; \
  style-src 'self'; img-src 'self' data: blob:; media-src 'self' blob:; font-src 'self'; \
  connect-src 'self' http://127.0.0.1:8080 ws: wss: https:; worker-src 'self' blob:; \
  frame-ancestors 'none'; base-uri 'none'; form-action 'self'";
const WAIT: Duration = Duration::from_secs(30);
/// Long enough for several open/close cycles of a flickering popover (#639 saw one every 0.5–1 s).
const POPOVER_SETTLE: Duration = Duration::from_millis(1500);

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
      let composer = wait_for_element(&page, "[data-composer]").await;

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
      assert_eq!(composer_text(&page).await, "hello @carol ");

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
      // Settings is a page of its own now (no modal footer): save, then close it.
      click_text(&page, "Save profile").await;
      wait_for_text(&page, "Saved").await;
      click_selector(&page, ".set-page-head button[aria-label=Close]").await;
      click_text(&page, "People").await;
      wait_for_text(&page, "Release engineer").await;
      shot(&page, "v1-people").await;
    })
    .await;
  rig.finish().await;
}

/// #639: a popover toggles exactly once per press. The send options open with a real mouse press
/// and stay open while the pointer rests on their button; a second press on the button (it lands on
/// the backdrop) closes them, and they stay closed. A watcher in the page counts every time the
/// menu is added or removed, so a quick close and reopen between two checks is caught too.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_popover_opened_once_stays_open_while_the_pointer_rests() {
  let Some(rig) = Rig::start().await else { return };
  rig
    .browser
    .screenshot_on_failure("web-popover-toggle", |page| async move {
      sign_in_as_alice(&page).await;
      wait_for_text(&page, "browser-channel").await;
      click_text(&page, "browser-channel").await;
      let button = wait_for_element(&page, ".send-menu-btn").await;
      page
        .evaluate(
          "(() => { window.__menuToggles = { added: 0, removed: 0 }; \
           const isMenu = n => n instanceof Element && n.matches('.popover.send-options:not(.motion-ghost)'); \
           new MutationObserver(records => { for (const r of records) { \
             for (const n of r.addedNodes) if (isMenu(n)) window.__menuToggles.added++; \
             for (const n of r.removedNodes) if (isMenu(n)) window.__menuToggles.removed++; \
           } }).observe(document.body, { childList: true, subtree: true }); })()",
        )
        .await
        .unwrap();

      button.click().await.unwrap();
      tokio::time::sleep(POPOVER_SETTLE).await;
      assert_eq!(menu_toggles(&page).await, (1, 0), "the menu must open once and stay open");
      wait_for_element(&page, ".popover.send-options").await;

      button.click().await.unwrap();
      tokio::time::sleep(POPOVER_SETTLE).await;
      assert_eq!(menu_toggles(&page).await, (1, 1), "a press on the button must close the menu once");
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

/// A new device that asks to be linked shows up on the open page within seconds, with no reload
/// (e2ee.md §5, approve prompt); once denied there it goes, and the next request shows up too.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_link_request_shows_on_the_open_page_and_goes_once_denied() {
  let Some(rig) = Rig::start().await else { return };
  let api = HttpServerApi::new(rig.stack.base_url.clone(), rig.alice.tokens.clone());
  rig
    .browser
    .screenshot_on_failure("web-link-request", |page| async move {
      sign_in_as_alice(&page).await;
      wait_for_text(&page, "browser-channel").await;
      // Nothing asks yet: the page is up before the request, so only a live update can show it.
      tokio::time::sleep(Duration::from_secs(2)).await;

      for name in ["First tablet", "Second tablet"] {
        let offer = LinkOffer::generate();
        let announcement = LinkAnnouncement {
          device_name: name.to_owned(),
          platform: DeviceLinkPlatform::Android,
        };
        let new_device = async {
          // The request is only taken once the new device waits in the relay, as in the app.
          let announce = async {
            let deadline = Instant::now() + WAIT;
            while let Err(error) = offer.announce(&api, &announcement).await {
              assert!(Instant::now() < deadline, "the request was never taken: {error}");
              tokio::time::sleep(Duration::from_millis(200)).await;
            }
          };
          tokio::join!(NewDeviceLink::connect(&api, &offer), announce).0
        };
        let (link, ()) = tokio::join!(new_device, async {
          wait_for_text(&page, name).await;
          click_text(&page, "Deny").await;
          wait_for_text_gone(&page, name).await;
        });
        assert!(
          matches!(link, Err(CoreError::LinkDenied)),
          "{name}: {:?}",
          link.err()
        );
      }
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
      let composer = wait_for_element(&page, "[data-composer]").await;
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

/// #744 (e2ee.md §9.4): one tab at a time runs the app, with the one core and store of the account.
/// A second tab asks before it takes over, the first one steps down and can take it back. Messages
/// sent from either tab reach Carol exactly once, in order, and decrypt.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_tab_takes_over_and_the_first_takes_it_back() {
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
  send_in_channel(&first, "tab one: 1").await;
  rig
    .deliver_until(|texts| texts.iter().any(|text| text == "tab one: 1"))
    .await;

  // The second tab asks first; taking over makes the first one inactive.
  let second = rig
    .browser
    .new_page(&format!("{WEB_ORIGIN}/?backend=server"))
    .await;
  wait_for_text(&second, "TeamCraft is already open in another tab").await;
  click_text(&second, "Use here").await;
  wait_for_text(&first, "TeamCraft was opened in another tab").await;
  sign_in_again(&second).await;
  wait_for_text(&second, "browser-channel").await;
  send_in_channel(&second, "tab two: 1").await;
  rig
    .deliver_until(|texts| texts.iter().any(|text| text == "tab two: 1"))
    .await;

  // And back to the first tab.
  first.bring_to_front().await.unwrap();
  click_text(&first, "Use here").await;
  wait_for_text(&second, "TeamCraft was opened in another tab").await;
  send_in_channel(&first, "tab one: 2").await;
  rig
    .deliver_until(|texts| texts.iter().any(|text| text == "tab one: 2"))
    .await;

  let texts = rig.carol.texts(channel);
  let seen: Vec<&str> = texts
    .iter()
    .map(String::as_str)
    .filter(|text| text.starts_with("tab "))
    .collect();
  assert_eq!(seen, ["tab one: 1", "tab two: 1", "tab one: 2"]);
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
  pub(crate) stack: Stack,
  pub(crate) browser: BrowserSession,
  pub(crate) alice: Peer,
  pub(crate) carol: Peer,
  pub(crate) channel: ConversationId,
  _turn: WebPortLease,
}

impl Rig {
  pub(crate) async fn start() -> Option<Self> {
    Self::start_with(|builder| builder, &[]).await
  }

  /// With more server configuration (`configure`) and extra browser switches.
  pub(crate) async fn start_with(
    configure: impl FnOnce(StackBuilder) -> StackBuilder,
    browser_args: &[&str],
  ) -> Option<Self> {
    let Some(web_dir) = web_dir() else {
      let is_required = std::env::var("E2E_REQUIRED").is_ok_and(|value| value == "1");
      assert!(!is_required, "no web build: run tests/e2e/web/build.sh");
      eprintln!("skipping: no web build (tests/e2e/web/build.sh)");
      return None;
    };
    let builder = configure(
      Stack::builder()
        .with_port(8082)
        .with_web_dir(web_dir)
        .with_env("TEAMCRAFT_WEB__CONTENT_SECURITY_POLICY", TEST_CSP),
    );
    // One test at a time owns the port.
    let turn = lease_web_port(8082).await;
    let stack = builder.start().await?;
    let browser = BrowserSession::launch_with(browser_args).await?;

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

/// Pumps Alice's other device (and Carol's) until it added the browser to the open conversation's
/// group, so the page no longer says the device isn't set up for it.
pub(crate) async fn wait_until_set_up(alice: &mut Peer, carol: &mut Peer, page: &Page) {
  let deadline = Instant::now() + Duration::from_secs(120);
  let mut round = 0_u32;
  // The notice shows once the conversation has loaded (its messages are there), so only its
  // absence for a while after that counts.
  let mut maybe_clear_since: Option<Instant> = None;
  loop {
    let text = body_text(page).await;
    if text.contains("hello from carol") && !text.contains("isn't fully set up") {
      let since = *maybe_clear_since.get_or_insert_with(Instant::now);
      if since.elapsed() >= Duration::from_secs(3) {
        return;
      }
    } else {
      maybe_clear_since = None;
    }
    assert!(
      Instant::now() < deadline,
      "the browser was never added to the conversation"
    );
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
}

/// Signs in through Keycloak's login page and unlocks the account's keys on this new browser.
pub(crate) async fn sign_in_as_alice(page: &Page) {
  teamcraft_e2e::browser::prefer_mouse(page).await;
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
  // them with the recovery passphrase (the field also takes a recovery kit's key).
  type_until_kept(page, "#enroll-secret", PASSPHRASE).await;
  click_text(page, "Unlock").await;
}

/// Types into a field and checks that the text stayed. The app re-renders the screen while the core
/// settles, which replaces the field and loses what was typed into the old one (issue #444); a
/// person would type again, so the test does too.
pub(crate) async fn type_until_kept(page: &Page, selector: &str, text: &str) {
  let deadline = Instant::now() + WAIT;
  let probe = format!("(document.querySelector('{selector}') || {{}}).value");
  loop {
    if let Ok(field) = page.find_element(selector).await
      && field.click().await.is_ok()
      && field.type_str(text).await.is_ok()
    {
      // The page needs a moment to apply the input event before the value can be read back.
      tokio::time::sleep(Duration::from_millis(300)).await;
      let kept = page
        .evaluate(probe.as_str())
        .await
        .ok()
        .and_then(|value| value.into_value::<String>().ok());
      if kept.as_deref() == Some(text) {
        return;
      }
    }
    assert!(
      Instant::now() < deadline,
      "{selector} did not keep the typed text"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
  }
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
  let composer = wait_for_element(page, "[data-composer]").await;
  // Banners come and go above the conversation while the app settles (the release check, storage
  // warnings) and move the composer: a click at where it was lands elsewhere. Focus it directly,
  // and type again if the text did not reach it.
  let deadline = Instant::now() + WAIT;
  loop {
    focus_composer(page).await;
    composer.type_str(text).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    if composer_text(page).await.contains(text) {
      break;
    }
    assert!(Instant::now() < deadline, "the composer never showed {text:?}");
  }
  composer.press_key("Enter").await.unwrap();
  wait_for_text(page, text).await;
}

/// Puts the keyboard focus into the channel's composer.
async fn focus_composer(page: &Page) {
  let deadline = Instant::now() + WAIT;
  loop {
    let is_focused = page
      .evaluate(
        "(() => { const el = document.querySelector('[data-composer]'); if (!el) return false; el.focus(); return document.activeElement === el; })()",
      )
      .await
      .ok()
      .and_then(|result| result.into_value::<bool>().ok())
      .unwrap_or(false);
    if is_focused {
      return;
    }
    assert!(Instant::now() < deadline, "the composer never took the focus");
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
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

/// How often the send options were added to and removed from the page (`__menuToggles`).
async fn menu_toggles(page: &Page) -> (u64, u64) {
  let counts: serde_json::Value = page
    .evaluate("window.__menuToggles")
    .await
    .unwrap()
    .into_value()
    .unwrap();
  (
    counts["added"].as_u64().unwrap_or(0),
    counts["removed"].as_u64().unwrap_or(0),
  )
}

async fn composer_text(page: &Page) -> String {
  page
    .evaluate("document.querySelector('[data-composer]').textContent")
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

/// Clicks the first button, link or list entry whose text contains `text`, once there is one: menus
/// and dialogs of the split bundle show after their chunk loaded.
pub(crate) async fn click_text(page: &Page, text: &str) {
  let script = format!(
    "(() => {{ const el = [...document.querySelectorAll('button, a, li, [role=button], [role=link], [role=treeitem], [role=option], [role=menuitem]')].find(e => e.innerText.includes({text:?})); if (!el) return false; el.click(); return true; }})()"
  );
  let deadline = Instant::now() + WAIT;
  loop {
    let clicked = page
      .evaluate(script.as_str())
      .await
      .ok()
      .and_then(|result| result.into_value::<bool>().ok())
      .unwrap_or(false);
    if clicked {
      return;
    }
    assert!(
      Instant::now() < deadline,
      "no button or link with the text {text:?} within {WAIT:?}"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
}
