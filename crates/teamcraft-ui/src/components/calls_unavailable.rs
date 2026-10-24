//! Calls aren't in the browser yet (#330): the call buttons still show, and pressing one explains
//! why nothing rings and where the apps are, instead of failing silently (#329, #322).

use leptos::prelude::*;

use crate::components::icons::{Icon, IconName};
use crate::components::modal::Modal;
use crate::i18n::use_i18n;
use crate::state::use_app_state;

/// Where the desktop and mobile apps are offered.
pub const DOWNLOAD_PAGE_URL: &str = "https://zeitgleich.github.io/TeamCraft/#download";

#[component]
pub fn CallsUnavailableDialog() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  view! {
    <Modal labelled_by="calls-unavailable-title" class="modal-notice">
      <header class="modal-head">
        <h2 id="calls-unavailable-title">{move || i18n.t("calls-browser-title")}</h2>
        <button class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
          <Icon name=IconName::Close />
        </button>
      </header>
      <div class="modal-body">
        <p>{move || i18n.t("calls-browser-body")}</p>
      </div>
      <footer class="modal-foot">
        <button class="btn" on:click=move |_| state.close_overlay()>
          {move || i18n.t("close")}
        </button>
        <a class="btn btn-primary" href=DOWNLOAD_PAGE_URL target="_blank" rel="noopener noreferrer" data-autofocus>
          <Icon name=IconName::Download />
          <span>{move || i18n.t("calls-browser-download")}</span>
        </a>
      </footer>
    </Modal>
  }
}
