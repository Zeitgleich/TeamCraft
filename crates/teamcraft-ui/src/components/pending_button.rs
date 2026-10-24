//! A button that starts async work (issue #322): disabled with a spinner while its
//! [`PendingAction`] runs, so a second click can't start it twice and "nothing happens" can't occur.

use leptos::prelude::*;

use crate::feedback::PendingAction;
use crate::i18n::use_i18n;

#[component]
pub fn PendingButton(
  pending: PendingAction,
  #[prop(into)] class: String,
  /// Disabled for another reason too (nothing chosen yet).
  #[prop(optional, into)]
  is_disabled: Signal<bool>,
  on_click: impl FnMut(leptos::ev::MouseEvent) + 'static,
  children: Children,
) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <button
      type="button"
      class=class
      prop:disabled=move || pending.is_pending() || is_disabled.get()
      aria-busy=move || pending.is_pending().then_some("true")
      on:click=on_click
    >
      <Show when=move || pending.is_pending() fallback=|| ()>
        <span class="spinner" role="status" aria-label=move || i18n.t("action-running")></span>
      </Show>
      {children()}
    </button>
  }
}
