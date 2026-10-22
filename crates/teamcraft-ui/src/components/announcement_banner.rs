//! The announcement banner admins publish (PLAN.md §13): above the whole app until dismissed or expired.

use leptos::prelude::*;
use teamcraft_proto::AnnouncementLevel;

use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::state::use_app_state;

/// The banner in force for the signed-in user; renders nothing without one.
#[component]
pub fn AnnouncementBanner() -> impl IntoView {
  let state = use_app_state();
  let current = Memo::new(move |_| state.read(|backend| backend.current_announcement()));
  move || {
    current.get().map(|announcement| {
      let id = announcement.id;
      view! {
        <BannerView
          text=announcement.text.clone()
          level=announcement.level.clone()
          is_dismissible=announcement.is_dismissible
          on_dismiss=Callback::new(move |()| state.write(|backend| backend.dismiss_announcement(id)))
        />
      }
    })
  }
}

/// One banner; also used for the preview in the admin console.
#[component]
pub fn BannerView(
  #[prop(into)] text: Signal<String>,
  #[prop(into)] level: Signal<AnnouncementLevel>,
  #[prop(into)] is_dismissible: Signal<bool>,
  #[prop(optional)] on_dismiss: Option<Callback<()>>,
) -> impl IntoView {
  let i18n = use_i18n();
  let class = move || format!("announcement-banner announcement-{}", level.get().as_str());
  view! {
    <div class=class role=move || if level.get() == AnnouncementLevel::Critical { "alert" } else { "status" }>
      <Icon name=IconName::Alert />
      <span class="announcement-text" translate="no">{move || text.get()}</span>
      <Show when=move || is_dismissible.get() && on_dismiss.is_some() fallback=|| ()>
        <button
          class="icon-button"
          aria-label=move || i18n.t("announcement-dismiss")
          on:click=move |_| {
            if let Some(dismiss) = on_dismiss {
              dismiss.run(());
            }
          }
        >
          <Icon name=IconName::Close />
        </button>
      </Show>
    </div>
  }
}
