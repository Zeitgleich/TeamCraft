use leptos::prelude::*;

use crate::areas::{self, Place, lazy_area};
use crate::auth::tauri;
use crate::brand::Brand;
use crate::components::account_switcher::OtherAccountsBadge;
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::brand_logo::BrandLogo;
use crate::components::channel_glyph::{ChannelGlyph, channel_fallback_icon};
use crate::components::icons::{Icon, IconName};
use crate::components::rail_menus::FocusMenu;
use crate::components::window_controls::{ClickOrDrag, window_drag_handler, window_menu_handler};
use crate::history::{Travel, travel};
use crate::i18n::use_i18n;
use crate::keys::{QUICK_SWITCHER, ShortcutKeys};
use crate::model::{Conversation, ConversationId, ConversationKind, UserId};
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
  let on_menu = window_menu_handler();
  let brand = Brand::use_current().0;
  let press = ClickOrDrag::new();
  // The browser has its own buttons; the desktop webview has none (#874).
  let has_history_buttons = tauri::is_available();

  view! {
    <header class="top-bar" data-window-drag="" on:mousedown=on_drag on:contextmenu=on_menu>
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
      // Back and forward sit left of the search, which stays where it was (#874).
      <div class="top-nav" class:has-history=has_history_buttons>
        {has_history_buttons.then(|| view! { <HistoryButtons /> })}
        <SearchField />
      </div>
      <div class="top-actions">
        // The browser has its own downloads list and never loads this one.
        {tauri::is_available().then(|| lazy_area(Place::Inline, || areas::downloads(())))}
        <button
          class="icon-btn on-dark"
          aria-label=move || i18n.t("shortcuts-title")
          title=move || i18n.t("shortcuts-title")
          on:click=move |_| state.overlay.set(Overlay::Shortcuts)
        >
          <Icon name=IconName::Keyboard />
        </button>
        // Focus mode next to the account menu (#299).
        <FocusMenu />
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
          // Something unread in another account on this computer (#214).
          <Show when=tauri::is_available fallback=|| ()>
            <OtherAccountsBadge />
          </Show>
          // On phones the menu opens from the bottom navigation instead (app.rs).
          <Show when=move || menu_open() && !state.is_compact() fallback=|| ()>
            {lazy_area(Place::Overlay, || areas::status_menu(()))}
          </Show>
        </div>
      </div>
    </header>
  }
}

/// Back and forward through the places the user has been, as in the browser: disabled when there
/// is nothing to go back or forward to. The same steps as Alt+←/→, ⌘[ / ⌘] and the mouse's back and
/// forward buttons (`history.rs`).
#[component]
fn HistoryButtons() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let reach = state.history_reach;
  view! {
    <div class="history-buttons">
      <button
        class="icon-btn on-dark"
        aria-label=move || i18n.t("history-back")
        title=move || i18n.t("history-back")
        prop:disabled=move || !reach.get().can_go_back()
        on:click=move |_| travel(Travel::Back)
      >
        <Icon name=IconName::ArrowLeft />
      </button>
      <button
        class="icon-btn on-dark"
        aria-label=move || i18n.t("history-forward")
        title=move || i18n.t("history-forward")
        prop:disabled=move || !reach.get().can_go_forward()
        on:click=move |_| travel(Travel::Forward)
      >
        <Icon name=IconName::ArrowRight />
      </button>
    </div>
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
  // Only what the chip shows: a new message changes the conversation's counts, not its chip, and
  // a chip built again loads its picture again (flicker in a busy conversation).
  let maybe_scope = Memo::new(move |_| {
    let id = state.current_conversation_id()?;
    if maybe_dismissed.get() == Some(id) {
      return None;
    }
    let me = state.me().id;
    state.read(|backend| {
      backend
        .conversation(id)
        .map(|conversation| ScopeChip::of(&conversation, me))
    })
  });
  let maybe_scope_id = Memo::new(move |_| maybe_scope.with(|maybe| maybe.as_ref().map(|scope| scope.id)));
  let open = move || {
    if maybe_scope.get_untracked().is_some() {
      state.open_scoped_switcher();
    } else {
      state.open_switcher("");
    }
  };
  let label = move || match maybe_scope.get() {
    Some(scope) => i18n.t_with("search-in", &[("name", &scope.name)]),
    None => i18n.t("search-placeholder"),
  };

  view! {
    // A press on the icon or the chip opens the search too; the button is the control.
    <div class="search-field" class:is-scoped=move || maybe_scope_id.with(Option::is_some) on:click=move |_| open()>
      <span class="search-field-icon" aria-hidden="true">
        <Icon name=IconName::Search />
      </span>
      // Built once per conversation; its name and picture follow in place.
      {move || {
        maybe_scope_id.get().map(|id| {
          let name = move || maybe_scope.with(|maybe| maybe.as_ref().map(|scope| scope.name.clone()).unwrap_or_default());
          view! {
            <span class="search-field-scope" translate="no">
              <ScopeGlyph state=state scope=maybe_scope />
              <span class="search-field-scope-name" aria-hidden="true">{name}</span>
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
fn ScopeGlyph(state: AppState, scope: Memo<Option<ScopeChip>>) -> impl IntoView {
  // The chip is built again for another conversation; its kind and person stay.
  let Some(first) = scope.get_untracked() else {
    return ().into_any();
  };
  match first.kind {
    ConversationKind::Direct => match first.maybe_peer {
      Some(peer) => {
        // Read through a memo: other backend changes don't build the avatar again.
        let user = Memo::new(move |_| state.read(|backend| backend.user(peer)));
        (move || {
          user
            .get()
            .map(|user| view! { <Avatar user=user size=AvatarSize::Small /> })
        })
        .into_any()
      },
      None => view! { <Icon name=IconName::User /> }.into_any(),
    },
    ConversationKind::Group => view! { <Icon name=IconName::Users /> }.into_any(),
    ConversationKind::Public | ConversationKind::Private | ConversationKind::Integration => {
      let fallback = channel_fallback_icon(first.kind).unwrap_or(IconName::Hash);
      let version = Signal::derive(move || {
        scope.with(|maybe| maybe.as_ref().and_then(|scope| scope.maybe_image_version.clone()))
      });
      view! { <ChannelGlyph conversation=first.id maybe_version=version fallback=fallback /> }.into_any()
    },
  }
}

/// What the search field's chip shows of the conversation it searches in.
#[derive(Clone, Debug, PartialEq)]
struct ScopeChip {
  id: ConversationId,
  kind: ConversationKind,
  name: String,
  maybe_image_version: Option<String>,
  /// The other person of a direct conversation.
  maybe_peer: Option<UserId>,
}

impl ScopeChip {
  fn of(conversation: &Conversation, me: UserId) -> Self {
    let maybe_peer = (conversation.kind == ConversationKind::Direct)
      .then(|| {
        conversation
          .member_ids
          .iter()
          .copied()
          .find(|member| *member != me)
      })
      .flatten();
    Self {
      id: conversation.id,
      kind: conversation.kind,
      name: conversation.name.clone(),
      maybe_image_version: conversation.maybe_image_version.clone(),
      maybe_peer,
    }
  }
}
