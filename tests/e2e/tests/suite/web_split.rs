//! The split web bundle (#334): areas of the app load as wasm chunks of their own the first time they
//! open. The settings are one; opening them fetches their chunk and shows them. In a signed release
//! the service worker installs without the chunks and fetches and checks one when it is first used.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use chromiumoxide::page::Page;
use teamcraft_e2e::browser::web_dir;

use crate::web::{Rig, WEB_ORIGIN, sign_in_as_alice, wait_for_text};

/// Only the settings show it (an entry of their section list).
const SETTINGS_TEXT: &str = "Security & privacy";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_settings_load_their_own_chunk() {
  let Some(rig) = Rig::start().await else {
    return;
  };
  rig
    .browser
    .screenshot_on_failure("web-split-settings", |page| async move {
      sign_in_as_alice(&page).await;
      wait_for_text(&page, "browser-channel").await;
      assert!(
        !loaded_chunks(&page)
          .await
          .iter()
          .any(|chunk| chunk.contains("-split-settings-")),
        "the settings chunk loaded before the settings opened"
      );
      page.goto(format!("{WEB_ORIGIN}/settings")).await.unwrap();
      wait_for_text(&page, SETTINGS_TEXT).await;
      let chunks = loaded_chunks(&page).await;
      assert!(
        chunks.iter().any(|chunk| chunk.contains("-split-settings-")),
        "no settings chunk among {chunks:?}"
      );
    })
    .await;
  rig.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_signed_release_fetches_and_checks_a_chunk_when_it_is_used() {
  let Some(unsigned) = web_dir() else {
    // Rig::start says why it skips (or fails when the web build is required).
    assert!(Rig::start().await.is_none());
    return;
  };
  let signed = signed_copy(&unsigned);
  let Some(rig) = Rig::start_with(|builder| builder.with_web_dir(signed), &[]).await else {
    return;
  };
  rig
    .browser
    .screenshot_on_failure("web-split-signed", |page| async move {
      sign_in_as_alice(&page).await;
      wait_for_text(&page, "browser-channel").await;
      wait_for_verified_worker(&page).await;
      assert!(
        cached_chunks(&page).await.is_empty(),
        "the worker installed chunks the first visit doesn't need"
      );
      page.goto(format!("{WEB_ORIGIN}/settings")).await.unwrap();
      wait_for_text(&page, SETTINGS_TEXT).await;
      let cached = cached_chunks(&page).await;
      assert!(
        cached.iter().any(|chunk| chunk.contains("-split-settings-")),
        "the settings chunk was not verified into the release cache: {cached:?}"
      );
    })
    .await;
  rig.finish().await;
}

/// A copy of the web build signed with a fresh key, as the release does it.
fn signed_copy(unsigned: &Path) -> PathBuf {
  let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("web-signed-{}", std::process::id()));
  let dist = root.join("dist");
  let _ = std::fs::remove_dir_all(&root);
  std::fs::create_dir_all(&root).unwrap();
  let status = Command::new("cp")
    .arg("-R")
    .arg(unsigned)
    .arg(&dist)
    .status()
    .unwrap();
  assert!(status.success(), "copying the web build failed");
  let key = root.join("release-key.pem");
  let status = Command::new("openssl")
    .args(["genpkey", "-algorithm", "ed25519", "-out"])
    .arg(&key)
    .status()
    .expect("openssl is needed to sign a release");
  assert!(status.success(), "generating a release key failed");
  let script =
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../crates/teamcraft-ui/scripts/sign-web-release.sh");
  let status = Command::new("sh")
    .arg(script)
    .arg(&dist)
    .arg(&key)
    .arg("1.0.0-e2e")
    .status()
    .unwrap();
  assert!(status.success(), "signing the web build failed");
  dist
}

/// Waits until the service worker verified the release and controls the page.
async fn wait_for_verified_worker(page: &Page) {
  let script = r#"(async () => {
    const registration = await navigator.serviceWorker.ready;
    const state = await new Promise((resolve) => {
      navigator.serviceWorker.addEventListener('message', (event) => {
        if (event.data && event.data.type === 'bundle') resolve(event.data.state);
      });
      registration.active.postMessage({ type: 'status' });
    });
    return state + (navigator.serviceWorker.controller ? ' controlled' : '');
  })()"#;
  let deadline = Instant::now() + Duration::from_secs(60);
  loop {
    let answer: String = page.evaluate(script).await.unwrap().into_value().unwrap();
    if answer == "verified controlled" {
      return;
    }
    assert!(
      Instant::now() < deadline,
      "the worker did not verify the release: {answer}"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
  }
}

/// The split chunks in the worker's release caches.
async fn cached_chunks(page: &Page) -> Vec<String> {
  let script = r#"(async () => {
    const found = [];
    for (const name of await caches.keys()) {
      if (!name.startsWith('teamcraft-release-')) continue;
      for (const request of await (await caches.open(name)).keys()) {
        if (/-(split|chunk)-[^/]*\.wasm$/.test(request.url)) found.push(request.url);
      }
    }
    return found;
  })()"#;
  page.evaluate(script).await.unwrap().into_value().unwrap()
}

/// The wasm chunks the page fetched.
async fn loaded_chunks(page: &Page) -> Vec<String> {
  page
    .evaluate("performance.getEntriesByType('resource').map(e => e.name).filter(n => n.endsWith('.wasm'))")
    .await
    .ok()
    .and_then(|result| result.into_value::<Vec<String>>().ok())
    .unwrap_or_default()
}
