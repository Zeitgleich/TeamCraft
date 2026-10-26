//! The two menus at the foot of the rail's destinations (issue #299): "create" (a new message, a
//! meeting, a channel, an invitation) and focus mode (pause notifications for a while, the same
//! do-not-disturb the account menu sets).

use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::clock::{now_ms, tomorrow_at};
use crate::components::icons::{Icon, IconName};
use crate::components::sheet::MenuSurface;
use crate::components::status_menu::{MORNING_MINUTE, MS_PER_HOUR};
use crate::i18n::use_i18n;
use crate::meetings::MeetingsHub;
use crate::model::{PresenceStatus, TimestampMs};
use crate::state::{Overlay, use_app_state};

const MS_PER_MINUTE: TimestampMs = 60_000;
const FOCUS_SHORT_MINUTES: TimestampMs = 30;

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
      // A menu that opens up (Focus, at the rail's foot) hangs from the button's bottom edge.
      let rect = button.get_bounding_client_rect();
      let window_height = web_sys::window()
        .and_then(|window| window.inner_height().ok())
        .and_then(|height| height.as_f64())
        .unwrap_or(rect.bottom());
      let style = parent.style();
      drop(style.set_property("--rail-menu-top", &format!("{}px", rect.top())));
      drop(style.set_property(
        "--rail-menu-bottom",
        &format!("{}px", window_height - rect.bottom()),
      ));
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

/// "+": new message, meeting, channel and invitation in one place.
#[component]
pub fn RailCreateMenu() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let anchor = MenuAnchor::new();
  let has_meetings = use_context::<MeetingsHub>().is_some();
  let space = move || state.space.get_untracked();
  let can_manage = Memo::new(move |_| {
    state.read(|backend| backend.my_role()) != teamcraft_proto::Role::Guest
      && state.read(|backend| !backend.spaces().is_empty())
  });
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
  view! {
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
          {item(IconName::Compose, "dm-new", Callback::new(move |()| state.overlay.set(Overlay::NewMessage)))}
          {has_meetings
            .then(|| item(IconName::Calendar, "meetings-schedule", Callback::new(move |()| state.overlay.set(Overlay::ScheduleMeeting))))}
          <Show when=move || can_manage.get() fallback=|| ()>
            {item(IconName::Hash, "channel-new-title", Callback::new(move |()| state.overlay.set(Overlay::NewChannel(space()))))}
            {item(IconName::UserPlus, "invite-title", Callback::new(move |()| state.overlay.set(Overlay::InvitePeople(space()))))}
          </Show>
        </div>
      </MenuSurface>
    </Show>
  }
}

/// Focus mode: do not disturb for 30 minutes, an hour or until tomorrow morning; pressing the
/// button while it is on ends it.
#[component]
pub fn RailFocusMenu() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let anchor = MenuAnchor::new();
  let is_focused = Memo::new(move |_| state.me().presence == PresenceStatus::DoNotDisturb);
  let set_focus = move |maybe_until: Option<TimestampMs>| {
    let presence = if maybe_until.is_some() {
      PresenceStatus::DoNotDisturb
    } else {
      PresenceStatus::Online
    };
    state.write(|backend| backend.set_presence_until(presence, maybe_until));
    state
      .settings
      .prefs
      .update(|prefs| prefs.notifications.maybe_dnd_until = maybe_until);
    anchor.is_open.set(false);
  };
  let duration_item = move |key: &'static str, until: Callback<(), TimestampMs>| {
    view! {
      <button class="menu-item" role="menuitem" on:click=move |_| set_focus(Some(until.run(())))>
        <span class="menu-label">{move || i18n.t(key)}</span>
      </button>
    }
  };
  view! {
    <button
      class="rail-dest rail-focus"
      class:current=move || is_focused.get()
      node_ref=anchor.button
      aria-haspopup="menu"
      aria-expanded=move || anchor.is_open.get().to_string()
      aria-pressed=move || is_focused.get().to_string()
      aria-label=move || i18n.t("rail-focus")
      title=move || i18n.t("rail-focus")
      on:click=move |_| {
        if is_focused.get_untracked() {
          set_focus(None);
        } else {
          anchor.toggle();
        }
      }
    >
      <span class="rail-dest-icon"><Icon name=IconName::BellOff /></span>
      <span class="rail-dest-label" aria-hidden="true">{move || i18n.t("rail-focus-short")}</span>
    </button>
    <Show when=move || anchor.is_open.get() fallback=|| ()>
      <MenuSurface
        class="rail-menu opens-up"
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
          {duration_item("focus-30-minutes", Callback::new(|()| now_ms() + FOCUS_SHORT_MINUTES * MS_PER_MINUTE))}
          {duration_item("presence-dnd-for-hour", Callback::new(|()| now_ms() + MS_PER_HOUR))}
          {duration_item("presence-dnd-until-morning", Callback::new(|()| tomorrow_at(now_ms(), MORNING_MINUTE)))}
        </div>
      </MenuSurface>
    </Show>
  }
}
