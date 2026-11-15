//! The desktop app's small call window (#2077): Carol is in a call with Alice in the Linux app. While
//! the main window is in front there is no small window; minimized, the small window comes up with
//! the call, its mute button mutes the call that runs with the main window, "back to the call"
//! brings the main window back and closes it, and hang up from it ends the call.
//!
//! Needs the desktop app (tests/e2e/README.md, "Desktop") and a window manager that moves the focus
//! (a headless KWin does: `kwin_wayland --virtual --xwayland`), as the window follows the main
//! window's focus.

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use teamcraft_e2e::call::{hold_call, join_call};
use teamcraft_e2e::desktop::{DesktopApp, DesktopSession};
use teamcraft_e2e::{Peer, Stack};

use crate::desktop_perf::sign_in_and_unlock;

const WAIT: Duration = Duration::from_secs(60);
/// Longer than the test can take: Alice stays in the call until it is torn down.
const ALICE_STAYS: Duration = Duration::from_secs(600);
const SMALL_WINDOW: &str = ".call-mini";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_small_call_window_follows_the_main_window_and_controls_the_call() {
  let Some(session) = DesktopSession::start("call-mini").await else {
    return;
  };
  let Some(stack) = Stack::builder().with_calls().start().await else {
    return;
  };
  let certificate_hash = stack.maybe_sfu.as_ref().unwrap().certificate_hash;
  let mut alice = Peer::enroll(&stack, "alice").await;
  // Carol's keys exist before the app signs in (it unlocks them); her first device stays offline.
  let carol = Peer::enroll(&stack, "carol").await.user_id();
  let dm = alice.client.open_dm(carol).await.unwrap();

  let mut app = session.launch(&stack.base_url).await.unwrap();
  sign_in_and_unlock(&session, &mut app).await;

  // Alice is in the DM's call; Carol joins it from the app, as its call screen's Join does.
  let alice_call = join_call(&mut alice, dm, certificate_hash, 330.0).await;
  let alice_holds = tokio::spawn(hold_call(alice_call, ALICE_STAYS));
  app
    .invoke(
      "call_action",
      json!({ "action": { "action": "join", "request": { "conversation": dm, "kind": "direct" } } }),
    )
    .await
    .unwrap();
  wait_for(
    &mut app,
    "the call screen",
    "document.querySelector('.call-screen') !== null",
  )
  .await;
  assert_eq!(
    app.window_count().await,
    1,
    "the main window is in front: no small window"
  );

  eprintln!("call mini: minimizing the main window");
  app
    .invoke("window_action", json!({ "action": "minimize" }))
    .await
    .unwrap();
  let title = wait_for_small_window(&app, "document.querySelector('.call-mini-title').textContent").await;
  assert!(title.as_str().is_some_and(|title| !title.is_empty()), "{title:?}");

  eprintln!("call mini: muting from the small window");
  in_small_window(
    &app,
    "document.querySelector('.call-mini-controls .round-btn').click()",
  )
  .await;
  wait_until(&mut app, "the call to be muted", |snapshot| {
    snapshot["maybe_active"]["is_muted"] == json!(true)
  })
  .await;
  let pressed = wait_for_small_window(
    &app,
    "document.querySelector('.call-mini-controls .round-btn').getAttribute('aria-pressed') === 'true' || null",
  )
  .await;
  assert_eq!(pressed, json!(true), "the small window shows the mute");

  eprintln!("call mini: back to the call");
  in_small_window(&app, "document.querySelector('.call-mini-back').click()").await;
  wait_for_window_count(&app, 1).await;
  wait_for(
    &mut app,
    "the main window",
    "document.visibilityState === 'visible'",
  )
  .await;

  eprintln!("call mini: hanging up from the small window");
  app
    .invoke("window_action", json!({ "action": "minimize" }))
    .await
    .unwrap();
  wait_for_small_window(&app, "true").await;
  in_small_window(&app, "document.querySelector('.call-mini-hangup').click()").await;
  wait_until(&mut app, "the call to end", |snapshot| {
    snapshot["maybe_active"].is_null()
  })
  .await;
  wait_for_window_count(&app, 1).await;

  alice_holds.abort();
  app.quit().await.unwrap();
  stack.stop().await;
}

/// Evaluates `expression` in the small window once it is there.
async fn wait_for_small_window(app: &DesktopApp, expression: &str) -> Value {
  let deadline = Instant::now() + WAIT;
  loop {
    if let Ok(Some(value)) = app.evaluate_in_window_with(SMALL_WINDOW, expression).await
      && !value.is_null()
    {
      return value;
    }
    assert!(
      Instant::now() < deadline,
      "no small call window; the app's log: {}",
      app.log()
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
}

async fn in_small_window(app: &DesktopApp, expression: &str) {
  wait_for_small_window(app, &format!("({expression}, true)")).await;
}

async fn wait_for_window_count(app: &DesktopApp, count: usize) {
  let deadline = Instant::now() + WAIT;
  while app.window_count().await != count {
    assert!(
      Instant::now() < deadline,
      "the app shows {} windows",
      app.window_count().await
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
}

async fn wait_for(app: &mut DesktopApp, what: &str, expression: &str) {
  let deadline = Instant::now() + WAIT;
  while app.evaluate(expression).await.unwrap_or_default() != json!(true) {
    assert!(
      Instant::now() < deadline,
      "no {what}; the page shows {:?}",
      app.text().await
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
}

/// Waits until the shell's call state (`call_snapshot`) satisfies `is_done`.
async fn wait_until(app: &mut DesktopApp, what: &str, is_done: impl Fn(&Value) -> bool) {
  let deadline = Instant::now() + WAIT;
  loop {
    let snapshot = app.invoke("call_snapshot", json!({})).await.unwrap_or_default();
    if is_done(&snapshot) {
      return;
    }
    assert!(Instant::now() < deadline, "waited for {what}: {snapshot}");
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
}
