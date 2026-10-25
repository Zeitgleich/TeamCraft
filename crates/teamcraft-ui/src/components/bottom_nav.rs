//! The bottom navigation of the compact layout (phones): Home, Activity, Spaces and You, as in the
//! Android app. It shows on the top-level pages; a conversation, thread or other page has a back
//! button instead.

use leptos::prelude::*;

use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::count_badge::{BadgeTone, CountBadge};
use crate::components::icons::{Icon, IconName};
use crate::components::rail::activity_tone;
use crate::i18n::use_i18n;
use crate::sections::badge_count;
use crate::state::{AppState, MainView, Overlay, RailEntry, use_app_state};
use crate::viewport::CompactPane;

#[component]
pub fn BottomNav() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let me = Memo::new(move |_| state.me());
  let home_count = Memo::new(move |_| {
    let list = state.with_mutes(state.read(|backend| backend.direct_conversations()));
    badge_count(&list)
  });
  let activity_count = Memo::new(move |_| state.read(|backend| backend.activity().unread_count));
  let spaces_count = Memo::new(move |_| {
    let list = state.with_mutes(state.read(|backend| {
      backend
        .spaces()
        .into_iter()
        .flat_map(|space| backend.space_conversations(space.id))
        .collect()
    }));
    badge_count(&list)
  });
  let is_list = move || state.compact_pane() == CompactPane::List;
  let is_home = Signal::derive(move || {
    state.overlay.get() != Overlay::StatusMenu
      && state.rail.get() == RailEntry::Home
      && (is_list() || state.view.get() == MainView::GettingStarted)
  });
  let is_activity = Signal::derive(move || {
    state.overlay.get() != Overlay::StatusMenu && state.view.get() == MainView::Activity
  });
  let is_spaces = Signal::derive(move || {
    state.overlay.get() != Overlay::StatusMenu && state.rail.get() == RailEntry::Space && is_list()
  });
  let is_you = Signal::derive(move || state.overlay.get() == Overlay::StatusMenu);

  view! {
    <nav class="bottom-nav" aria-label=move || i18n.t("bottom-nav-label")>
      <NavTab
        label_key="rail-home"
        is_current=is_home
        count=Signal::derive(move || home_count.get())
        on_open=Callback::new(move |()| {
          state.close_overlay();
          state.open_home();
        })
      >
        <Icon name=IconName::Home />
      </NavTab>
      <NavTab
        label_key="rail-activity"
        is_current=is_activity
        count=Signal::derive(move || activity_count.get())
        tone=activity_tone(state)
        on_open=Callback::new(move |()| state.open_activity())
      >
        <Icon name=IconName::Bell />
      </NavTab>
      <NavTab
        label_key="bottom-nav-spaces"
        is_current=is_spaces
        count=Signal::derive(move || spaces_count.get())
        on_open=Callback::new(move |()| open_spaces(state))
      >
        <Icon name=IconName::Layers />
      </NavTab>
      <NavTab
        label_key="bottom-nav-you"
        is_current=is_you
        count=Signal::derive(|| 0)
        on_open=Callback::new(move |()| {
          state.overlay.update(|overlay| {
            *overlay = if *overlay == Overlay::StatusMenu { Overlay::None } else { Overlay::StatusMenu };
          });
        })
      >
        {move || view! { <Avatar user=me.get() size=AvatarSize::Small show_presence=true /> }}
      </NavTab>
    </nav>
  }
}

/// The channel list of the space last open; without any space, joining or creating one.
fn open_spaces(state: AppState) {
  state.close_overlay();
  let space = state.space.get_untracked();
  let maybe_space = state.read(|backend| {
    let spaces = backend.spaces();
    spaces
      .iter()
      .find(|candidate| candidate.id == space)
      .or_else(|| spaces.first())
      .map(|space| space.id)
  });
  match maybe_space {
    Some(space) => state.open_space(space),
    // Browsing lists the open spaces to join and has the button to create a new one.
    None => state.overlay.set(Overlay::BrowseSpaces(None)),
  }
}

/// One tab: icon (or avatar) with a count, the label under it.
#[component]
fn NavTab(
  label_key: &'static str,
  is_current: Signal<bool>,
  count: Signal<u32>,
  #[prop(into, optional)] tone: Signal<BadgeTone>,
  on_open: Callback<()>,
  children: Children,
) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <button
      type="button"
      class="bottom-nav-tab"
      class:current=move || is_current.get()
      aria-current=move || is_current.get().then_some("page")
      on:click=move |_| on_open.run(())
    >
      <span class="bottom-nav-icon">
        {children()}
        <CountBadge count=count tone=tone />
      </span>
      <span class="bottom-nav-label">{move || i18n.t(label_key)}</span>
    </button>
  }
}
