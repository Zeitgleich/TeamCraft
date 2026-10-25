use leptos::prelude::*;

use crate::brand::Brand;
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::brand_logo::BrandLogo;
use crate::components::icons::{Icon, IconName};
use crate::components::status_menu::StatusMenu;
use crate::components::window_controls::{ClickOrDrag, window_drag_handler};
use crate::i18n::use_i18n;
use crate::keys::{QUICK_SWITCHER, ShortcutKeys};
use crate::state::{Overlay, use_app_state};

/// Global header: search, help and the account/status menu. In the desktop app it is also the
/// window's title bar: a press on its empty parts moves the window, and so does dragging the brand, which a click
/// sends to Chats.
#[component]
pub fn TopBar() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let me = Memo::new(move |_| state.me());
  let menu_open = move || matches!(state.overlay.get(), Overlay::StatusMenu);
  let on_drag = window_drag_handler();
  let brand = Brand::use_current().0;
  let press = ClickOrDrag::new();

  view! {
    <header class="top-bar" data-window-drag="" on:mousedown=on_drag>
      <a
        class="brand"
        href="/home"
        translate="no"
        aria-label=move || i18n.t("brand-home-label")
        on:mousedown=move |event| press.press(&event)
        on:mousemove=move |event| press.moved(&event)
        on:mouseup=move |_| press.release()
        on:mouseleave=move |_| press.release()
        on:click=move |event| {
          // Modified clicks keep the browser's own link behavior (new tab); a drag is no click.
          if event.ctrl_key() || event.meta_key() || event.shift_key() || event.button() != 0 {
            return;
          }
          event.prevent_default();
          if !press.take_was_drag() {
            state.open_chats();
          }
        }
      >
        <BrandLogo class="brand-logo" fallback=|| () />
        <span class="brand-name">{move || brand.with(|view| view.display_name.clone())}</span>
      </a>
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
          // On phones the menu opens from the bottom navigation instead (app.rs).
          <Show when=move || menu_open() && !state.is_compact() fallback=|| ()>
            <StatusMenu />
          </Show>
        </div>
      </div>
    </header>
  }
}
