use leptos::prelude::*;

use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::icons::{Icon, IconName};
use crate::components::status_menu::StatusMenu;
use crate::i18n::use_i18n;
use crate::keys::{QUICK_SWITCHER, ShortcutKeys};
use crate::state::{Overlay, use_app_state};

/// Global header: search, help and the account/status menu.
#[component]
pub fn TopBar() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let me = Memo::new(move |_| state.me());
  let menu_open = move || matches!(state.overlay.get(), Overlay::StatusMenu);

  view! {
    <header class="top-bar">
      <div class="brand" translate="no">
        "TeamCraft"
      </div>
      <button class="search-btn" on:click=move |_| state.overlay.set(Overlay::QuickSwitcher)>
        <Icon name=IconName::Search />
        <span>{move || i18n.t("search-placeholder")}</span>
        <ShortcutKeys shortcut=QUICK_SWITCHER />
      </button>
      <div class="top-actions">
        <button
          class="icon-btn on-dark"
          aria-label=move || i18n.t("shortcuts-title")
          title=move || i18n.t("shortcuts-title")
          on:click=move |_| state.overlay.set(Overlay::Shortcuts)
        >
          <Icon name=IconName::Keyboard />
        </button>
        <div class="account">
          <button
            class="avatar-btn"
            aria-haspopup="menu"
            aria-expanded=move || menu_open().to_string()
            aria-label=move || {
              let user = me.get();
              i18n.t_with(
                "account-menu-label",
                &[("name", &user.display_name), ("status", &i18n.t(&format!("presence-{}", user.presence.slug())))],
              )
            }
            on:click=move |_| {
              state.overlay.update(|overlay| {
                *overlay = if *overlay == Overlay::StatusMenu { Overlay::None } else { Overlay::StatusMenu };
              });
            }
          >
            {move || view! { <Avatar user=me.get() size=AvatarSize::Medium show_presence=true /> }}
          </button>
          <Show when=menu_open fallback=|| ()>
            <StatusMenu />
          </Show>
        </div>
      </div>
    </header>
  }
}
