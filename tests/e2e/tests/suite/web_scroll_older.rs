//! Scrolling up with a mouse wheel through history the device doesn't hold yet (#1595): the pages
//! of older history arrive and the rows above the reader get their real heights while the reader
//! scrolls, and every notch of the wheel still moves the messages by its own distance. Setting the
//! scroll position to make up for rows above ended the wheel's smooth scrolling (stutter) or fought
//! with it (jumps).

use std::time::{Duration, Instant};

use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;
use chromiumoxide::page::Page;
use teamcraft_e2e::StackBuilder;

use crate::web::{Rig, WEB_ORIGIN, body_text, sign_in_as_alice, wait_for_text};
use crate::web_feed_stability::wait_until_readable;

/// Many server pages of older history (50 messages each) beyond the newest one the browser holds.
const MESSAGES: usize = 1_200;
/// How long the reader may scroll before the beginning must have been reached.
const SCROLL_DEADLINE: Duration = Duration::from_secs(600);
/// Scrolls up like a mouse wheel: a notch of 120 px, scrolled smoothly as browsers do, the next one
/// once it is over. After each notch the message that was at the top must have moved down by
/// exactly the notch; at the very top, where the list waits for
/// the next page, there is nothing to compare. Stops at the first message.
const SCROLL_UP_BY_NOTCHES: &str = r#"window.__scrollOlder = null, new Promise(resolve => {
  const list = document.querySelector('.message-list');
  const notch = 120;
  const stats = { notches: 0, notches_off: 0, moved_px: [], notches_at_top: 0 };
  const started = performance.now();
  const topMessage = () => {
    const top = list.getBoundingClientRect().top;
    const row = [...list.querySelectorAll('.row[data-idx]')]
      .find(row => row.getBoundingClientRect().top > top + 40);
    const id = row?.querySelector('[data-msg-id]')?.dataset.msgId;
    return id ? { id, top: row.getBoundingClientRect().top } : null;
  };
  // Once the smooth scroll of a notch is over: the position unchanged for four frames.
  const afterRest = done => {
    let last = list.scrollTop, still = 0;
    const until = performance.now() + 1500;
    const frame = () => {
      still = list.scrollTop === last ? still + 1 : 0;
      last = list.scrollTop;
      if (still >= 4 || performance.now() > until) done(); else requestAnimationFrame(frame);
    };
    requestAnimationFrame(frame);
  };
  const step = () => {
    const isDone = list.textContent.includes('the very first') && list.scrollTop === 0;
    if (isDone || performance.now() - started > 600000) {
      stats.reached_beginning = isDone;
      resolve(JSON.stringify(stats));
      return;
    }
    const before = list.scrollTop;
    const seen = before > 2 * notch ? topMessage() : null;
    if (!seen) stats.notches_at_top += 1;
    list.dispatchEvent(new WheelEvent('wheel', { deltaY: -notch, bubbles: true }));
    list.scrollBy({ top: -notch, behavior: 'smooth' });
    afterRest(() => {
      stats.notches += 1;
      const message = seen && list.querySelector(`[data-msg-id="${seen.id}"]`);
      if (message) {
        const moved = message.closest('.row').getBoundingClientRect().top - seen.top;
        if (Math.abs(moved - notch) > 10) {
          stats.notches_off += 1;
          if (stats.moved_px.length < 30) stats.moved_px.push(Math.round(moved));
        }
      }
      step();
    });
  };
  step();
}).then(json => { window.__scrollOlder = json; })"#;

/// What [`SCROLL_UP_BY_NOTCHES`] measured.
#[derive(Debug)]
struct ScrollStats {
  notches: f64,
  notches_off: f64,
  reached_beginning: bool,
  /// Everything it measured, for the log.
  all: serde_json::Value,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_wheel_notch_moves_the_messages_while_older_history_arrives() {
  // Boxed: the scenario's future is large in a debug build.
  Box::pin(scenario()).await;
}

async fn scenario() {
  // Many messages in a row; the limits are not what this test looks at.
  let unlimited =
    |builder: StackBuilder| builder.with_env("TEAMCRAFT_SECURITY__RATE_LIMITS__IS_ENABLED", "false");
  let Some(mut rig) = Rig::start_with(unlimited, &[]).await else {
    return;
  };
  let dm = rig.alice.client.open_dm(rig.carol.user_id()).await.unwrap();
  rig.carol.wait_joined(dm).await;
  let carol = &mut rig.carol.client;
  carol.send_text(dm, None, "the very first").await.unwrap();
  for index in 0..MESSAGES {
    carol.send_text(dm, None, &message(index)).await.unwrap();
  }
  carol.send_text(dm, None, "hello from carol").await.unwrap();
  rig.alice.wait_for_text(dm, "hello from carol").await;
  rig.alice.client.sync_account_data().await.unwrap();

  let page = rig.browser.page.clone();
  sign_in_as_alice(&page).await;
  wait_for_text(&page, "browser-channel").await;
  page.goto(format!("{WEB_ORIGIN}/dm/{dm}")).await.unwrap();
  wait_until_readable(&mut rig.alice, &mut rig.carol, &page, "line ").await;
  // From the newest message, like a reader who opens the conversation and scrolls back.
  page
    .evaluate("document.querySelector('.jump-latest')?.click()")
    .await
    .unwrap();
  tokio::time::sleep(Duration::from_secs(2)).await;

  let started = Instant::now();
  let stats = scroll_up_by_notches(&page).await;
  eprintln!(
    "scrolling up through older history by wheel notches ({:.1} s): {}",
    started.elapsed().as_secs_f64(),
    stats.all
  );
  assert!(
    stats.reached_beginning,
    "the first message was not reached in {SCROLL_DEADLINE:?}: {stats:?}\n{}",
    body_text(&page).await
  );
  // Near the very top the list still has to move the scroll position once in a while (the spacer
  // ran out), and a notch lands a little off on a loaded runner: about 3 in 100 here. The bug moved
  // more than 80 in 100.
  assert!(
    stats.notches_off <= stats.notches / 10.0,
    "wheel notches didn't move the messages by their distance: {stats:?}"
  );
  rig.finish().await;
}

/// One to three lines, like a conversation: the rows differ in height.
fn message(index: usize) -> String {
  let words = "and a little more text so that the line wraps on a narrow window";
  match index % 5 {
    0 => format!("line {index}: {words}, {words}, {words}"),
    1 | 3 => format!("line {index}: {words}"),
    _ => format!("line {index}"),
  }
}

async fn scroll_up_by_notches(page: &Page) -> ScrollStats {
  // Longer than a DevTools request may take: started here, its result read once it is there.
  page
    .evaluate(format!("void ({SCROLL_UP_BY_NOTCHES})"))
    .await
    .unwrap();
  let deadline = Instant::now() + SCROLL_DEADLINE + Duration::from_secs(30);
  let json = loop {
    let params = EvaluateParams::builder()
      .expression("window.__scrollOlder || ''")
      .return_by_value(true)
      .build()
      .unwrap();
    let json: String = page.evaluate(params).await.unwrap().into_value().unwrap();
    if !json.is_empty() {
      break json;
    }
    assert!(Instant::now() < deadline, "the scrolling never ended");
    tokio::time::sleep(Duration::from_millis(500)).await;
  };
  let all: serde_json::Value = serde_json::from_str(&json).unwrap();
  let number = |name: &str| all[name].as_f64().unwrap_or_default();
  ScrollStats {
    notches: number("notches"),
    notches_off: number("notches_off"),
    reached_beginning: all["reached_beginning"].as_bool().unwrap_or_default(),
    all,
  }
}
