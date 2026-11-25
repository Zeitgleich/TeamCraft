//! A video plays while it streams in the web client whose core runs in the worker (#200, #2397):
//! the service worker's range reads go through the page to the core in the worker, which decrypts
//! only the chunks they cover.

use std::time::Duration;

use chromiumoxide::page::Page;
use teamcraft_core::client::OutgoingFile;
use teamcraft_e2e::browser::{BrowserSession, lease_web_port, prefer_mouse, web_dir};
use teamcraft_e2e::scenario::space_with;
use teamcraft_e2e::{Peer, Stack};
use teamcraft_proto::UserId;

use crate::perf::sign_in_and_unlock;
use crate::web::{TEST_CSP, shot, wait_for_element};

const PORT: u16 = 8083;
const WEB_ORIGIN: &str = "http://127.0.0.1:8083";
const WAIT: Duration = Duration::from_secs(60);
const CLIP: &[u8] = include_bytes!("../../../../crates/teamcraft-ui/demo-media/clip.webm");

/// Whether the page's store is on OPFS, where only the core's worker opens it.
const STORE_ON_OPFS: &str = "(async () => {
  const root = await navigator.storage.getDirectory();
  for await (const name of root.keys()) {
    if (name.startsWith('teamcraft-')) return true;
  }
  return false;
})()";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_video_streams_from_the_core_in_the_worker() {
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
    // The page reads the encrypted blob from the dev stack's storage.
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

  let alice_ref = &mut alice;
  browser
    .screenshot_on_failure("video-stream", |page| async move {
      prefer_mouse(&page).await;
      sign_in_and_unlock(&page, WEB_ORIGIN, "carol").await;
      let channel = alice_ref.client.create_channel(space, "clips").await.unwrap();
      alice_ref.client.add_member(channel, carol).await.unwrap();
      alice_ref
        .client
        .send_file(
          channel,
          None,
          OutgoingFile {
            file_name: "clip.webm".into(),
            media_type: "video/webm".into(),
            content: CLIP.to_vec(),
          },
        )
        .await
        .unwrap();
      page.goto(format!("{WEB_ORIGIN}/c/{channel}")).await.unwrap();
      wait_for_element(&page, ".media-tile [data-media-key]").await;
      let is_on_opfs: bool = page.evaluate(STORE_ON_OPFS).await.unwrap().into_value().unwrap();
      assert!(is_on_opfs, "the core runs in the worker over OPFS by default");
      page
        .evaluate("document.querySelector('.media-tile [data-media-key]').click()")
        .await
        .unwrap();
      wait_until(
        &page,
        "the lightbox plays the video from its stream",
        "(() => { const video = document.querySelector('video[src*=\"/media-stream/\"]'); \
         return !!video && video.readyState >= 2; })()",
      )
      .await;
      shot(&page, "video-stream").await;
    })
    .await;
  browser.close().await;
  stack.stop().await;
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
