use leptos::prelude::*;

use crate::brand::Brand;
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::brand_logo::BrandLogo;
use crate::components::channel_glyph::ChannelGlyph;
use crate::components::icons::{Icon, IconName};
use crate::components::status_menu::StatusMenu;
use crate::components::window_controls::{ClickOrDrag, window_drag_handler};
use crate::i18n::use_i18n;
use crate::keys::{QUICK_SWITCHER, ShortcutKeys};
use crate::model::{Conversation, ConversationId, ConversationKind};
use crate::state::{AppState, Overlay, use_app_state};

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
      <SearchField />
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

/// The global search in the top bar (#724). With a conversation open, the search starts limited to
/// it, and the field says so with a chip (its picture or glyph and name); the chip's x searches
/// everywhere instead until another conversation opens. The `in:` syntax behind it isn't shown;
/// typing it in the search still works.
#[component]
fn SearchField() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let maybe_dismissed = RwSignal::new(None::<ConversationId>);
  let maybe_scope = Memo::new(move |_| {
    let id = state.current_conversation_id()?;
    if maybe_dismissed.get() == Some(id) {
      return None;
    }
    state.read(|backend| backend.conversation(id))
  });
  let open = move || {
    if maybe_scope.get_untracked().is_some() {
      state.open_scoped_switcher();
    } else {
      state.open_switcher("");
    }
  };
  let label = move || match maybe_scope.get() {
    Some(conversation) => i18n.t_with("search-in", &[("name", &conversation.name)]),
    None => i18n.t("search-placeholder"),
  };

  view! {
    // A press on the icon or the chip opens the search too; the button is the control.
    <div class="search-field" class:is-scoped=move || maybe_scope.with(Option::is_some) on:click=move |_| open()>
      <span class="search-field-icon" aria-hidden="true">
        <Icon name=IconName::Search />
      </span>
      {move || {
        maybe_scope
          .get()
          .map(|conversation| {
            let id = conversation.id;
            view! {
              <span class="search-field-scope" translate="no">
                <ScopeGlyph state=state conversation=conversation.clone() />
                <span class="search-field-scope-name" aria-hidden="true">{conversation.name.clone()}</span>
                <button
                  type="button"
                  class="search-field-scope-clear"
                  aria-label=move || i18n.t("search-scope-clear")
                  title=move || i18n.t("search-scope-clear")
                  on:click=move |ev| {
                    ev.stop_propagation();
                    maybe_dismissed.set(Some(id));
                  }
                >
                  <Icon name=IconName::Close />
                </button>
              </span>
            }
          })
      }}
      <button
        class="search-btn"
        aria-label=label
        on:click=move |ev| {
          ev.stop_propagation();
          open();
        }
      >
        <Icon name=IconName::Search />
        <span class="search-btn-label" aria-hidden="true">{move || i18n.t("search-placeholder")}</span>
        <ShortcutKeys shortcut=QUICK_SWITCHER />
      </button>
    </div>
  }
}

/// What stands for the scope's conversation in its chip: the other person's avatar in a DM, the
/// channel's picture or glyph otherwise.
#[component]
fn ScopeGlyph(state: AppState, conversation: Conversation) -> impl IntoView {
  match conversation.kind {
    ConversationKind::Direct => {
      let me = state.me().id;
      let maybe_other = conversation
        .member_ids
        .iter()
        .copied()
        .find(|member| *member != me)
        .and_then(|member| state.read(|backend| backend.user(member)));
      match maybe_other {
        Some(user) => view! { <Avatar user=user size=AvatarSize::Small /> }.into_any(),
        None => view! { <Icon name=IconName::User /> }.into_any(),
      }
    },
    ConversationKind::Group => view! { <Icon name=IconName::Users /> }.into_any(),
    ConversationKind::Integration => view! { <Icon name=IconName::Terminal /> }.into_any(),
    ConversationKind::Public | ConversationKind::Private => {
      let fallback = if conversation.kind == ConversationKind::Public {
        IconName::Hash
      } else {
        IconName::Lock
      };
      view! {
        <ChannelGlyph conversation=conversation.id maybe_version=conversation.maybe_image_version.clone() fallback=fallback />
      }
      .into_any()
    },
  }
}
