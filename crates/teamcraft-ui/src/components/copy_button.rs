use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::components::icons::{Icon, IconName};
use crate::components::message_actions::copy_text;
use crate::i18n::use_i18n;
use crate::reactive::SetIfAlive;
use crate::state::use_app_state;

/// Copies `text`; the label says "Copied" afterwards.
#[component]
pub fn CopyButton(#[prop(into)] text: Signal<String>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let is_copied = RwSignal::new(false);
  let copy = move |_| {
    let failed = i18n.t("action-copy-failed");
    // Started inside the click: browsers only allow clipboard writes from a user action.
    let copying = copy_text(&text.get_untracked());
    spawn_local(async move {
      if copying.await {
        is_copied.set_if_alive(true);
      } else {
        state.show_error(failed, String::new());
      }
    });
  };
  view! {
    <button type="button" class="btn" on:click=copy>
      <Icon name=IconName::Copy />
      <span aria-live="polite">{move || i18n.t(if is_copied.get() { "meetings-link-copied" } else { "admin-integrations-copy" })}</span>
    </button>
  }
}
