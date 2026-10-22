//! The web client in headless Chromium: sign in through the real Keycloak login page and see the
//! conversations of the account. Failures leave screenshots in `target/e2e-artifacts/`.
//!
//! The web build is served by the server under test at `http://127.0.0.1:8082`, the origin the dev
//! realm registers as redirect URI; these tests therefore need that port and run one at a time.

use std::time::{Duration, Instant};

use chromiumoxide::page::Page;
use teamcraft_e2e::browser::{BrowserSession, web_dir};
use teamcraft_e2e::peer::PASSPHRASE;
use teamcraft_e2e::scenario::{channel_with, space_with};
use teamcraft_e2e::{Peer, Stack};
use teamcraft_proto::ConversationId;

const WEB_ORIGIN: &str = "http://127.0.0.1:8082";
static PORT_8082: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
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

/// The stack with the web build, the browser and the data the browser will show: Alice owns a
/// channel she shares with Carol.
struct Rig {
  stack: Stack,
  browser: BrowserSession,
  alice: Peer,
  carol: Peer,
  channel: ConversationId,
  _turn: tokio::sync::MutexGuard<'static, ()>,
}

impl Rig {
  async fn start() -> Option<Self> {
    let Some(web_dir) = web_dir() else {
      let is_required = std::env::var("E2E_REQUIRED").is_ok_and(|value| value == "1");
      assert!(!is_required, "no web build: run tests/e2e/web/build.sh");
      eprintln!("skipping: no web build (tests/e2e/web/build.sh)");
      return None;
    };
    // The default policy only lets pages talk to https: and wss: origins; the dev Keycloak is plain http.
    let policy = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; \
    img-src 'self' data: blob:; media-src 'self' blob:; font-src 'self'; \
    connect-src 'self' http://127.0.0.1:8080 ws: wss: https:; worker-src 'self' blob:; \
    frame-ancestors 'none'; base-uri 'none'; form-action 'self'";
    let builder = Stack::builder()
      .with_port(8082)
      .with_web_dir(web_dir)
      .with_env("TEAMCRAFT_WEB__CONTENT_SECURITY_POLICY", policy);
    // One test at a time owns the port.
    let turn = PORT_8082.lock().await;
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

  async fn finish(self) {
    self.browser.close().await;
    self.stack.stop().await;
  }
}

/// Signs in through Keycloak's login page and unlocks the account's keys on this new browser.
async fn sign_in_as_alice(page: &Page) {
  // The UI follows the browser language; pin English before the app loads (same origin).
  page.goto(format!("{WEB_ORIGIN}/healthz")).await.unwrap();
  page
    .evaluate("localStorage.setItem('teamcraft.locale', 'en')")
    .await
    .unwrap();
  page.goto(format!("{WEB_ORIGIN}/?backend=server")).await.unwrap();
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

async fn wait_for_element(page: &Page, selector: &str) -> chromiumoxide::element::Element {
  let deadline = Instant::now() + WAIT;
  loop {
    if let Ok(element) = page.find_element(selector).await {
      return element;
    }
    assert!(Instant::now() < deadline, "no element {selector} within {WAIT:?}");
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
}

async fn body_text(page: &Page) -> String {
  page
    .evaluate("document.body ? document.body.innerText : ''")
    .await
    .ok()
    .and_then(|result| result.into_value::<String>().ok())
    .unwrap_or_default()
}

async fn wait_for_text(page: &Page, needle: &str) {
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
async fn click_text(page: &Page, text: &str) {
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
