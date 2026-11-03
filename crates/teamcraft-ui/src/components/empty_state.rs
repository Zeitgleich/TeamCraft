//! The one empty state of every list and view: an icon, one headline, one sentence and at most one
//! action (issue #273, design reference).

use leptos::prelude::*;

use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;

/// The single next step an empty state offers.
#[derive(Clone, Copy)]
pub struct EmptyAction {
  pub label_key: &'static str,
  pub on_click: Callback<()>,
}

/// An empty list or view. `as_item` renders it as an `<li>` for use inside a list.
#[component]
pub fn EmptyState(
  icon: IconName,
  title_key: &'static str,
  body_key: &'static str,
  #[prop(default = None)] maybe_action: Option<EmptyAction>,
  #[prop(optional)] as_item: bool,
) -> impl IntoView {
  let i18n = use_i18n();
  let content = view! {
    <span class="empty-icon" aria-hidden="true"><Icon name=icon /></span>
    <p class="empty-title">{move || i18n.t(title_key)}</p>
    <p class="empty-body">{move || i18n.t(body_key)}</p>
    {maybe_action.map(|action| {
      view! {
        <button class="btn btn-primary" on:click=move |_| action.on_click.run(())>
          {move || i18n.t(action.label_key)}
        </button>
      }
    })}
  };
  if as_item {
    view! { <li class="empty-state is-rich">{content}</li> }.into_any()
  } else {
    view! { <div class="empty-state is-rich">{content}</div> }.into_any()
  }
}
