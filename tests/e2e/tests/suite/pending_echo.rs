//! The web client shows the user's own changes at once (issue #778): a sent message is in the list
//! the moment Enter is pressed, a reaction does not wait for a connection that is still being
//! made, and what was written while the network was gone is still there after a restart and goes
//! out once, when the network is back.
//!
//! The first part measures, in the page: the time from the Enter key to the pending row in the
//! DOM (`PENDING_ECHO_RUNS` sends, default 15), and the time from a click on a reaction to its
//! chip while the start is still connecting (the reads of the connection held back for six seconds). `PENDING_ECHO_MEASURE_ONLY=1` stops after the measurement and asserts
//! nothing (to measure a build from before this change):
//!
//! ```sh
//! PENDING_ECHO_RUNS=30 cargo test -p teamcraft-e2e --features browser --test suite -- --nocapture pending_echo::
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chromiumoxide::cdp::browser_protocol::fetch::ContinueRequestParams;
use chromiumoxide::cdp::browser_protocol::network::EnableParams as NetworkEnableParams;
use chromiumoxide::page::Page;
use teamcraft_e2e::browser::{BrowserSession, lease_web_port, prefer_mouse, web_dir};
use teamcraft_e2e::scenario::space_with;
use teamcraft_e2e::{Peer, Stack};
use teamcraft_proto::{ConversationId, UserId};

use crate::cached_start::{CLOCK_RUNS_ON, add_script, block_the_network, intercept, unblock_the_network};
use crate::perf::sign_in_and_unlock;
use crate::web::{TEST_CSP, body_text, wait_for_element, wait_for_text};

const PORT: u16 = 8083;
const WEB_ORIGIN: &str = "http://127.0.0.1:8083";
const CHANNEL: &str = "instant-echo";
const OLDER: &str = "a message that is already here";
/// The newest message once the sends of the measurement are done: the one that is reacted to.
const TARGET: &str = "the message to react to";
const QUEUED: &str = "written while the network was gone";
/// Pending rows may show this late after Enter. The budget is one frame (16 ms) on the user's
/// machine; the headless browser of the test runs on a loaded one, and this only catches a core
/// round trip put back in front of the row.
const ECHO_LIMIT_MS: f64 = 150.0;
/// A reaction while the connection is held: the chip must not wait for it.
const REACTION_LIMIT: Duration = Duration::from_millis(2_000);
/// How long the connection's reads are held back: the connection takes at least this long.
const SYNC_HELD: Duration = Duration::from_secs(6);
const WAIT: Duration = Duration::from_secs(60);

/// What the page notes of a send: when Enter went in and when the row with the text was added.
const ECHO_PROBE: &str = r"(() => {
  if (window.__echo) return;
  window.__echo = { enter: null, shown: null, expect: null };
  document.addEventListener('keydown', event => {
    const inComposer = event.target.closest && event.target.closest('[data-composer]');
    if (event.key === 'Enter' && inComposer && window.__echo.expect) window.__echo.enter = event.timeStamp;
  }, true);
  const inRow = node => {
    const element = node.nodeType === 1 ? node : node.parentElement;
    return element && element.closest && element.closest('[data-msg-id]');
  };
  new MutationObserver(records => {
    const at = performance.now();
    const echo = window.__echo;
    if (echo.enter === null || echo.shown !== null) return;
    for (const record of records) {
      const nodes = [...record.addedNodes];
      if (record.type === 'characterData') nodes.push(record.target);
      for (const node of nodes) {
        if (inRow(node) && (node.textContent || '').includes(echo.expect)) { echo.shown = at; return; }
      }
    }
  }).observe(document.body, { childList: true, subtree: true, characterData: true });
})()";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn own_changes_show_at_once_and_survive_a_lost_network() {
  let Some(web_dir) = web_dir() else {
    let is_required = std::env::var("E2E_REQUIRED").is_ok_and(|value| value == "1");
    assert!(!is_required, "no web build: run tests/e2e/web/build.sh");
    eprintln!("skipping: no web build (tests/e2e/web/build.sh)");
    return;
  };
  let _turn = lease_web_port(PORT).await;
  let Some(stack) = Stack::builder()
    .with_port(PORT)
    .with_web_dir(web_dir)
    .with_env("TEAMCRAFT_WEB__CONTENT_SECURITY_POLICY", TEST_CSP)
    // A page that comes back starts a fresh session with a full sync, as after a night: the
    // connection is the long one.
    .with_env("TEAMCRAFT_GATEWAY__RESUME_WINDOW_SECS", "1")
    .start()
    .await
  else {
    return;
  };
  let Some(browser) = BrowserSession::launch_for_measurements().await else {
    return;
  };
  let mut alice = Peer::enroll(&stack, "alice").await;
  drop(Peer::enroll(&stack, "carol").await);
  let carol_tokens = stack.tokens("carol");
  let carol: UserId = stack.get(&carol_tokens, "/me").await.id().parse().unwrap();
  let space = space_with(&stack, &alice, "Space", &[]).await;
  stack
    .add_space_member(&alice.tokens, &space.to_string(), &carol.to_string(), "member")
    .await;

  let alice_ref = &mut alice;
  browser
    .screenshot_on_failure("pending-echo", |page| async move {
      // A machine with a touch screen would make Enter a newline (issue #444).
      prefer_mouse(&page).await;
      sign_in_and_unlock(&page, "carol").await;
      // After the browser's device exists, so it gets its welcome without anybody's help.
      let channel = alice_ref.client.create_channel(space, CHANNEL).await.unwrap();
      alice_ref.client.add_member(channel, carol).await.unwrap();
      alice_ref.client.send_text(channel, None, OLDER).await.unwrap();
      let url = format!("{WEB_ORIGIN}/c/{channel}");
      page.goto(url.clone()).await.unwrap();
      wait_for_text(&page, OLDER).await;
      // The account is written after the start; give it its moment before the page goes.
      tokio::time::sleep(Duration::from_secs(2)).await;

      let is_measure_only = std::env::var("PENDING_ECHO_MEASURE_ONLY").is_ok();
      measure_echo(&page, is_measure_only).await;
      alice_ref.client.send_text(channel, None, TARGET).await.unwrap();
      wait_for_text(&page, TARGET).await;
      tokio::time::sleep(Duration::from_secs(2)).await;
      measure_reaction_while_connecting(&page, &url, is_measure_only).await;
      if is_measure_only {
        return;
      }
      written_offline_survives_a_restart_and_goes_out_once(&page, alice_ref, channel, &url).await;
    })
    .await;
  browser.close().await;
  stack.stop().await;
}

/// Sends messages from the composer one after the other; prints the time from Enter to the row.
async fn measure_echo(page: &Page, is_measure_only: bool) {
  let runs: usize = std::env::var("PENDING_ECHO_RUNS")
    .ok()
    .and_then(|value| value.parse().ok())
    .unwrap_or(15);
  page.evaluate(ECHO_PROBE).await.unwrap();
  let mut millis = Vec::new();
  for run in 0..runs {
    let text = format!(
      "echo probe {run} {}",
      teamcraft_proto::ClientMessageId::random().to_hex()
    );
    page
      .evaluate(format!(
        "window.__echo.expect = {text:?}; window.__echo.enter = null; window.__echo.shown = null;"
      ))
      .await
      .unwrap();
    let composer = type_into_composer(page, &text).await;
    composer.press_key("Enter").await.unwrap();
    millis.push(wait_for_echo(page).await);
    wait_until_sent(page, &text).await;
  }
  millis.sort_by(f64::total_cmp);
  let median = millis[millis.len() / 2];
  let p95 = millis[(millis.len() * 95 / 100).min(millis.len() - 1)];
  eprintln!(
    "pending echo: Enter to the row in the DOM: median {median:.1} ms, p95 {p95:.1} ms, all {millis:.1?}"
  );
  if !is_measure_only {
    assert!(p95 < ECHO_LIMIT_MS, "the row came {p95:.1} ms after Enter");
  }
}

async fn wait_for_echo(page: &Page) -> f64 {
  let deadline = Instant::now() + Duration::from_secs(10);
  loop {
    let result = page
      .evaluate("(window.__echo.shown !== null && window.__echo.enter !== null) ? window.__echo.shown - window.__echo.enter : -1")
      .await
      .ok()
      .and_then(|result| result.into_value::<f64>().ok())
      .unwrap_or(-1.0);
    if result >= 0.0 {
      return result;
    }
    assert!(Instant::now() < deadline, "the sent message never appeared");
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
}

/// Reloads, holds the reads of the connection back for a while, and reacts to the newest message while
/// the connection waits for it: the reaction must not wait for the connection.
async fn measure_reaction_while_connecting(page: &Page, url: &str, is_measure_only: bool) {
  page.execute(NetworkEnableParams::default()).await.unwrap();
  // Every load turns the page's clock an hour on, so the saved access token has run out and the
  // start goes through the identity provider first (cached_start.rs).
  add_script(page, CLOCK_RUNS_ON).await;
  let is_syncing = Arc::new(AtomicBool::new(false));
  let noted = is_syncing.clone();
  let held = intercept(page, &["*/api/v1/*"], move |page, event| {
    let noted = noted.clone();
    async move {
      // The check of the account and the page's own files go through; what the connection reads
      // (the sync, the counts, the presence) is held.
      let url = event.request.url.clone();
      let is_account_check = url.ends_with("/api/v1/me") || url.contains("/api/v1/server-info");
      // The full sync of a fresh session starts with the conversation list.
      if url.ends_with("/api/v1/conversations") {
        noted.store(true, Ordering::SeqCst);
      }
      if !is_account_check {
        tokio::time::sleep(SYNC_HELD).await;
      }
      // The page may have gone meanwhile.
      let _ = page
        .execute(ContinueRequestParams::new(event.request_id.clone()))
        .await;
    }
  })
  .await;
  page.goto("about:blank").await.unwrap();
  page.goto(url.to_owned()).await.unwrap();
  wait_for_text(page, TARGET).await;
  let deadline = Instant::now() + WAIT;
  while !is_syncing.load(Ordering::SeqCst) {
    assert!(Instant::now() < deadline, "the sync never started");
    tokio::time::sleep(Duration::from_millis(10)).await;
  }
  let clicked = Instant::now();
  react_to(page, TARGET).await;
  let took = wait_for_reaction(page, TARGET, clicked).await;
  eprintln!(
    "pending echo: reaction while connecting (sync held {} s): chip after {} ms",
    SYNC_HELD.as_secs(),
    took.as_millis()
  );
  if !is_measure_only {
    assert!(took < REACTION_LIMIT, "the reaction waited {took:?}");
  }
  held.stop(page).await;
  // Let the held requests finish, so the next part starts from a connected page.
  tokio::time::sleep(SYNC_HELD + Duration::from_secs(5)).await;
}

/// Hovers the row of `text` and clicks the first quick reaction of its toolbar.
async fn react_to(page: &Page, text: &str) {
  let script = format!(
    "(() => {{ const row = [...document.querySelectorAll('[data-msg-id]')].find(r => r.innerText.includes({text:?})); if (!row) return false; row.dispatchEvent(new PointerEvent('pointerenter', {{ pointerType: 'mouse' }})); return true; }})()"
  );
  let deadline = Instant::now() + WAIT;
  loop {
    let hovered = page
      .evaluate(script.as_str())
      .await
      .ok()
      .and_then(|result| result.into_value::<bool>().ok())
      .unwrap_or(false);
    let is_clicked = hovered
      && page
        .evaluate("(() => { const button = document.querySelector('.quick-reaction'); if (!button) return false; button.click(); return true; })()")
        .await
        .ok()
        .and_then(|result| result.into_value::<bool>().ok())
        .unwrap_or(false);
    if is_clicked {
      return;
    }
    assert!(Instant::now() < deadline, "no reaction toolbar for {text:?}");
    tokio::time::sleep(Duration::from_millis(50)).await;
  }
}

async fn wait_for_reaction(page: &Page, text: &str, since: Instant) -> Duration {
  let script = format!(
    "(() => {{ const row = [...document.querySelectorAll('[data-msg-id]')].find(r => r.innerText.includes({text:?})); return !!row && !!row.querySelector('.reaction'); }})()"
  );
  let deadline = Instant::now() + Duration::from_secs(30);
  loop {
    let has_chip = page
      .evaluate(script.as_str())
      .await
      .ok()
      .and_then(|result| result.into_value::<bool>().ok())
      .unwrap_or(false);
    if has_chip {
      return since.elapsed();
    }
    assert!(Instant::now() < deadline, "the reaction never showed");
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
}

/// No network: a message shows as pending, stays through a restart of the page, and goes out
/// exactly once when the network is back. The start of the second page is from the cached account.
async fn written_offline_survives_a_restart_and_goes_out_once(
  page: &Page,
  alice: &mut Peer,
  channel: ConversationId,
  url: &str,
) {
  page.goto("about:blank").await.unwrap();
  page.goto(url.to_owned()).await.unwrap();
  wait_for_text(page, TARGET).await;
  block_the_network(page).await;

  let composer = type_into_composer(page, QUEUED).await;
  composer.press_key("Enter").await.unwrap();
  wait_for_row(page, QUEUED, RowState::Pending).await;

  // The page goes away and comes back, still without a network: the message is where it was.
  page.goto("about:blank").await.unwrap();
  page.goto(url.to_owned()).await.unwrap();
  wait_for_text(page, QUEUED).await;
  wait_for_row(page, QUEUED, RowState::Pending).await;
  assert_eq!(
    body_text(page).await.matches(QUEUED).count(),
    1,
    "shown once, not twice"
  );

  unblock_the_network(page).await;
  alice.wait_for_text(channel, QUEUED).await;
  wait_for_row(page, QUEUED, RowState::Sent).await;
  // A retry must not double it: give the outbox its ticks.
  tokio::time::sleep(Duration::from_secs(8)).await;
  alice.pump().await;
  let received = alice.texts(channel).iter().filter(|text| *text == QUEUED).count();
  assert_eq!(received, 1, "sent once");
  assert_eq!(body_text(page).await.matches(QUEUED).count(), 1);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowState {
  Pending,
  Sent,
}

async fn wait_for_row(page: &Page, text: &str, wanted: RowState) {
  let script = format!(
    "(() => {{ const row = [...document.querySelectorAll('[data-msg-id]')].find(r => r.innerText.includes({text:?})); if (!row) return 'missing'; return row.querySelector('.msg-delivery.is-pending') ? 'pending' : 'sent'; }})()"
  );
  let want = match wanted {
    RowState::Pending => "pending",
    RowState::Sent => "sent",
  };
  let deadline = Instant::now() + WAIT;
  let mut state = String::new();
  while Instant::now() < deadline {
    state = page
      .evaluate(script.as_str())
      .await
      .ok()
      .and_then(|result| result.into_value::<String>().ok())
      .unwrap_or_default();
    if state == want {
      return;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
  panic!("the row of {text:?} is {state:?}, not {want:?}");
}

async fn wait_until_sent(page: &Page, text: &str) {
  wait_for_row(page, text, RowState::Sent).await;
}

/// Types `text` into the composer and waits until the app has it (it re-renders while it settles and
/// may lose what was typed into the old field): Enter before that adds a line instead of sending.
async fn type_into_composer(page: &Page, text: &str) -> chromiumoxide::element::Element {
  let deadline = Instant::now() + WAIT;
  loop {
    let composer = wait_for_element(page, "[data-composer]").await;
    focus_composer(page).await;
    composer.type_str(text).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let typed = page
      .evaluate("document.querySelector('[data-composer]').textContent")
      .await
      .ok()
      .and_then(|result| result.into_value::<String>().ok())
      .unwrap_or_default();
    if typed.contains(text) {
      return composer;
    }
    assert!(Instant::now() < deadline, "the composer never showed {text:?}");
  }
}

async fn focus_composer(page: &Page) {
  let deadline = Instant::now() + WAIT;
  loop {
    let is_focused = page
      .evaluate("(() => { const el = document.querySelector('[data-composer]'); if (!el) return false; el.focus(); return document.activeElement === el; })()")
      .await
      .ok()
      .and_then(|result| result.into_value::<bool>().ok())
      .unwrap_or(false);
    if is_focused {
      return;
    }
    assert!(Instant::now() < deadline, "the composer never took the focus");
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
}
