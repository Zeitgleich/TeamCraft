//! A banner when the browser does not save the local data (e2ee.md §9.4): no storage space left,
//! or a newer version of the app took the database over. What is not saved is lost on reload, so
//! the user has to know.

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_client_api::remote::{CORE_CHANGED_EVENT, StoreHealth};

use crate::auth::tauri;
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::remote_backend;

/// Shown above the app while local writes fail. Browser only: the desktop store reports failures
/// as errors of the action that wrote.
#[component]
pub fn StorageBanner() -> impl IntoView {
  let i18n = use_i18n();
  let health = RwSignal::new(StoreHealth::Saved);
  if !tauri::is_available() && tauri::has_core() {
    let refresh = move || {
      spawn_local(async move {
        if let Ok(next) = remote_backend::store_health().await {
          health.set(next);
        }
      });
    };
    refresh();
    tauri::listen(CORE_CHANGED_EVENT, refresh);
  }
  move || {
    let current = health.get();
    let key = message_key(&current)?;
    let needs_reload = current == StoreHealth::Closed;
    Some(view! {
      <div class="update-banner" role="alert">
        <Icon name=IconName::Alert />
        <span>{move || i18n.t(key)}</span>
        {needs_reload.then(|| view! {
          <button class="btn btn-primary btn-small" on:click=move |_| reload()>
            {move || i18n.t("storage-reload")}
          </button>
        })}
      </div>
    })
  }
}

fn message_key(health: &StoreHealth) -> Option<&'static str> {
  match health {
    StoreHealth::Saved => None,
    StoreHealth::QuotaExceeded => Some("storage-quota-exceeded"),
    StoreHealth::Closed => Some("storage-closed"),
    StoreHealth::Failed | StoreHealth::Unknown => Some("storage-not-saved"),
  }
}

fn reload() {
  if let Some(window) = web_sys::window()
    && let Err(error) = window.location().reload()
  {
    tracing::warn!(?error, "could not reload");
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::i18n::{Locale, translate};

  #[test]
  fn every_problem_has_a_translated_message() {
    let problems = [
      StoreHealth::QuotaExceeded,
      StoreHealth::Closed,
      StoreHealth::Failed,
      StoreHealth::Unknown,
    ];
    for locale in [Locale::En, Locale::De] {
      for problem in &problems {
        let key = message_key(problem).expect("a problem has a message");
        assert_ne!(translate(locale, key, None), key, "{locale:?} lacks {key}");
      }
      assert_ne!(translate(locale, "storage-reload", None), "storage-reload");
    }
    assert_eq!(message_key(&StoreHealth::Saved), None);
  }
}
