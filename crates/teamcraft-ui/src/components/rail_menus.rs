//! The shell's two quick menus (issue #299): "create" under the rail's destinations (a new message,
//! a meeting, a channel, an invitation) and focus mode beside the account menu (pause
//! notifications for a while, the same do-not-disturb the account menu sets).

use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::channel_access::{can_create_channel_in, can_invite_to, can_start_any_direct, is_guest};
use crate::clock::{now_ms, tomorrow_at};
use crate::components::icons::{Icon, IconName};
use crate::components::sheet::MenuSurface;
use crate::components::status_menu::{FOCUS_SHORT_MS, MORNING_MINUTE, MS_PER_HOUR, paused_until_label};
use crate::i18n::use_i18n;
use crate::meetings::MeetingsHub;
use crate::model::{PresenceStatus, TimestampMs};
use crate::state::{AppState, Overlay, use_app_state};

/// A rail button with a menu beside it. `open` is shared so the menu can close itself.
#[derive(Clone, Copy)]
struct MenuAnchor {
  button: NodeRef<html::Button>,
  is_open: RwSignal<bool>,
}

impl MenuAnchor {
  fn new() -> Self {
    Self {
      button: NodeRef::new(),
      is_open: RwSignal::new(false),
    }
  }

  fn toggle(self) {
    // The menu is the button's sibling, so a variable on their parent reaches it.
    if let Some(button) = self.button.get_untracked()
      && let Some(parent) = button
        .parent_element()
        .and_then(|parent| parent.dyn_into::<web_sys::HtmlElement>().ok())
    {
      // Beside the rail the menu starts level with the button; under the top bar (Focus) it hangs
      // from the button's bottom edge, its right edge in line with the button's.
      let rect = button.get_bounding_client_rect();
      let window_width = web_sys::window()
        .and_then(|window| window.inner_width().ok())
        .and_then(|width| width.as_f64())
        .unwrap_or(rect.right());
      let style = parent.style();
      drop(style.set_property("--rail-menu-top", &format!("{}px", rect.top())));
      drop(style.set_property("--rail-menu-below", &format!("{}px", rect.bottom())));
      drop(style.set_property("--rail-menu-right", &format!("{}px", window_width - rect.right())));
    }
    self.is_open.update(|is_open| *is_open = !*is_open);
  }

  fn close(self) {
    self.is_open.set(false);
    if let Some(button) = self.button.get_untracked() {
      drop(button.focus());
    }
  }
}

/// "+": new message, meeting, channel and invitation in one place, each for people who may.
#[component]
pub fn RailCreateMenu() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let anchor = MenuAnchor::new();
  let has_meetings = use_context::<MeetingsHub>().is_some();
  let space = move || state.space.get_untracked();
  let can_create_channel = Memo::new(move |_| can_create_channel_in(state, state.space.get()));
  let can_invite = Memo::new(move |_| can_invite_to(state, state.space.get()));
  let item = move |icon: IconName, key: &'static str, open: Callback<()>| {
    view! {
      <button
        class="menu-item"
        role="menuitem"
        on:click=move |_| {
          anchor.is_open.set(false);
          open.run(());
        }
      >
        <Icon name=icon />
        <span class="menu-label">{move || i18n.t(key)}</span>
      </button>
    }
  };
  // A guest who may start nothing gets no "+": an empty menu would be a button that does nothing.
  let has_entries = Memo::new(move |_| {
    can_start_any_direct(state)
      || (has_meetings && !is_guest(state))
      || can_create_channel.get()
      || can_invite.get()
  });
  view! {
    <Show when=move || has_entries.get() fallback=|| ()>
    <button
      class="rail-dest rail-create"
      node_ref=anchor.button
      aria-haspopup="menu"
      aria-expanded=move || anchor.is_open.get().to_string()
      aria-label=move || i18n.t("rail-create")
      title=move || i18n.t("rail-create")
      on:click=move |_| anchor.toggle()
    >
      <span class="rail-dest-icon"><Icon name=IconName::Plus /></span>
      <span class="rail-dest-label" aria-hidden="true">{move || i18n.t("rail-create-short")}</span>
    </button>
    <Show when=move || anchor.is_open.get() fallback=|| ()>
      <MenuSurface
        class="rail-menu"
        label=Signal::derive(move || i18n.t("rail-create"))
        on_close=Callback::new(move |()| anchor.close())
        on_keydown=Callback::new(move |ev: web_sys::KeyboardEvent| {
          if ev.key() == "Escape" {
            ev.stop_propagation();
            anchor.close();
          }
        })
      >
        <div class="rail-menu-body">
          <Show when=move || can_start_any_direct(state) fallback=|| ()>
            {item(IconName::Compose, "dm-new", Callback::new(move |()| state.overlay.set(Overlay::NewMessage)))}
          </Show>
          {(has_meetings && !is_guest(state))
            .then(|| item(IconName::Calendar, "meetings-schedule", Callback::new(move |()| state.overlay.set(Overlay::ScheduleMeeting))))}
          <Show when=move || can_create_channel.get() fallback=|| ()>
            {item(IconName::Hash, "channel-new-title", Callback::new(move |()| state.overlay.set(Overlay::NewChannel(space()))))}
          </Show>
          <Show when=move || can_invite.get() fallback=|| ()>
            {item(IconName::UserPlus, "invite-title", Callback::new(move |()| state.overlay.set(Overlay::InvitePeople(space()))))}
          </Show>
        </div>
      </MenuSurface>
    </Show>
    </Show>
  }
}

/// Focus mode beside the account menu: pause notifications for 30 minutes, an hour or until
/// tomorrow morning; pressing the button while they are paused resumes them. It is the same
/// do-not-disturb the account menu and the tray set (`AppState::set_presence_until`), not a mode of
/// its own. Whatever else focus mode should change (a darker look, say) reads [`is_focus_mode`].
#[component]
pub fn FocusMenu() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let anchor = MenuAnchor::new();
  let is_focused = Memo::new(move |_| is_focus_mode(state));
  let set_focus = move |maybe_until: Option<TimestampMs>| {
    let presence = if maybe_until.is_some() {
      PresenceStatus::DoNotDisturb
    } else {
      PresenceStatus::Online
    };
    state.set_presence_until(presence, maybe_until);
    anchor.is_open.set(false);
  };
  let duration_item = move |key: &'static str, until: Callback<(), TimestampMs>| {
    view! {
      <button class="menu-item" role="menuitem" on:click=move |_| set_focus(Some(until.run(())))>
        <span class="menu-label">{move || i18n.t(key)}</span>
      </button>
    }
  };
  // While paused, the button says until when and that a press resumes.
  let label = move || {
    if is_focused.get() {
      match paused_until_label(state, i18n) {
        Some(until) => format!("{} ({until})", i18n.t("tray-resume-notifications")),
        None => i18n.t("tray-resume-notifications"),
      }
    } else {
      i18n.t("rail-focus")
    }
  };
  view! {
    <div class="focus-menu">
      <button
        class="icon-btn on-dark focus-btn"
        class:is-on=move || is_focused.get()
        node_ref=anchor.button
        aria-haspopup=move || (!is_focused.get()).then_some("menu")
        aria-expanded=move || (!is_focused.get()).then(|| anchor.is_open.get().to_string())
        aria-pressed=move || is_focused.get().to_string()
        aria-label=label
        title=label
        on:click=move |_| {
          if is_focused.get_untracked() {
            set_focus(None);
          } else {
            anchor.toggle();
          }
        }
      >
        <Icon name=IconName::BellOff />
      </button>
      <Show when=move || anchor.is_open.get() fallback=|| ()>
        <MenuSurface
          class="rail-menu opens-below"
          label=Signal::derive(move || i18n.t("rail-focus"))
          on_close=Callback::new(move |()| anchor.close())
          on_keydown=Callback::new(move |ev: web_sys::KeyboardEvent| {
            if ev.key() == "Escape" {
              ev.stop_propagation();
              anchor.close();
            }
          })
        >
          <div class="rail-menu-body">
            {duration_item("focus-30-minutes", Callback::new(|()| now_ms() + FOCUS_SHORT_MS))}
            {duration_item("presence-dnd-for-hour", Callback::new(|()| now_ms() + MS_PER_HOUR))}
            {duration_item("presence-dnd-until-morning", Callback::new(|()| tomorrow_at(now_ms(), MORNING_MINUTE)))}
          </div>
        </MenuSurface>
      </Show>
    </div>
  }
}

/// Whether focus mode is on: notifications are paused (do not disturb). The one place to ask, for
/// anything that should follow focus mode.
pub fn is_focus_mode(state: AppState) -> bool {
  state.me().presence == PresenceStatus::DoNotDisturb
}
