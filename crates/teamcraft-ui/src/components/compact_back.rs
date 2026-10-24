//! The back button of the compact layout, at the start of a page's header. Wider layouts have the
//! sidebar instead, so the stylesheet hides it there.

use leptos::prelude::*;

use crate::components::icons::{Icon, IconName};
use crate::history::back_or_up;
use crate::i18n::use_i18n;
use crate::state::use_app_state;

#[component]
pub fn CompactBack() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  view! {
    <button
      type="button"
      class="icon-btn compact-back"
      aria-label=move || i18n.t("back")
      title=move || i18n.t("back")
      on:click=move |_| back_or_up(state)
    >
      <Icon name=IconName::ChevronLeft />
    </button>
  }
}
