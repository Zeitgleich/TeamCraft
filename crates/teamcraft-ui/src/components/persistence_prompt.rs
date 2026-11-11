//! Keeping the local data in a browser that asks the user first (Firefox; e2ee.md §9.4). Browsers
//! may delete a site's data under storage pressure unless it is marked persistent; Chromium decides
//! that by itself, Firefox turns the request into a question nobody can judge without context. So
//! after sign-in the app explains it in a banner and only a click on "Continue" starts the request
//! (inside the click, or Firefox shows nothing). "Not now" and the browser's answer are kept per
//! user on this device: the banner asks once. Meeting guests never see it.

use leptos::prelude::*;

use crate::auth::tauri;
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::reactive::SetIfAlive;
use crate::settings::{load_string, store_string};
use crate::state::use_app_state;

/// Storage key prefix of "asked already on this device" (any answer); the user ID follows.
const ASKED_KEY_PREFIX: &str = "teamcraft.persistence_prompt.";
/// How long the answer stays in the banner before it goes.
const CONFIRMATION_MS: u64 = 4_000;

/// What came of the click on "Continue".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PersistOutcome {
  Kept,
  Refused,
}

/// Asks above the app, once per user and device, to let the browser keep the local data, where the
/// browser asks the user (see the module doc) and doesn't keep it yet.
#[component]
pub fn PersistencePrompt() -> impl IntoView {
  let i18n = use_i18n();
  let me = use_app_state().me().id;
  let asked_key = format!("{ASKED_KEY_PREFIX}{me}");
  let is_shown = RwSignal::new(false);
  let maybe_outcome = RwSignal::new(None::<PersistOutcome>);
  if !tauri::is_available() && tauri::has_core() && load_string(&asked_key).is_none() && asks_the_user() {
    leptos::task::spawn_local(async move {
      if !is_persisted().await {
        is_shown.set_if_alive(true);
      }
    });
  }
  let remember = {
    let asked_key = asked_key.clone();
    move || store_string(&asked_key, "1")
  };
  let keep = {
    let remember = remember.clone();
    move |_| {
      // Started here, inside the click: Firefox asks only for a request a person started.
      let request = request_persistence();
      remember();
      leptos::task::spawn_local(async move {
        let is_kept = request.await;
        let outcome = if is_kept {
          PersistOutcome::Kept
        } else {
          PersistOutcome::Refused
        };
        maybe_outcome.set_if_alive(Some(outcome));
        set_timeout(
          move || {
            let _ = is_shown.try_set(false);
          },
          std::time::Duration::from_millis(CONFIRMATION_MS),
        );
      });
    }
  };
  let later = move |_| {
    remember();
    is_shown.set(false);
  };
  move || {
    is_shown.get().then(|| {
      let outcome = maybe_outcome.get();
      let text = move || i18n.t(text_key(outcome));
      view! {
        <div class="update-banner" role="status">
          <Icon name=IconName::Lock />
          <span>{text}</span>
          {outcome.is_none().then(|| view! {
            <button class="btn btn-primary btn-small" on:click=keep.clone()>
              {move || i18n.t("persist-prompt-continue")}
            </button>
            <button class="btn btn-small" on:click=later.clone()>
              {move || i18n.t("persist-prompt-later")}
            </button>
          })}
        </div>
      }
    })
  }
}

#[cfg(target_arch = "wasm32")]
fn asks_the_user() -> bool {
  crate::web_core::persistence_asks_the_user()
}

#[cfg(not(target_arch = "wasm32"))]
fn asks_the_user() -> bool {
  false
}

#[cfg(target_arch = "wasm32")]
async fn is_persisted() -> bool {
  crate::web_core::is_storage_persisted().await
}

#[cfg(not(target_arch = "wasm32"))]
async fn is_persisted() -> bool {
  true
}

/// The browser's request, started at once (inside the click); the future says whether it kept the
/// data.
#[cfg(target_arch = "wasm32")]
fn request_persistence() -> impl std::future::Future<Output = bool> {
  let maybe_request = crate::web_core::request_storage_persistence();
  async move {
    match maybe_request {
      Some(request) => wasm_bindgen_futures::JsFuture::from(request)
        .await
        .is_ok_and(|granted| granted.is_truthy()),
      None => false,
    }
  }
}

#[cfg(not(target_arch = "wasm32"))]
fn request_persistence() -> impl std::future::Future<Output = bool> {
  std::future::ready(false)
}

/// The banner's text: the explanation, or what came of the click.
fn text_key(maybe_outcome: Option<PersistOutcome>) -> &'static str {
  match maybe_outcome {
    None => "persist-prompt-text",
    Some(PersistOutcome::Kept) => "persist-prompt-kept",
    Some(PersistOutcome::Refused) => "persist-prompt-refused",
  }
}

#[cfg(test)]
mod tests {
  use crate::i18n::{Locale, translate};

  #[test]
  fn the_banner_is_translated() {
    for locale in [Locale::En, Locale::De] {
      for key in [
        "persist-prompt-text",
        "persist-prompt-continue",
        "persist-prompt-later",
        "persist-prompt-kept",
        "persist-prompt-refused",
      ] {
        assert_ne!(translate(locale, key, None), key, "{locale:?} lacks {key}");
      }
    }
  }
}
