use leptos::prelude::*;

use crate::components::icons::{Icon, IconName};
use crate::components::space_tile::SpaceTileContent;
use crate::i18n::use_i18n;
use crate::model::Space;
use crate::sections::{RailBadge, badge_count};
use crate::state::{Overlay, RailEntry, use_app_state};

/// Space switcher on the far left (Mattermost "team sidebar"), with the direct messages on top
/// (they live outside spaces). In the grouped sidebar one entry stands for all spaces.
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
      <HomeButton />
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
  let is_current = move || state.rail.get() == RailEntry::Home;
  let label = move || match count.get() {
    0 => i18n.t("home-title"),
    unread => i18n.t_with("rail-home-unread", &[("count", &unread.to_string())]),
  };
  view! {
    <button
      class="space-btn rail-dm"
      class:current=is_current
      aria-label=label
      title=label
      aria-current=move || is_current().then_some("true")
      on:click=move |_| state.open_home()
    >
      <Icon name=IconName::Home />
      {move || {
        let unread = count.get();
        (unread > 0).then(|| view! { <span class="badge-count" aria-hidden="true">{unread}</span> })
      }}
    </button>
  }
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
  let is_current = move || state.rail.get() == RailEntry::Space;
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
  move || {
    let current = badge.get();
    if current.mention_count > 0 {
      view! { <span class="badge-count" aria-hidden="true">{current.mention_count}</span> }.into_any()
    } else if current.has_unread {
      view! { <span class="dot-unread" aria-hidden="true"></span> }.into_any()
    } else {
      ().into_any()
    }
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
  let is_current = move || state.rail.get() == RailEntry::Space && state.space.get() == id;
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
