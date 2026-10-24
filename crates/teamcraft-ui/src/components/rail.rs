use leptos::prelude::*;

use crate::components::count_badge::CountBadge;
use crate::components::icons::{Icon, IconName};
use crate::components::space_tile::SpaceTileContent;
use crate::i18n::use_i18n;
use crate::model::Space;
use crate::sections::{RailBadge, badge_count};
use crate::state::{MainView, Overlay, RailEntry, use_app_state};

/// Space switcher on the far left (Mattermost "team sidebar"). On top the global destinations with
/// a label under each icon: Home (the DMs, which live outside spaces), Activity and Threads. In the
/// grouped sidebar one entry stands for all spaces.
#[component]
pub fn SpaceRail() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let spaces = Memo::new(move |_| state.read(|backend| backend.spaces()));
  let is_grouped = Memo::new(move |_| {
    state
      .settings
      .prefs
      .with(|prefs| prefs.sidebar.is_grouped_by_space)
  });

  view! {
    <nav class="space-rail" aria-label=move || i18n.t("rail-label")>
      <div class="rail-destinations">
        <HomeButton />
        <ActivityButton />
        <ThreadsButton />
      </div>
      <span class="rail-divider" aria-hidden="true"></span>
      <ul>
        {move || {
          if is_grouped.get() {
            view! { <AllSpacesButton /> }.into_any()
          } else {
            view! {
              // A new name, color or picture renders the button again.
              <For
                each=move || spaces.get()
                key=|space| (space.id, space.name.clone(), space.hue, space.maybe_image_version.clone())
                let:space
              >
                <SpaceButton space=space />
              </For>
            }
              .into_any()
          }
        }}
      </ul>
      <button class="rail-add" on:click=move |_| {
        // Joining an open space is the common case for everyone but the first person.
        let has_joinable = state.read(|backend| !backend.joinable_spaces().is_empty());
        state.overlay.set(if has_joinable { Overlay::JoinSpace } else { Overlay::NewSpace });
      } aria-label=move || i18n.t("rail-add-space") title=move || i18n.t("rail-add-space")>
        <Icon name=IconName::Plus />
      </button>
    </nav>
  }
}

/// Home (the DMs), with the number of unread DMs and mentions in them.
#[component]
fn HomeButton() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let count = Memo::new(move |_| {
    let list = state.with_mutes(state.read(|backend| backend.direct_conversations()));
    badge_count(&list)
  });
  let is_current =
    Signal::derive(move || state.rail.get() == RailEntry::Home && !is_destination_view(state.view.get()));
  let label = Signal::derive(move || match count.get() {
    0 => i18n.t("home-title"),
    unread => i18n.t_with("rail-home-unread", &[("count", &unread.to_string())]),
  });
  view! {
    <RailDestination
      icon=IconName::Home
      label_key="rail-home"
      accessible_label=label
      is_current=is_current
      count=Signal::derive(move || count.get())
      on_open=Callback::new(move |()| state.open_home())
    />
  }
}

/// Mentions, reactions and thread replies addressed to the user.
#[component]
fn ActivityButton() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let count = Memo::new(move |_| state.read(|backend| backend.activity().unread_count));
  let label = Signal::derive(move || match count.get() {
    0 => i18n.t("sidebar-activity"),
    unread => i18n.t_with("rail-activity-unread", &[("count", &unread.to_string())]),
  });
  view! {
    <RailDestination
      icon=IconName::Bell
      label_key="rail-activity"
      accessible_label=label
      is_current=Signal::derive(move || state.view.get() == MainView::Activity)
      count=Signal::derive(move || count.get())
      on_open=Callback::new(move |()| state.open_activity())
    />
  }
}

/// The threads the user follows, with the number that have unread replies.
#[component]
fn ThreadsButton() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let count = Memo::new(move |_| {
    state.read(|backend| {
      backend
        .followed_threads()
        .iter()
        .filter(|thread| thread.unread_replies > 0)
        .count() as u32
    })
  });
  let label = Signal::derive(move || match count.get() {
    0 => i18n.t("sidebar-threads"),
    unread => i18n.t_with("rail-threads-unread", &[("count", &unread.to_string())]),
  });
  view! {
    <RailDestination
      icon=IconName::Threads
      label_key="rail-threads"
      accessible_label=label
      is_current=Signal::derive(move || state.view.get() == MainView::Threads)
      count=Signal::derive(move || count.get())
      on_open=Callback::new(move |()| state.open_threads())
    />
  }
}

/// One labelled destination: icon with a count, the short label under it.
#[component]
fn RailDestination(
  icon: IconName,
  label_key: &'static str,
  /// The full label for screen readers and the tooltip, with the count in words.
  accessible_label: Signal<String>,
  is_current: Signal<bool>,
  count: Signal<u32>,
  on_open: Callback<()>,
) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <button
      class="rail-dest"
      class:current=move || is_current.get()
      aria-label=move || accessible_label.get()
      title=move || accessible_label.get()
      aria-current=move || is_current.get().then_some("page")
      on:click=move |_| on_open.run(())
    >
      <span class="rail-dest-icon">
        <Icon name=icon />
        <CountBadge count=count is_decorative=true />
      </span>
      <span class="rail-dest-label" aria-hidden="true">{move || i18n.t(label_key)}</span>
    </button>
  }
}

/// Views that have their own rail destination: while one is open, Home isn't the current one.
fn is_destination_view(view: MainView) -> bool {
  matches!(view, MainView::Activity | MainView::Threads)
}

/// In the grouped sidebar: back from Home to the channels of every space.
#[component]
fn AllSpacesButton() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let badge = Memo::new(move |_| {
    let list = state.read(|backend| {
      backend
        .spaces()
        .into_iter()
        .flat_map(|space| backend.space_conversations(space.id))
        .collect::<Vec<_>>()
    });
    RailBadge::of(&state.with_mutes(list))
  });
  let is_current = move || state.rail.get() == RailEntry::Space && !is_destination_view(state.view.get());
  view! {
    <li>
      <button
        class="space-btn"
        class:current=is_current
        aria-label=move || i18n.t("sidebar-all-spaces")
        title=move || i18n.t("sidebar-all-spaces")
        aria-current=move || is_current().then_some("true")
        on:click=move |_| state.open_space(state.space.get_untracked())
      >
        <Icon name=IconName::Grid />
        <RailBadgeView badge=badge />
      </button>
    </li>
  }
}

/// Mentions as a number, other unread messages as a dot.
#[component]
fn RailBadgeView(badge: Memo<RailBadge>) -> impl IntoView {
  view! {
    <CountBadge count=Signal::derive(move || badge.get().mention_count) is_decorative=true />
    <Show when=move || { let current = badge.get(); current.mention_count == 0 && current.has_unread } fallback=|| ()>
      <span class="dot-unread" aria-hidden="true"></span>
    </Show>
  }
}

#[component]
fn SpaceButton(space: Space) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let id = space.id;
  // Only the space's own channels count; DMs have their own entry.
  let badge = Memo::new(move |_| {
    RailBadge::of(&state.with_mutes(state.read(|backend| backend.space_conversations(id))))
  });
  let is_current = move || {
    state.rail.get() == RailEntry::Space && state.space.get() == id && !is_destination_view(state.view.get())
  };
  let is_muted = move || {
    state
      .settings
      .prefs
      .with(|prefs| prefs.notifications.is_space_muted(&id.to_string()))
  };
  let label = {
    let name = space.name.clone();
    move || {
      let current = badge.get();
      if current.mention_count > 0 {
        i18n.t_with(
          "rail-space-mentions",
          &[("name", &name), ("count", &current.mention_count.to_string())],
        )
      } else if current.has_unread {
        i18n.t_with("rail-space-unread", &[("name", &name)])
      } else {
        name.clone()
      }
    }
  };

  view! {
    <li>
      <button
        class="space-btn"
        class:current=is_current
        class:muted=is_muted
        style:--hue=space.hue.to_string()
        aria-label=label.clone()
        title=label
        aria-current=move || is_current().then_some("true")
        on:click=move |_| state.open_space(id)
      >
        <SpaceTileContent space=space.clone() />
        <RailBadgeView badge=badge />
      </button>
    </li>
  }
}
