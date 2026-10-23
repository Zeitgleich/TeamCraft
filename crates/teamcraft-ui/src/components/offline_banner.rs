//! A banner while the device has no network: messages wait until it is back.

use leptos::ev;
use leptos::prelude::*;

use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;

/// Shown at the top while the browser (or the desktop webview) reports no network connection.
#[component]
pub fn OfflineBanner() -> impl IntoView {
  let i18n = use_i18n();
  let is_online = RwSignal::new(navigator_online());
  let _ = window_event_listener(ev::online, move |_| is_online.set(true));
  let _ = window_event_listener(ev::offline, move |_| is_online.set(false));

  move || {
    (!is_online.get()).then(|| {
      view! {
        <div class="offline-banner" role="status">
          <Icon name=IconName::Alert />
          <span>{move || i18n.t("offline-banner")}</span>
        </div>
      }
    })
  }
}

fn navigator_online() -> bool {
  web_sys::window().is_none_or(|window| window.navigator().on_line())
}
