//! A banner when this browser can't run the client core in its worker (#2397): it lacks module
//! workers, Web Locks or OPFS sync access handles (an old version, or a private window that blocks
//! the storage), so the core runs in the page and long work there can hold up the screen. The
//! user can update the browser or use the desktop app; the banner says so once a session.

use leptos::prelude::*;

use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;

/// Shown above the app while the core runs in the page for want of browser features. Members only:
/// a guest's core always runs in the page.
#[component]
pub fn BrowserSupportBanner() -> impl IntoView {
  let i18n = use_i18n();
  move || {
    is_shown().then(|| {
      view! {
        <div class="update-banner" role="status">
          <Icon name=IconName::Alert />
          <span>{move || i18n.t("browser-unsupported")}</span>
          <button class="btn btn-small" on:click=move |_| dismiss()>
            {move || i18n.t("browser-unsupported-dismiss")}
          </button>
        </div>
      }
    })
  }
}

#[cfg(target_arch = "wasm32")]
fn is_shown() -> bool {
  crate::web_core::shows_browser_unsupported()
}

#[cfg(not(target_arch = "wasm32"))]
fn is_shown() -> bool {
  false
}

#[cfg(target_arch = "wasm32")]
fn dismiss() {
  crate::web_core::dismiss_browser_unsupported();
}

#[cfg(not(target_arch = "wasm32"))]
fn dismiss() {}

#[cfg(test)]
mod tests {
  use crate::i18n::{Locale, translate};

  #[test]
  fn the_banner_is_translated() {
    for locale in [Locale::En, Locale::De] {
      for key in ["browser-unsupported", "browser-unsupported-dismiss"] {
        assert_ne!(translate(locale, key, None), key, "{locale:?} lacks {key}");
      }
    }
  }
}
