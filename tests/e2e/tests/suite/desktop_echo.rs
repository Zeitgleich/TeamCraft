//! Send to paint in the Linux desktop app (issue #2269): the time from Enter in the composer to the
//! row of the message in the page, for a text and for a 12 MP photo with a caption.
//!
//! On the desktop the client core runs in the shell, so the page's mirror answers through the shell;
//! the pending row (`RemoteChat::show_pending`) must not wait for those round trips. The web build
//! measures the same in `pending_echo.rs`; the numbers of both are in docs/performance.md, "Own
//! changes show at once".
//!
//! Needs the desktop app (tests/e2e/README.md, "Desktop"):
//!
//! ```sh
//! DESKTOP_ECHO_RUNS=20 E2E_DESKTOP_BIN=$PWD/target/debug/teamcraft-desktop xvfb-run -a \
//!   cargo test -p teamcraft-e2e --features desktop --test suite -- --nocapture desktop_echo::
//! ```

use std::time::{Duration, Instant};

use serde_json::json;
use teamcraft_e2e::desktop::{DesktopApp, DesktopSession};
use teamcraft_e2e::{Peer, Stack};

use crate::desktop_perf::sign_in_and_unlock;
use crate::pending_echo::{ECHO_PROBE, PICK_PICTURE};

const WAIT: Duration = Duration::from_secs(60);
const OLDER: &str = "a message that is already here";
/// Pending rows may show this late after Enter (median): the budget is one frame, the limit only catches
/// a round trip through the shell put back in front of the row.
const ECHO_LIMIT_MS: f64 = 150.0;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sent_message_shows_in_the_desktop_app_without_waiting_for_the_shell() {
  let Some(session) = DesktopSession::start("echo").await else {
    return;
  };
  let Some(stack) = Stack::builder().start().await else {
    return;
  };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let carol = Peer::enroll(&stack, "carol").await.user_id();
  let dm = alice.client.open_dm(carol).await.unwrap();

  let mut app = session.launch(&stack.base_url).await.unwrap();
  sign_in_and_unlock(&session, &mut app).await;
  // Once Alice's client took the app's new device into the conversation's encryption.
  let deadline = Instant::now() + WAIT;
  loop {
    alice.pump().await;
    if alice.client.send_text(dm, None, OLDER).await.is_ok() {
      break;
    }
    assert!(
      Instant::now() < deadline,
      "Alice could not write to the direct chat"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
  }
  app.wait_for_text(OLDER, WAIT).await.unwrap();
  // The only chat opens by itself; its message list and composer are what is measured.
  wait_for_composer(&mut app).await;
  app.evaluate(ECHO_PROBE).await.unwrap();

  let runs: usize = std::env::var("DESKTOP_ECHO_RUNS")
    .ok()
    .and_then(|value| value.parse().ok())
    .unwrap_or(10);
  let mut texts = Vec::new();
  let mut photos = Vec::new();
  for run in 0..runs {
    texts.push(send_and_measure(&mut app, &format!("desktop echo {run}"), false).await);
    photos.push(send_and_measure(&mut app, &format!("desktop photo {run}"), true).await);
  }
  report("text", &mut texts);
  report("photo", &mut photos);
  // The median is the gate. The p95 of 20 is the slowest send: it is printed, not gated, because
  // a shared CI runner has its own slow moments (#2364 numbers: docs/performance.md).
  for (what, millis) in [("text", &texts), ("photo", &photos)] {
    let median = millis[millis.len() / 2];
    assert!(
      median < ECHO_LIMIT_MS,
      "the {what} row came {median:.1} ms after Enter"
    );
  }
  app.quit().await.unwrap();
}

/// Types `text` (with the photo picked first when `is_photo`), presses Enter, and returns the
/// milliseconds until the row with the text is in the DOM, measured in the page.
async fn send_and_measure(app: &mut DesktopApp, text: &str, is_photo: bool) -> f64 {
  if is_photo {
    let name = format!("{}.jpg", text.replace(' ', "-"));
    app
      .evaluate_async(&format!("({PICK_PICTURE})({name:?})"), WAIT)
      .await
      .unwrap();
    app.wait_for_text(&name, WAIT).await.unwrap();
  }
  app
    .evaluate(&format!(
      "window.__echo.expect = {text:?}; window.__echo.enter = null; window.__echo.shown = null; 1"
    ))
    .await
    .unwrap();
  // The composer re-renders while it settles and may lose what was typed: type until it has it.
  let deadline = Instant::now() + WAIT;
  loop {
    app
      .evaluate(&format!(
        "(() => {{ const c = document.querySelector('[data-composer]'); c.focus(); \
         document.execCommand('insertText', false, {text:?}); return c.textContent; }})()"
      ))
      .await
      .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let typed = app
      .evaluate("document.querySelector('[data-composer]').textContent")
      .await
      .unwrap();
    if typed.as_str().is_some_and(|value| value.contains(text)) {
      break;
    }
    assert!(Instant::now() < deadline, "the composer never showed {text:?}");
  }
  // Photos go with the keypad's Enter, which must send the same way (#2387).
  let code = if is_photo { "NumpadEnter" } else { "Enter" };
  app
    .evaluate(&format!(
      "document.querySelector('[data-composer]').dispatchEvent(new KeyboardEvent('keydown', \
       {{ key: 'Enter', code: '{code}', bubbles: true, cancelable: true }})); 1"
    ))
    .await
    .unwrap();
  let deadline = Instant::now() + Duration::from_secs(10);
  loop {
    let value = app
      .evaluate("(window.__echo.shown !== null && window.__echo.enter !== null) ? window.__echo.shown - window.__echo.enter : -1")
      .await
      .unwrap();
    if let Some(millis) = value.as_f64().filter(|millis| *millis >= 0.0) {
      return millis;
    }
    if Instant::now() >= deadline {
      let state = app
        .evaluate(
          "JSON.stringify({ echo: window.__echo, composer: document.querySelector('[data-composer]')?.textContent, \
           rows: [...document.querySelectorAll('[data-msg-id]')].map(row => row.textContent.slice(0, 60)).slice(-5) })",
        )
        .await;
      panic!("the sent message {text:?} never appeared: {state:?}");
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
}

async fn wait_for_composer(app: &mut DesktopApp) {
  let deadline = Instant::now() + WAIT;
  while app
    .evaluate("!!document.querySelector('[data-composer]')")
    .await
    .unwrap()
    != json!(true)
  {
    assert!(Instant::now() < deadline, "no composer: {:?}", app.text().await);
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
}

fn report(what: &str, millis: &mut [f64]) {
  millis.sort_by(f64::total_cmp);
  let median = millis[millis.len() / 2];
  let p95 = millis[(millis.len() * 95 / 100).min(millis.len() - 1)];
  eprintln!(
    "desktop echo: {what}, Enter to the row in the DOM: median {median:.1} ms, p95 {p95:.1} ms, all {millis:.1?}"
  );
}
