//! The desktop app's downloads button stays in the top bar while the app catches up after a start
//! (issue #2338): Carol saves a file, quits, and comes back to a workspace that filled up
//! while she was away. The button must not disappear for a moment while the backlog is read: the
//! search beside it would jump sideways with every flicker.
//!
//! Needs the desktop app like `desktop_perf.rs` (tests/e2e/README.md, "Desktop").

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use teamcraft_client_api::remote::ChatAction;
use teamcraft_core::client::OutgoingFile;
use teamcraft_core::store::LocalStore;
use teamcraft_e2e::desktop::{DesktopApp, DesktopSession};
use teamcraft_e2e::scenario::space_with;
use teamcraft_e2e::{Peer, Stack};
use teamcraft_proto::UserId;

use crate::desktop_perf::sign_in_and_unlock;
use crate::perf::{Scale, seed};

/// An earlier build of the app for the first run (optional).
const PREVIOUS_BINARY_VARIABLE: &str = "E2E_DESKTOP_PREVIOUS_BIN";
/// How long the start is watched: the catch-up of the small workspace and what follows it.
const WATCH: Duration = Duration::from_secs(120);
/// Samples the button in the page: on every DOM change and every frame-ish tick, so a flicker of a
/// single frame is seen too. `window.__downloadsWatch` keeps the counts.
const WATCH_SCRIPT: &str = "(() => {
  if (window.__downloadsWatch) return true;
  if (!document.querySelector('.top-bar')) return false;
  const watch = window.__downloadsWatch = { last: null, shown: 0, hidden: 0, changes: [], moves: [] };
  const sample = () => {
    const nav = document.querySelector('.top-bar .top-nav');
    const left = nav ? Math.round(nav.getBoundingClientRect().left) : null;
    if (left !== watch.navLeft) {
      const actions = [...document.querySelectorAll('.top-bar .top-actions > *')].map(e => e.className || e.tagName);
      watch.moves.push([Math.round(performance.now()), left, actions.join('|')]);
      if (watch.moves.length > 400) watch.moves.shift();
      watch.navLeft = left;
    }
    const isShown = !!document.querySelector('.top-bar .downloads-btn');
    if (isShown === watch.last) return;
    if (isShown) watch.shown++; else if (watch.last !== null) watch.hidden++;
    watch.last = isShown;
    watch.changes.push([Math.round(performance.now()), isShown]);
    if (watch.changes.length > 400) watch.changes.shift();
  };
  new MutationObserver(sample).observe(document.body, { childList: true, subtree: true });
  setInterval(sample, 8);
  sample();
  return true;
})()";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_downloads_button_stays_while_the_app_catches_up() {
  let scale = Scale::from_env();
  let Some(session) = DesktopSession::start("downloads").await else {
    return;
  };
  let Some(stack) = Stack::builder()
    // Seeding writes far faster than a person; the rate limits would throttle it.
    .with_env("TEAMCRAFT_SECURITY__RATE_LIMITS__IS_ENABLED", "false")
    .with_env("TEAMCRAFT_DATABASE__MAX_CONNECTIONS", "16")
    .start()
    .await
  else {
    return;
  };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut bob = Peer::enroll(&stack, "bob").await;
  // Carol's keys exist before the app signs in (it unlocks them); her first device stays offline.
  drop(Peer::enroll(&stack, "carol").await);

  // `E2E_DESKTOP_PREVIOUS_BIN`: the first run is an earlier version, so the watched start is the
  // first one after an update.
  let mut app = match std::env::var_os(PREVIOUS_BINARY_VARIABLE) {
    Some(previous) => session
      .launch_binary(std::path::Path::new(&previous), &stack.base_url)
      .await
      .unwrap(),
    None => session.launch(&stack.base_url).await.unwrap(),
  };
  sign_in_and_unlock(&session, &mut app).await;
  save_a_file(&stack, &mut alice, &mut app).await;
  app.quit().await.unwrap();

  let started = Instant::now();
  seed(&stack, &mut alice, &mut bob, scale).await;
  eprintln!("seeded the {} workspace in {:?}", scale.name, started.elapsed());

  let mut app = session.launch(&stack.base_url).await.unwrap();
  install_watch(&mut app).await;
  tokio::time::sleep(WATCH).await;
  let watch = app.evaluate("window.__downloadsWatch").await.unwrap();
  eprintln!("downloads button while catching up: {watch}");
  assert_eq!(
    watch["last"],
    json!(true),
    "the button is gone after the catch-up: {watch}"
  );
  assert_eq!(
    watch["hidden"],
    json!(0),
    "the button disappeared while the app caught up: {watch}"
  );
  app.quit().await.unwrap();
}

/// Alice sends Carol a file in a channel of their own, and Carol's app saves it to her downloads:
/// the downloads button shows.
async fn save_a_file(stack: &Stack, alice: &mut Peer, app: &mut DesktopApp) {
  let carol_tokens = stack.tokens("carol");
  let carol: UserId = stack.get(&carol_tokens, "/me").await.id().parse().unwrap();
  let space = space_with(stack, alice, "Files", &[]).await;
  stack
    .add_space_member(&alice.tokens, &space.to_string(), &carol.to_string(), "member")
    .await;
  let channel = alice.client.create_channel(space, "handouts").await.unwrap();
  alice.client.add_member(channel, carol).await.unwrap();
  let sent = alice
    .client
    .send_file(
      channel,
      None,
      OutgoingFile {
        file_name: "agenda.txt".into(),
        media_type: "text/plain".into(),
        content: b"agenda: budget, hiring, launch".to_vec(),
      },
    )
    .await
    .unwrap();
  let message = alice
    .store
    .message(channel, sent)
    .unwrap()
    .unwrap()
    .maybe_server_id
    .unwrap();
  app.wait_for_text("Files", Duration::from_secs(60)).await.unwrap();
  let action = serde_json::to_value(ChatAction::DownloadFile {
    conversation: channel,
    message,
  })
  .unwrap();
  // The core may not have the message yet: asked again until the button shows.
  let deadline = Instant::now() + Duration::from_secs(60);
  loop {
    drop(app.invoke("core_action", json!({ "action": action })).await);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let is_shown = app
      .evaluate("!!document.querySelector('.top-bar .downloads-btn')")
      .await
      .unwrap();
    if is_shown == json!(true) {
      return;
    }
    assert!(
      Instant::now() < deadline,
      "the saved file shows no downloads button: {:?}\nlog:\n{}",
      app.text().await,
      app.log()
    );
  }
}

/// Starts watching as soon as the page has its top bar.
async fn install_watch(app: &mut DesktopApp) {
  let deadline = Instant::now() + Duration::from_secs(120);
  loop {
    if let Ok(Value::Bool(true)) = app.evaluate(WATCH_SCRIPT).await {
      return;
    }
    assert!(Instant::now() < deadline, "no top bar: {:?}", app.text().await);
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
}
