//! Calls aren't possible here (a browser without WebTransport, a second tab of the account, a
//! client without call support): the call buttons still show, and pressing one explains why nothing
//! rings and what to do instead, instead of failing silently (#329, #322).

use leptos::prelude::*;

use crate::brand::Brand;
use crate::calls::CallsUnavailableReason;
use crate::components::icons::{Icon, IconName};
use crate::components::modal::Modal;
use crate::i18n::use_i18n;
use crate::state::use_app_state;

/// Where the desktop and mobile apps are offered when the brand names no page of its own
/// (`download_url`, docs/branding.md): the TeamCraft project's download page. Self-hosted
/// instances that ship their own builds set the brand's link instead.
pub const DEFAULT_DOWNLOAD_PAGE_URL: &str = "https://zeitgleich.github.io/TeamCraft/#download";

#[component]
pub fn CallsUnavailableDialog() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let brand = Brand::use_current().0;
  let reason = state.read(|backend| backend.calls_unavailable_reason());
  let (title_key, body_key) = match reason {
    CallsUnavailableReason::BrowserUnsupported => ("calls-unsupported-title", "calls-unsupported-body"),
    CallsUnavailableReason::OtherTab => ("calls-other-tab-title", "calls-other-tab-body"),
    CallsUnavailableReason::NotInThisApp => ("calls-browser-title", "calls-browser-body"),
  };
  let offers_apps = reason != CallsUnavailableReason::OtherTab;
  let download_page = move || {
    brand
      .with(|view| view.links.maybe_download.clone())
      .unwrap_or_else(|| DEFAULT_DOWNLOAD_PAGE_URL.to_owned())
  };
  view! {
    <Modal labelled_by="calls-unavailable-title" class="modal-notice">
      <header class="modal-head">
        <h2 id="calls-unavailable-title">{move || i18n.t(title_key)}</h2>
        <button class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
          <Icon name=IconName::Close />
        </button>
      </header>
      <div class="modal-body">
        <p>{move || i18n.t(body_key)}</p>
      </div>
      <footer class="modal-foot">
        <button class="btn" on:click=move |_| state.close_overlay()>
          {move || i18n.t("close")}
        </button>
        <Show when=move || offers_apps fallback=|| ()>
          <a class="btn btn-primary" href=download_page target="_blank" rel="noopener noreferrer" data-autofocus>
            <Icon name=IconName::Download />
            <span>{move || i18n.t("calls-browser-download")}</span>
          </a>
        </Show>
      </footer>
    </Modal>
  }
}
