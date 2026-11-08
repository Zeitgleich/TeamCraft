//! The split web bundle (#334): areas of the app load as wasm chunks of their own the first time they
//! open. The settings are one; opening them fetches their chunk and shows them. In a signed release
//! the service worker installs without the chunks and fetches and checks one when it is first used.
//! The UI's texts are files of their own, one per language, of which a page fetches the one it shows.

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
      record_every_resource(&page).await;
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
      // The chunks this visit loaded are in the cache now, but not the settings' (until the
      // worker fetches the rest in the background, a while after the page loaded).
      let cached = cached_chunks(&page).await;
      assert!(
        !cached.iter().any(|chunk| chunk.contains("-split-settings-")),
        "the worker installed the settings chunk before they were used: {cached:?}"
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

/// The UI's texts are a file per language (#1437): a page fetches only the one it shows, before its
/// first view; a switch in the settings fetches the other one and shows it, and the next load starts
/// in it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_fetches_only_the_language_it_shows() {
  let Some(rig) = Rig::start().await else {
    return;
  };
  rig
    .browser
    .screenshot_on_failure("web-split-texts", |page| async move {
      record_every_resource(&page).await;
      // Pins English before the app loads.
      sign_in_as_alice(&page).await;
      wait_for_text(&page, "browser-channel").await;
      let texts = loaded_texts(&page).await;
      assert!(
        texts.iter().any(|text| text.contains("/i18n/en-"))
          && !texts.iter().any(|text| text.contains("/i18n/de-")),
        "an English page fetched {texts:?}"
      );
      assert!(
        !texts.iter().any(|text| text.contains("-admin-")),
        "the console's texts loaded before the console opened: {texts:?}"
      );
      page
        .goto(format!("{WEB_ORIGIN}/settings/preferences"))
        .await
        .unwrap();
      wait_for_text(&page, "App language").await;
      page
        .evaluate(
          "(() => { const select = document.getElementById('set-settings-app-language-input'); \
           select.value = 'de'; select.dispatchEvent(new Event('change', { bubbles: true })); })()",
        )
        .await
        .unwrap();
      wait_for_text(&page, "Sprache der App").await;
      assert!(
        loaded_texts(&page)
          .await
          .iter()
          .any(|text| text.contains("/i18n/de-")),
        "the switch fetched no German texts"
      );

      page.reload().await.unwrap();
      let language = first_view_language(&page).await;
      assert_eq!(language, "de", "the first view after the switch");
      wait_for_text(&page, "Sprache der App").await;
      let texts = loaded_texts(&page).await;
      assert!(
        texts.iter().any(|text| text.contains("/i18n/de-"))
          && !texts.iter().any(|text| text.contains("/i18n/en-")),
        "a German page fetched {texts:?}"
      );
    })
    .await;
  rig.finish().await;
}

/// The page's language (`<html lang>`) once the app showed its first view (startup.rs).
async fn first_view_language(page: &Page) -> String {
  let script =
    "document.documentElement.hasAttribute('data-first-view') ? document.documentElement.lang : ''";
  let deadline = Instant::now() + Duration::from_secs(60);
  loop {
    let language: String = page
      .evaluate(script)
      .await
      .ok()
      .and_then(|result| result.into_value().ok())
      .unwrap_or_default();
    if !language.is_empty() {
      return language;
    }
    assert!(Instant::now() < deadline, "the app showed no first view");
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
}

/// The UI's text files the page fetched.
async fn loaded_texts(page: &Page) -> Vec<String> {
  page
    .evaluate("performance.getEntriesByType('resource').map(e => e.name).filter(n => n.endsWith('.ftl'))")
    .await
    .ok()
    .and_then(|result| result.into_value::<Vec<String>>().ok())
    .unwrap_or_default()
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

/// The browser keeps 250 resource timings per page by default, and the app alone loads more chunks
/// than that: the later ones, the settings among them, would never be listed.
async fn record_every_resource(page: &Page) {
  page
    .evaluate_on_new_document("performance.setResourceTimingBufferSize(100000)")
    .await
    .unwrap();
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
