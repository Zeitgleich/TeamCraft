//! A new browser of an account with a long direct-message history (issue #1573): the DM shows its
//! messages soon, and never says "No messages yet" while its history is on its way.
//!
//! Carol has a device (a headless client) with a DM to Alice that holds `E2E_DM_HISTORY` messages
//! (600 by default), and 12 channels with history. She signs in on a fresh browser profile, a new device, and opens the DM at
//! once. Both other devices are away for the first seconds, then come online, as a phone would, to
//! bring the new one into the conversation. Measured from opening the DM: when its first message shows (it opens at the first
//! unread one), and whether the empty state showed meanwhile.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;
use chromiumoxide::page::Page;
use teamcraft_e2e::browser::{BrowserSession, lease_web_port, web_dir};
use teamcraft_e2e::scenario::space_with;
use teamcraft_e2e::{Peer, Stack};

use crate::perf::sign_in_and_unlock;

const PORT: u16 = 8083;
const WEB_ORIGIN: &str = "http://127.0.0.1:8083";
const NEWEST: &str = "the newest message of the DM";
/// Channels with history beside the DM, and their messages each.
const OTHER_CHANNELS: usize = 12;
const OTHER_CHANNEL_MESSAGES: usize = 150;
/// How long the other devices stay away after the browser signed in: until one comes back, the new
/// device has no key for the DM.
const OTHERS_AWAY_FOR: Duration = Duration::from_secs(20);
/// Generous: the budget is seconds (robustness.md), the bug was minutes.
const SHOWN_WITHIN: Duration = Duration::from_secs(120);
/// Per frame: when a message of the DM first showed (it opens at the first unread one), and how many frames the empty state showed
/// before it (`performance.now()` milliseconds, from the page's navigation).
const FEED_RECORDER: &str = "window.__feed = { firstMs: null, emptyFrames: 0, firstEmptyMs: null };
  const watch = () => {
    const list = document.querySelector('.message-list');
    if (list && list.querySelector('.empty-title')) {
      window.__feed.emptyFrames += 1;
      if (window.__feed.firstEmptyMs === null) window.__feed.firstEmptyMs = performance.now();
    }
    if (list && list.innerText.includes(' of the DM')) {
      window.__feed.firstMs = performance.now();
      return;
    }
    requestAnimationFrame(watch);
  };
  requestAnimationFrame(watch);";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_browser_shows_a_long_dm_without_calling_it_empty() {
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
    // Seeding writes far faster than a person; the rate limits would throttle it.
    .with_env("TEAMCRAFT_SECURITY__RATE_LIMITS__IS_ENABLED", "false")
    .start()
    .await
  else {
    return;
  };
  let Some(browser) = BrowserSession::launch().await else {
    return;
  };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut carol = Peer::enroll(&stack, "carol").await;
  // Other conversations with history, so the DM is one of many the new device catches up.
  let space = space_with(&stack, &alice, "Space", &[]).await;
  stack
    .add_space_member(
      &alice.tokens,
      &space.to_string(),
      &carol.user_id().to_string(),
      "member",
    )
    .await;
  for channel_index in 0..OTHER_CHANNELS {
    let channel = alice
      .client
      .create_channel(space, &format!("channel {channel_index:02}"))
      .await
      .unwrap();
    alice.client.add_member(channel, carol.user_id()).await.unwrap();
    for index in 0..OTHER_CHANNEL_MESSAGES {
      let body = format!("a message in channel {channel_index} ({index})");
      alice.client.send_text(channel, None, &body).await.unwrap();
    }
  }
  let dm = alice.client.open_dm(carol.user_id()).await.unwrap();
  carol.wait_joined(dm).await;
  let history: usize = std::env::var("E2E_DM_HISTORY")
    .ok()
    .and_then(|value| value.parse().ok())
    .unwrap_or(600);
  for index in 0..history {
    let body = format!("an older message of the DM ({index})");
    alice.client.send_text(dm, None, &body).await.unwrap();
  }
  alice.client.send_text(dm, None, NEWEST).await.unwrap();
  carol.wait_for_text(dm, NEWEST).await;

  let is_done = Arc::new(AtomicBool::new(false));
  let done = is_done.clone();
  let (alice_ref, carol_ref) = (&mut alice, &mut carol);
  browser
    .screenshot_on_failure("new-device-dm", |page| async move {
      let online = async {
        // Carol's other device and Alice are away at first: nobody can bring the new device in.
        tokio::time::sleep(OTHERS_AWAY_FOR).await;
        while !done.load(Ordering::Relaxed) {
          for peer in [&mut *alice_ref, &mut *carol_ref] {
            peer.pump_reconnecting().await;
            peer.heartbeat().await;
          }
          tokio::time::sleep(Duration::from_millis(100)).await;
        }
      };
      let measured = async {
        let result = open_the_dm_on_a_new_browser(&page, dm.to_string()).await;
        done.store(true, Ordering::Relaxed);
        result
      };
      let ((), (first_ms, empty_frames, first_empty_ms)) = tokio::join!(online, measured);
      eprintln!(
        "new device, DM with {history} messages: first message shown after {first_ms:.0} ms, \
         empty state in {empty_frames} frames (first at {first_empty_ms:?} ms)"
      );
      assert_eq!(
        empty_frames, 0,
        "the DM said it is empty while its history was on its way (#1573)"
      );
    })
    .await;
  browser.close().await;
  stack.stop().await;
}

/// Signs Carol in on the fresh browser, opens the DM and waits until a message of it shows:
/// milliseconds from opening it, frames with the empty state, and when that first showed.
async fn open_the_dm_on_a_new_browser(page: &Page, dm: String) -> (f64, u64, Option<f64>) {
  sign_in_and_unlock(page, "carol").await;
  page.evaluate_on_new_document(FEED_RECORDER).await.unwrap();
  page.goto(format!("{WEB_ORIGIN}/dm/{dm}")).await.unwrap();
  let started = Instant::now();
  let mut has_shot = false;
  loop {
    let feed = evaluate_value(page, "window.__feed || null").await;
    if let Some(first_ms) = feed.get("firstMs").and_then(serde_json::Value::as_f64) {
      let empty_frames = feed
        .get("emptyFrames")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
      let first_empty_ms = feed.get("firstEmptyMs").and_then(serde_json::Value::as_f64);
      return (first_ms, empty_frames, first_empty_ms);
    }
    assert!(
      started.elapsed() < SHOWN_WITHIN,
      "no message of the DM showed within {SHOWN_WITHIN:?}: {feed}"
    );
    // E2E_SHOT_WHILE_WAITING=1: what the DM shows while it waits for its keys.
    if !has_shot
      && std::env::var("E2E_SHOT_WHILE_WAITING").is_ok()
      && started.elapsed() > Duration::from_secs(8)
    {
      crate::web::shot(page, "new-device-dm-waiting").await;
      has_shot = true;
    }

    tokio::time::sleep(Duration::from_millis(200)).await;
  }
}

async fn evaluate_value(page: &Page, script: &str) -> serde_json::Value {
  let params = EvaluateParams::builder()
    .expression(script)
    .return_by_value(true)
    .build()
    .unwrap();
  page
    .evaluate(params)
    .await
    .ok()
    .and_then(|result| result.into_value().ok())
    .unwrap_or(serde_json::Value::Null)
}
