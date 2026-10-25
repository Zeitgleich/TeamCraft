//! The split web bundle (#334): areas of the app load as wasm chunks of their own the first time they
//! open. The settings are one; opening them fetches their chunk and shows them.

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

/// The wasm chunks the page fetched.
async fn loaded_chunks(page: &chromiumoxide::page::Page) -> Vec<String> {
  page
    .evaluate("performance.getEntriesByType('resource').map(e => e.name).filter(n => n.endsWith('.wasm'))")
    .await
    .ok()
    .and_then(|result| result.into_value::<Vec<String>>().ok())
    .unwrap_or_default()
}
