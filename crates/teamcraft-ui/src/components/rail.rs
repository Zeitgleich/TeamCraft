use leptos::ev;
use leptos::html;
use leptos::prelude::*;

use crate::channel_access::is_guest;
use crate::components::count_badge::{BadgeTone, CountBadge};
use crate::components::icons::{Icon, IconName};
use crate::components::rail_menus::{RailCreateMenu, RailFocusMenu};
use crate::components::space_tile::SpaceTileContent;
use crate::i18n::use_i18n;
use crate::model::{ConversationKind, Space};
use crate::sections::{RailBadge, badge_count, has_urgent_activity};
use crate::state::{AppState, MainView, Overlay, RailEntry, use_app_state};

/// The largest count a space tile's badge shows; more reads "9+".
const RAIL_BADGE_CAP: u32 = 9;

/// Space switcher on the far left (Mattermost "team sidebar"). On top the global destinations with
/// a label under each icon: Home (the DMs, which live outside spaces), Activity and Threads. In the
/// grouped sidebar one entry stands for all spaces.
///
/// Only the spaces (with "add space" at their end) scroll; the destinations above and Focus below
/// stay in place (#652). Where spaces are scrolled out of sight, the scroller's edge fades.
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

  let scroller = NodeRef::<html::Div>::new();
  // The edges depend on the content and the rail's height as well as on scrolling.
  Effect::new(move |_| {
    spaces.track();
    is_grouped.track();
    if let Some(element) = scroller.get() {
      request_animation_frame(move || mark_hidden_edges(&element));
    }
  });
  let on_resize = window_event_listener(ev::resize, move |_| {
    if let Some(element) = scroller.get_untracked() {
      mark_hidden_edges(&element);
    }
  });
  on_cleanup(move || on_resize.remove());

  view! {
    <nav class="space-rail" aria-label=move || i18n.t("rail-label")>
      <div class="rail-destinations">
        <ChatsButton />
        <ActivityButton />
        <ThreadsButton />
        <RailCreateMenu />
      </div>
      <span class="rail-divider" aria-hidden="true"></span>
      <div class="rail-spaces" node_ref=scroller on:scroll=move |_| {
        if let Some(element) = scroller.get_untracked() {
          mark_hidden_edges(&element);
        }
      }>
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
        // Browsing lists the open spaces to join and has the button to create a new one. Guests
        // join no spaces themselves: they are invited (admin.md §1).
        <Show when=move || !is_guest(state) fallback=|| ()>
          <button class="rail-add" on:click=move |_| state.overlay.set(Overlay::BrowseSpaces(None)) aria-label=move || i18n.t("rail-add-space") title=move || i18n.t("rail-add-space")>
            <Icon name=IconName::Plus />
          </button>
        </Show>
      </div>
      <div class="rail-bottom">
        <RailFocusMenu />
      </div>
    </nav>
  }
}

/// Marks the sides of the spaces' scroller with content out of sight (`.has-more-above`,
/// `.has-more-below`), where the stylesheet fades the edge.
fn mark_hidden_edges(element: &web_sys::HtmlDivElement) {
  // A pixel of slack: fractional scroll positions never quite reach the end.
  let top = element.scroll_top();
  let has_more_below = top + element.client_height() < element.scroll_height() - 1;
  let classes = element.class_list();
  // Toggling a class on a live element doesn't fail.
  drop(classes.toggle_with_force("has-more-above", top > 1));
  drop(classes.toggle_with_force("has-more-below", has_more_below));
}

/// Chats (the DMs and group DMs), with the number of unread DMs and mentions in them.
#[component]
fn ChatsButton() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let count = Memo::new(move |_| {
    let list = state.with_mutes(state.read(|backend| backend.direct_conversations()));
    badge_count(&list)
  });
  let is_current =
    Signal::derive(move || state.rail.get() == RailEntry::Chats && !is_destination_view(state.view.get()));
  let label = Signal::derive(move || match count.get() {
    0 => i18n.t("chats-title"),
    unread => i18n.t_with("rail-chats-unread", &[("count", &unread.to_string())]),
  });
  view! {
    <RailDestination
      icon=IconName::Chat
      label_key="rail-chats"
      accessible_label=label
      is_current=is_current
      count=Signal::derive(move || count.get())
      on_open=Callback::new(move |()| state.open_chats())
    />
  }
}

/// Mentions, reactions and thread replies addressed to the user.
#[component]
fn ActivityButton() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let count = Memo::new(move |_| state.read(|backend| backend.activity().unread_count));
  let tone = activity_tone(state);
  let label = Signal::derive(move || match (count.get(), tone.get()) {
    (0, _) => i18n.t("sidebar-activity"),
    (unread, BadgeTone::Mention) => i18n.t_with("rail-activity-mentions", &[("count", &unread.to_string())]),
    (unread, BadgeTone::Unread) => i18n.t_with("rail-activity-unread", &[("count", &unread.to_string())]),
  });
  view! {
    <RailDestination
      icon=IconName::Bell
      label_key="rail-activity"
      accessible_label=label
      is_current=Signal::derive(move || state.view.get() == MainView::Activity)
      count=Signal::derive(move || count.get())
      tone=tone
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
  let tone = Signal::derive(move || {
    BadgeTone::of(state.read(|backend| {
      backend
        .followed_threads()
        .iter()
        .any(|thread| thread.unread_replies > 0 && thread.has_mention)
    }))
  });
  let label = Signal::derive(move || match (count.get(), tone.get()) {
    (0, _) => i18n.t("sidebar-threads"),
    (unread, BadgeTone::Mention) => i18n.t_with("rail-threads-mentions", &[("count", &unread.to_string())]),
    (unread, BadgeTone::Unread) => i18n.t_with("rail-threads-unread", &[("count", &unread.to_string())]),
  });
  view! {
    <RailDestination
      icon=IconName::Threads
      label_key="rail-threads"
      accessible_label=label
      is_current=Signal::derive(move || state.view.get() == MainView::Threads)
      count=Signal::derive(move || count.get())
      tone=tone
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
  #[prop(into, optional)] tone: Signal<BadgeTone>,
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
        <CountBadge count=count tone=tone is_decorative=true />
      </span>
      <span class="rail-dest-label" aria-hidden="true">{move || i18n.t(label_key)}</span>
    </button>
  }
}

/// Activity is red when an unread item is a mention or in a direct conversation, quiet otherwise.
pub fn activity_tone(state: AppState) -> Signal<BadgeTone> {
  Signal::derive(move || {
    BadgeTone::of(state.read(|backend| {
      has_urgent_activity(&backend.activity().items, |id| {
        backend.conversation(id).is_some_and(|conversation| {
          matches!(
            conversation.kind,
            ConversationKind::Direct | ConversationKind::Group
          )
        })
      })
    }))
  })
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

/// Mentions as a number in a red bubble on the tile's corner (up to 9, then "9+", so it always fits
/// the bubble), other unread messages as a small neutral dot there (#715). The memo per space keeps
/// a change elsewhere from touching this tile.
#[component]
fn RailBadgeView(badge: Memo<RailBadge>) -> impl IntoView {
  view! {
    <CountBadge
      count=Signal::derive(move || badge.get().mention_count)
      is_decorative=true
      maybe_cap=RAIL_BADGE_CAP
    />
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
        i18n.t_count_with(
          "rail-space-mentions",
          current.mention_count as usize,
          &[("name", &name)],
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
