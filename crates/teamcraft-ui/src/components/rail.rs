use leptos::ev;
use leptos::html;
use leptos::prelude::*;

use crate::channel_access::is_guest;
use crate::components::count_badge::{BadgeTone, CountBadge};
use crate::components::entry_menu::EntryMenuTrigger;
use crate::components::icons::{Icon, IconName};
use crate::components::rail_menus::{RailCreateMenu, RailFocusMenu};
use crate::components::space_tile::SpaceTileContent;
use crate::entry_actions::MenuEntry;
use crate::i18n::use_i18n;
use crate::model::{ConversationKind, Space, SpaceId};
use crate::sections::{RailBadge, badge_count, has_urgent_activity};
use crate::state::{AppState, Overlay, RailEntry, use_app_state};

/// The largest count a space tile's badge shows; more reads "99+", so the badge keeps its size.
const RAIL_BADGE_CAP: u32 = 99;

/// Space switcher on the far left (Mattermost "team sidebar"). On top the global places with a
/// label under each icon: Chats (the DMs, which live outside spaces) and Activity, whose list takes
/// the sidebar's place like a space's (#752; followed threads are one of its filters). In the
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
  let is_current = Signal::derive(move || state.rail.get() == RailEntry::Chats);
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

/// Mentions, reactions and thread replies addressed to the user, and the threads they follow.
#[component]
fn ActivityButton() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let count = Memo::new(move |_| state.read(|backend| backend.activity().badge_count()));
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
      maybe_tip_key="rail-activity-tip"
      is_current=Signal::derive(move || state.rail.get() == RailEntry::Activity)
      count=Signal::derive(move || count.get())
      tone=tone
      on_open=Callback::new(move |()| state.open_activity())
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
  /// A sentence for the tooltip saying what the place is for, after the label.
  #[prop(optional)]
  maybe_tip_key: Option<&'static str>,
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
      title=move || match maybe_tip_key {
        Some(key) => format!("{}\n{}", accessible_label.get(), i18n.t(key)),
        None => accessible_label.get(),
      }
      aria-description=move || maybe_tip_key.map(|key| i18n.t(key))
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

/// The space's unread messages as a number in a red bubble on the tile's corner, up to "99+"
/// (#955), so a busy space stands out from a quiet one.
#[component]
fn RailBadgeView(badge: Memo<RailBadge>) -> impl IntoView {
  view! {
    <CountBadge
      count=Signal::derive(move || badge.get().count)
      is_decorative=true
      maybe_cap=RAIL_BADGE_CAP
    />
  }
}

/// What the rail entry of `space` counts: only the space's own channels, DMs have their own entry.
/// It reads through `state`, so a memo of it follows every new or read message.
fn space_badge(state: AppState, space: SpaceId) -> RailBadge {
  RailBadge::of(&state.with_mutes(state.read(|backend| backend.space_conversations(space))))
}

#[component]
fn SpaceButton(space: Space) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let id = space.id;
  // The memo per space keeps a change elsewhere from touching this tile.
  let badge = Memo::new(move |_| space_badge(state, id));
  let menu = EntryMenuTrigger::new(MenuEntry::Space(id));
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
      let unread = current.count.to_string();
      match (current.count, current.mention_count) {
        (0, _) => name.clone(),
        (_, 0) => i18n.t_with("rail-space-unread", &[("name", &name), ("unread", &unread)]),
        (_, mentions) => i18n.t_count_with(
          "rail-space-mentions",
          mentions as usize,
          &[("name", &name), ("unread", &unread)],
        ),
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
        class:is-pressing=move || menu.is_pressing()
        on:pointerdown=move |ev| menu.on_pointerdown(&ev)
        on:pointermove=move |ev| menu.on_pointermove(&ev)
        on:pointerup=move |_| menu.on_pointerup()
        on:pointercancel=move |_| menu.on_pointerup()
        on:contextmenu=move |ev| menu.on_contextmenu(&ev)
        on:keydown=move |ev| menu.on_keydown(&ev)
        on:click:capture=move |ev| menu.on_click_capture(&ev)
        on:click=move |_| state.open_space(id)
      >
        <SpaceTileContent space=space.clone() />
        <RailBadgeView badge=badge />
      </button>
    </li>
  }
}

#[cfg(test)]
mod tests {
  use std::rc::Rc;

  use super::*;
  use crate::backend::Backend;
  use crate::mock::MockBackend;
  use crate::model::{ConversationId, TimestampMs};
  use crate::prefs::{ConversationNotify, MuteFor};
  use crate::settings::Settings;

  const NOW: TimestampMs = 1_792_584_000_000;

  #[test]
  fn another_space_counts_its_own_channels_whichever_space_is_open() {
    let (_, state) = demo_state();
    let (open, other) = spaces(state);
    state.open_space(open);
    let before = (space_badge(state, open), space_badge(state, other));
    assert_eq!(
      before.1,
      RailBadge {
        count: 5,
        mention_count: 1
      },
      "Project B: 5 unread in #planning, one of them a mention"
    );
    assert!(before.0.count > 0 && before.0 != before.1);
    state.open_space(other);
    assert_eq!((space_badge(state, open), space_badge(state, other)), before);
  }

  #[test]
  fn the_badge_follows_new_and_read_messages() {
    let (mock, state) = demo_state();
    let (_, other) = spaces(state);
    let badge = Memo::new(move |_| space_badge(state, other));
    assert_eq!(badge.get().count, 5);

    mock.receive(channel(state, other, "general"), false);
    state.backend_changed();
    assert_eq!(
      badge.get(),
      RailBadge {
        count: 6,
        mention_count: 1
      }
    );

    state.write(|backend| backend.mark_read(channel(state, other, "planning")));
    assert_eq!(
      badge.get(),
      RailBadge {
        count: 1,
        mention_count: 0
      }
    );
  }

  #[test]
  fn a_muted_space_counts_only_its_mentions() {
    let (_, state) = demo_state();
    let (_, other) = spaces(state);
    state.settings.prefs.update(|prefs| {
      prefs.notifications.set_space(
        &other.to_string(),
        ConversationNotify {
          maybe_level: None,
          mute: MuteFor::Forever,
        },
      );
    });
    assert_eq!(
      space_badge(state, other),
      RailBadge {
        count: 1,
        mention_count: 1
      }
    );
  }

  fn demo_state() -> (Rc<MockBackend>, AppState) {
    let mock = Rc::new(MockBackend::new(NOW));
    let backend: Rc<dyn Backend> = mock.clone();
    (mock, AppState::new(backend, Settings::for_tests(), None))
  }

  /// The demo's two spaces: Project A, then Project B.
  fn spaces(state: AppState) -> (SpaceId, SpaceId) {
    let spaces = state.read(|backend| backend.spaces());
    (spaces[0].id, spaces[1].id)
  }

  fn channel(state: AppState, space: SpaceId, name: &str) -> ConversationId {
    state
      .read(|backend| backend.space_conversations(space))
      .into_iter()
      .find(|conversation| conversation.name == name)
      .map(|conversation| conversation.id)
      .expect("a demo channel")
  }
}
