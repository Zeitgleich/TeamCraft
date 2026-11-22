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
use teamcraft_e2e::call::{SfuIdentity, hold_call, join_call};
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
  // The app pins the SFU's certificate by the hash the server hands out with the join token.
  let sfu = SfuIdentity::generate();
  let Some(stack) = Stack::builder()
    .with_calls()
    .with_sfu_identity(sfu.certificate_pem, sfu.key_pem, sfu.sha256_hex)
    .start()
    .await
  else {
    return;
  };
  let certificate_hash = stack.maybe_sfu.as_ref().unwrap().certificate_hash;
  let mut alice = Peer::enroll(&stack, "alice").await;
  // Carol's keys exist before the app signs in (it unlocks them); her first device stays offline.
  let carol = Peer::enroll(&stack, "carol").await.user_id();
  let dm = alice.client.open_dm(carol).await.unwrap();

  let mut app = session.launch(&stack.base_url).await.unwrap();
  sign_in_and_unlock(&session, &mut app).await;

  // Carol starts the DM's call from the app, as its call screen's Join does, once Alice's client
  // took the app's new device into the conversation's encryption; then Alice comes in.
  let join = json!({ "action": { "action": "join", "request": { "conversation": dm, "kind": "direct" } } });
  let deadline = Instant::now() + WAIT;
  loop {
    alice.pump().await;
    match app.invoke("call_action", join.clone()).await {
      Ok(_) => break,
      Err(error) if error.to_string().contains("call_encryption_pending") && Instant::now() < deadline => {
        tokio::time::sleep(Duration::from_millis(500)).await;
      },
      Err(error) => panic!("joining from the app: {error:#}"),
    }
  }
  // The page takes over a call the shell runs when it loads (#824), as after a reload.
  app.evaluate("location.reload()").await.unwrap();
  app.detach_inspector();
  let alice_call = join_call(&mut alice, dm, certificate_hash, 330.0).await;
  let alice_holds = tokio::spawn(hold_call(alice_call, ALICE_STAYS));
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
  minimize(&mut app).await;
  let title = wait_for_small_window(&app, "document.querySelector('.call-mini-title').textContent").await;
  assert!(title.as_str().is_some_and(|title| !title.is_empty()), "{title:?}");

  eprintln!("call mini: muting from the small window");
  in_small_window(
    &app,
    "document.querySelector('.call-mini-controls .round-btn').click()",
  )
  .await;
  // The window shows the mute only once the main window's call took it (its state comes back).
  let pressed = wait_for_small_window(
    &app,
    "document.querySelector('.call-mini-controls .round-btn').classList.contains('off') || null",
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
  minimize(&mut app).await;
  wait_for_small_window(&app, "true").await;
  in_small_window(&app, "document.querySelector('.call-mini-hangup').click()").await;
  // The call ended in the main window: it hands over "no call", and the window goes.
  wait_for_window_count(&app, 1).await;
  // The server's roster has Carol no more.
  let deadline = Instant::now() + WAIT;
  loop {
    let roster = stack
      .get(&alice.tokens, &format!("/conversations/{dm}/call"))
      .await;
    if !roster.body.to_string().contains(&carol.to_string()) {
      break;
    }
    assert!(
      Instant::now() < deadline,
      "Carol is still in the call: {}",
      roster.body
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
  }

  alice_holds.abort();
  app.quit().await.unwrap();
  stack.stop().await;
}

/// Minimizes the main window as its title bar's button does. A minimized page may not run its
/// scripts any more, so the answer isn't waited for.
async fn minimize(app: &mut DesktopApp) {
  app
    .evaluate("window.__TAURI_INTERNALS__.invoke('window_action', { action: 'minimize' }), true")
    .await
    .unwrap();
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
    if Instant::now() > deadline {
      let windows = app.window_count().await;
      let pages = app
        .evaluate_in_every_window(
          "[location.href, document.documentElement.className, !!document.querySelector('.call-mini'), \
           (document.querySelector('.panic-details') || {}).value || '', \
           [...document.querySelectorAll('script,link')].map(e => e.src || e.href).filter(Boolean).join(',')].join(' | ')",
        )
        .await;
      panic!("no small call window ({windows} windows: {pages:#?})");
    }
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
