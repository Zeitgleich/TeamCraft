//! Animated images scrolled out of view cost nothing (issue #201): their moving copy leaves the DOM,
//! no animation runs and the browser stays idle. A positive control shows the same images moving
//! while they are on screen, so the check can't pass because nothing ever moved.

use std::time::Duration;

use chromiumoxide::page::Page;
use teamcraft_e2e::browser::{BrowserSession, lease_web_port, prefer_mouse, web_dir};
use teamcraft_e2e::resources::ProcessUsage;
use teamcraft_e2e::scenario::space_with;
use teamcraft_e2e::{Peer, Stack};
use teamcraft_proto::{ConversationId, UserId};

use crate::pending_echo::type_into_composer;
use crate::perf::sign_in_and_unlock;
use crate::web::{TEST_CSP, wait_for_element};

const PORT: u16 = 8083;
const WEB_ORIGIN: &str = "http://127.0.0.1:8083";
const WAIT: Duration = Duration::from_secs(60);
/// Animated images in the conversation.
const ANIMATED_IMAGES: usize = 6;
/// Text messages after them: enough to push them far out of view.
const FILLER_MESSAGES: usize = 60;
/// Time for the arriving messages' entry motion to end; the images' animations never would.
const ARRIVAL_SETTLES: Duration = Duration::from_secs(5);
/// How long the idle CPU is measured with every animation off screen.
const IDLE_WINDOW: Duration = Duration::from_secs(10);
/// Budget for the browser's processes together while idle (docs/performance.md, "Clients").
const MAX_IDLE_CPU_PERCENT: f64 = 2.0;
/// A two-frame GIF, 160 x 120.
const GIF_BASE64: &str = "R0lGODlhoAB4AIEAAMgyMgAAAAAAAAAAACH/C05FVFNDQVBFMi4wAwEAAAAh+QQACgAAACwAAAAAoAB4AAAI3wABCBxIsKDBgwgTKlzIsKHDhxAjSpxIsaLFixgzatzIsaPHjyBDihxJsqTJkyhTqlzJsqXLlzBjypxJs6bNmzhz6tzJs6fPn0CDCh1KtKjRo0iTKl3KtKnTp1CjSp1KtarVq1izat3KtavXr2DDih1LtqzZs2jTql3Ltq3bt3Djyp1Lt67du3jz6t3Lt6/fv4ADCx5MuLDhw4gTK17MuLHjx5AjS55MubLly5gza97MubPnz6BDix5NurTp06hTq17NurXr17Bjy55Nu7bt27hz697Nu7fv38CDCx+uNSAAIfkEAQoAAQAsAAAAAKAAeACBMsgyAAAAAAAAAAAACN8AAQgcSLCgwYMIEypcyLChw4cQI0qcSLGixYsYM2rcyLGjx48gQ4ocSbKkyZMoU6pcybKly5cwY8qcSbOmzZs4c+rcybOnz59AgwodSrSo0aNIkypdyrSp06dQo0qdSrWq1atYs2rdyrWr169gw4odS7as2bNo06pdy7at27dw48qdS7eu3bt48+rdy7ev37+AAwseTLiw4cOIEytezLix48eQI0ueTLmy5cuYM2vezLmz58+gQ4seTbq06dOoU6tezbq169ewY8ueTbu27du4c+vezbu379/AgwsfrjUgADs=";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn animated_images_out_of_view_cost_nothing() {
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
    // The images go up from the page, to the dev stack's storage.
    .with_env(
      "TEAMCRAFT_WEB__CONTENT_SECURITY_POLICY",
      &TEST_CSP.replace("connect-src 'self'", "connect-src 'self' http://127.0.0.1:3900"),
    )
    .start()
    .await
  else {
    return;
  };
  let Some(browser) = BrowserSession::launch().await else {
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
  let channel = alice.client.create_channel(space, "gifs").await.unwrap();
  alice.client.add_member(channel, carol).await.unwrap();

  let profile = browser.profile_dir().to_string_lossy().into_owned();
  let alice_ref = &mut alice;
  browser
    .screenshot_on_failure("animated-images", |page| async move {
      prefer_mouse(&page).await;
      sign_in_and_unlock(&page, "carol").await;
      page.goto(format!("{WEB_ORIGIN}/c/{channel}")).await.unwrap();
      wait_for_element(&page, "[data-composer]").await;
      let_the_browser_in(alice_ref, &page).await;

      send_animated_images(&page).await;
      send_filler(alice_ref, channel).await;
      wait_until(
        &page,
        "the newest message shows",
        "document.body.innerText.includes('filler 59')",
      )
      .await;

      scroll_list(&page, "list.scrollHeight").await;
      wait_until(&page, "the images left the view", &moving_images_are("=== 0")).await;
      // The filler's arrival motion ends by itself; nothing may run on after it.
      let deadline = tokio::time::Instant::now() + ARRIVAL_SETTLES;
      loop {
        let running = running_animations(&page).await;
        if running.is_empty() {
          break;
        }
        assert!(
          tokio::time::Instant::now() < deadline,
          "animations run out of view: {running:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
      }
      let cpu = idle_cpu(&profile).await;
      assert!(
        cpu <= MAX_IDLE_CPU_PERCENT,
        "animated images out of view keep the browser busy: {cpu:.2} % CPU (budget {MAX_IDLE_CPU_PERCENT} %)"
      );

      // Control: back in view they move.
      scroll_list(&page, "0").await;
      wait_until(&page, "the visible images move", &moving_images_are("> 0")).await;
      // And only the visible ones: not all of them at once on a short screen.
      let moving = count(&page, "document.querySelectorAll('img.media-moving').length").await;
      assert!(moving <= ANIMATED_IMAGES, "{moving} moving images");

      scroll_list(&page, "list.scrollHeight").await;
      wait_until(&page, "they stop again", &moving_images_are("=== 0")).await;
    })
    .await;
  browser.close().await;
  stack.stop().await;
}

/// Pumps Alice's device until it added the browser, a new device of Carol's, to the channel's group:
/// until then what the page sends waits on the device ("isn't fully set up").
async fn let_the_browser_in(alice: &mut Peer, page: &Page) {
  let deadline = tokio::time::Instant::now() + WAIT;
  let mut maybe_clear_since: Option<tokio::time::Instant> = None;
  let mut round = 0_u32;
  loop {
    let text = crate::web::body_text(page).await;
    if text.contains("isn't fully set up") {
      maybe_clear_since = None;
    } else if maybe_clear_since
      .get_or_insert_with(tokio::time::Instant::now)
      .elapsed()
      >= Duration::from_secs(3)
    {
      return;
    }
    assert!(
      tokio::time::Instant::now() < deadline,
      "the browser was never added to the channel:\n{text}"
    );
    if round.is_multiple_of(50) {
      alice.heartbeat().await;
    }
    round += 1;
    alice.pump_reconnecting().await;
    alice.client.apply_pending_changes().await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
}

/// Each image goes up as its own message, with a caption, from the page's file input.
async fn send_animated_images(page: &Page) {
  for index in 0..ANIMATED_IMAGES {
    let script = format!(
      "(() => {{
        const bytes = Uint8Array.from(atob('{GIF_BASE64}'), c => c.charCodeAt(0));
        const transfer = new DataTransfer();
        transfer.items.add(new File([bytes], 'moving-{index}.gif', {{ type: 'image/gif' }}));
        const input = document.querySelector('input[type=file][multiple]');
        input.files = transfer.files;
        input.dispatchEvent(new Event('change', {{ bubbles: true }}));
      }})()"
    );
    page.evaluate(script).await.unwrap();
    let composer = type_into_composer(page, &format!("animation {index}")).await;
    composer.press_key("Enter").await.unwrap();
    wait_until(
      page,
      "the image is sent",
      &format!(
        "document.querySelectorAll('.media-tile [data-media-key]').length === {}",
        index + 1
      ),
    )
    .await;
  }
}

async fn send_filler(alice: &mut Peer, channel: ConversationId) {
  for index in 0..FILLER_MESSAGES {
    alice
      .client
      .send_text(channel, None, &format!("filler {index}"))
      .await
      .unwrap();
  }
}

/// Moves the message list to `top` (a script expression, `list` is the list element).
async fn scroll_list(page: &Page, top: &str) {
  // The synthetic wheel event says a person scrolled, so the list doesn't snap back to the bottom.
  page
    .evaluate(format!(
      "(() => {{ const list = document.querySelector('.message-list'); \
         list.dispatchEvent(new WheelEvent('wheel', {{ deltaY: -1, bubbles: true }})); \
         list.scrollTop = {top}; }})()"
    ))
    .await
    .unwrap();
}

/// What runs: each running animation's name and the classes of the element it moves.
async fn running_animations(page: &Page) -> Vec<String> {
  page
    .evaluate(
      "document.getAnimations().filter(a => a.playState === 'running')\
        .map(a => `${a.animationName ?? a.id} on ${a.effect?.target?.className ?? '?'}`)",
    )
    .await
    .unwrap()
    .into_value()
    .unwrap_or_default()
}

fn moving_images_are(comparison: &str) -> String {
  format!("document.querySelectorAll('img.media-moving').length {comparison}")
}

async fn count(page: &Page, script: &str) -> usize {
  page
    .evaluate(script)
    .await
    .unwrap()
    .into_value::<f64>()
    .unwrap_or(0.0) as usize
}

/// CPU (percent of one core) of the browser's processes over [`IDLE_WINDOW`].
async fn idle_cpu(profile: &str) -> f64 {
  let before = ProcessUsage::of_matching(profile);
  tokio::time::sleep(IDLE_WINDOW).await;
  let after = ProcessUsage::of_matching(profile);
  after.cpu_time.saturating_sub(before.cpu_time).as_secs_f64() / IDLE_WINDOW.as_secs_f64() * 100.0
}

async fn wait_until(page: &Page, what: &str, condition: &str) {
  let deadline = tokio::time::Instant::now() + WAIT;
  while tokio::time::Instant::now() < deadline {
    let is_met: bool = page
      .evaluate(condition)
      .await
      .ok()
      .and_then(|value| value.into_value().ok())
      .unwrap_or(false);
    if is_met {
      return;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
  panic!("waited in vain until {what}");
}
