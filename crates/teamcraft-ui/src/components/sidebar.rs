use std::collections::HashSet;

use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::components::avatar::PresenceDot;
use crate::components::huddle_bar::CallPill;
use crate::components::icons::{Icon, IconName};
use crate::components::space_tile::SpaceTile;
use crate::i18n::use_i18n;
use crate::meetings::MeetingsHub;
use crate::model::{Conversation, ConversationId, ConversationKind, PresenceStatus, SpaceId};
use crate::sections::{
  GroupedSidebar, SectionKind, SpaceGroup, build_grouped, build_sections, move_favorite,
};
use crate::settings::{load_json, store_json};
use crate::state::{MainView, Overlay, use_app_state};

/// Sidebar sections and spaces the user collapsed, remembered on this device.
const KEY_COLLAPSED_SECTIONS: &str = "teamcraft.sidebar_collapsed";
const KEY_COLLAPSED_SPACES: &str = "teamcraft.sidebar_collapsed_spaces";

/// Channel sidebar: search, threads, then the categories of the current space.
#[component]
pub fn Sidebar() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let collapsed = RwSignal::new(
    load_json::<Vec<String>>(KEY_COLLAPSED_SECTIONS)
      .unwrap_or_default()
      .iter()
      .filter_map(|slug| SectionKind::from_slug(slug))
      .collect::<HashSet<SectionKind>>(),
  );
  Effect::new(move |_| {
    let slugs: Vec<&str> = collapsed.with(|set| set.iter().map(|kind| kind.slug()).collect());
    store_json(KEY_COLLAPSED_SECTIONS, &slugs);
  });

  let current_space = Memo::new(move |_| {
    let space = state.space.get();
    state.read(|backend| {
      backend
        .spaces()
        .into_iter()
        .find(|candidate| candidate.id == space)
    })
  });
  let is_grouped = Memo::new(move |_| {
    state
      .settings
      .prefs
      .with(|prefs| prefs.sidebar.is_grouped_by_space)
  });
  let spaces = Memo::new(move |_| {
    state.read(|backend| {
      backend
        .spaces()
        .into_iter()
        .map(|space| (space.id, space.name))
        .collect::<Vec<_>>()
    })
  });
  let conversations = Memo::new(move |_| {
    let space = state.space.get();
    let is_grouped = is_grouped.get();
    let list = state.read(|backend| {
      if is_grouped {
        backend
          .spaces()
          .into_iter()
          .flat_map(|space| backend.space_conversations(space.id))
          .collect()
      } else {
        backend.space_conversations(space)
      }
    });
    state.sidebar_list(list)
  });
  let collapsed_spaces =
    RwSignal::new(load_json::<HashSet<SpaceId>>(KEY_COLLAPSED_SPACES).unwrap_or_default());
  Effect::new(move |_| collapsed_spaces.with(|set| store_json(KEY_COLLAPSED_SPACES, set)));
  let listed = Memo::new(move |_| state.with_mutes(conversations.get()));
  let sections = Memo::new(move |_| build_sections(&listed.get(), &state.settings.sidebar_options()));
  let grouped =
    Memo::new(move |_| build_grouped(&spaces.get(), &listed.get(), &state.settings.sidebar_options()));
  view! {
    <nav class="sidebar" aria-label=move || i18n.t("sidebar-label")>
      <div class="sidebar-head">
        <h2 class="space-name" translate="no">
          {move || {
            if is_grouped.get() {
              i18n.t("sidebar-all-spaces").into_any()
            } else {
              current_space
                .get()
                .map(|space| {
                  let name = space.name.clone();
                  view! { <SpaceTile space=space /> <span>{name}</span> }
                })
                .into_any()
            }
          }}
        </h2>
        <button class="icon-btn" aria-label=move || i18n.t("browse-title") title=move || i18n.t("browse-title") on:click=move |_| state.overlay.set(Overlay::BrowseChannels(state.space.get_untracked()))>
          <Icon name=IconName::List />
        </button>
        <button class="icon-btn" aria-label=move || i18n.t("channel-new-title") title=move || i18n.t("channel-new-title") on:click=move |_| state.overlay.set(Overlay::NewChannel(state.space.get_untracked()))>
          <Icon name=IconName::Plus />
        </button>
        <Show when=move || !is_grouped.get() fallback=|| ()>
          <button class="icon-btn" aria-label=move || i18n.t("space-settings-title") title=move || i18n.t("space-settings-title") on:click=move |_| state.overlay.set(Overlay::SpaceSettings(state.space.get_untracked()))>
            <Icon name=IconName::Sliders />
          </button>
        </Show>
      </div>
      <GlobalEntries />
      <div class="sidebar-scroll">
        {move || {
          if is_grouped.get() {
            view! { <GroupedSections grouped=grouped collapsed=collapsed collapsed_spaces=collapsed_spaces /> }
              .into_any()
          } else {
            view! {
              <For each=move || sections.get() key=section_key let:section>
                <SidebarSection kind=section.kind conversations=section.conversations collapsed=collapsed />
              </For>
            }
              .into_any()
          }
        }}
      </div>
      <CallPill />
    </nav>
  }
}

/// The entries that are not about one space: threads, activity, calls, meetings, people, saved
/// items. Home and the space sidebar both start with them; search lives in the top bar.
#[component]
pub fn GlobalEntries() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let thread_unread = Memo::new(move |_| {
    state.read(|backend| {
      backend
        .followed_threads()
        .iter()
        .filter(|thread| thread.unread_replies > 0)
        .count() as u32
    })
  });

  let activity_unread = Memo::new(move |_| state.read(|backend| backend.activity().unread_count));

  view! {
      <button
        class="sb-item sb-threads"
        class:current=move || state.view.get() == MainView::Threads
        class:unread={move || thread_unread.get() > 0}
        aria-current=move || (state.view.get() == MainView::Threads).then_some("page")
        on:click=move |_| state.open_threads()
      >
        <Icon name=IconName::Threads />
        <span class="sb-name">{move || i18n.t("sidebar-threads")}</span>
        {move || {
          let unread = thread_unread.get();
          (unread > 0).then(|| view! { <span class="badge-count">{unread}</span> })
        }}
      </button>
      <button
        class="sb-item sb-activity"
        class:current=move || state.view.get() == MainView::Activity
        class:unread={move || activity_unread.get() > 0}
        aria-current=move || (state.view.get() == MainView::Activity).then_some("page")
        on:click=move |_| state.open_activity()
      >
        <Icon name=IconName::Bell />
        <span class="sb-name">{move || i18n.t("sidebar-activity")}</span>
        {move || {
          let unread = activity_unread.get();
          (unread > 0).then(|| view! { <span class="badge-count">{unread}</span> })
        }}
      </button>
      <button
        class="sb-item sb-calls"
        class:current=move || state.view.get() == MainView::Calls
        aria-current=move || (state.view.get() == MainView::Calls).then_some("page")
        on:click=move |_| state.open_calls()
      >
        <Icon name=IconName::Phone />
        <span class="sb-name">{move || i18n.t("sidebar-calls")}</span>
      </button>
      {use_context::<MeetingsHub>().map(|_| {
        view! {
          <button
            class="sb-item sb-meetings"
            class:current=move || state.view.get() == MainView::Meetings
            aria-current=move || (state.view.get() == MainView::Meetings).then_some("page")
            on:click=move |_| state.open_meetings()
          >
            <Icon name=IconName::Calendar />
            <span class="sb-name">{move || i18n.t("sidebar-meetings")}</span>
          </button>
        }
      })}
      <button
        class="sb-item"
        class:current=move || state.view.get() == MainView::Directory
        aria-current=move || (state.view.get() == MainView::Directory).then_some("page")
        on:click=move |_| state.open_directory()
      >
        <Icon name=IconName::Users />
        <span class="sb-name">{move || i18n.t("sidebar-people")}</span>
      </button>
      <button
        class="sb-item"
        class:current=move || state.view.get() == MainView::Saved
        aria-current=move || (state.view.get() == MainView::Saved).then_some("page")
        on:click=move |_| state.open_saved()
      >
        <Icon name=IconName::Bookmark />
        <span class="sb-name">{move || i18n.t("sidebar-saved")}</span>
      </button>
  }
}

/// What makes a section look different on screen: its kind and the state of its rows.
type SectionKey = (SectionKind, Vec<(crate::model::ConversationId, u32, u32, bool)>);

fn section_key(section: &crate::sections::Section) -> SectionKey {
  let rows = section
    .conversations
    .iter()
    .map(|c| (c.id, c.unread_count, c.mention_count, c.is_favorite))
    .collect();
  (section.kind, rows)
}

/// The grouped layout: unread and favorite channels, then one collapsible group per space.
#[component]
fn GroupedSections(
  grouped: Memo<GroupedSidebar>,
  collapsed: RwSignal<HashSet<SectionKind>>,
  collapsed_spaces: RwSignal<HashSet<SpaceId>>,
) -> impl IntoView {
  view! {
    <For each=move || grouped.get().top key=section_key let:section>
      <SidebarSection kind=section.kind conversations=section.conversations collapsed=collapsed />
    </For>
    <For
      each=move || grouped.get().spaces
      key=|group| (group.space, group.conversations.iter().map(|c| (c.id, c.unread_count, c.mention_count)).collect::<Vec<_>>())
      let:group
    >
      <SpaceGroupSection group=group collapsed=collapsed_spaces />
    </For>
  }
}

/// The channels of one space, under a header with the space's name, a mute toggle and its settings.
#[component]
fn SpaceGroupSection(group: SpaceGroup, collapsed: RwSignal<HashSet<SpaceId>>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let space = group.space;
  let space_id = space.to_string();
  let is_collapsed = move || collapsed.with(|set| set.contains(&space));
  let mentions: u32 = group.conversations.iter().map(|conv| conv.mention_count).sum();
  let list_id = format!("sb-space-{space_id}");
  let is_muted = {
    let space_id = space_id.clone();
    Memo::new(move |_| {
      state
        .settings
        .prefs
        .with(|prefs| prefs.notifications.is_space_muted(&space_id))
    })
  };
  let name = group.name.clone();
  let toggle_label = {
    let name = name.clone();
    move || {
      i18n.t_with(
        if is_muted.get() {
          "space-unmute"
        } else {
          "space-mute"
        },
        &[("name", &name)],
      )
    }
  };

  view! {
    <section class="sb-section sb-space-group" class:muted=move || is_muted.get()>
      <div class="sb-section-head sb-space-head">
        <button
          class="sb-space-toggle"
          aria-expanded=move || (!is_collapsed()).to_string()
          aria-controls=list_id.clone()
          on:click=move |_| {
            collapsed.update(|set| {
              if !set.remove(&space) {
                set.insert(space);
              }
            });
          }
        >
          <Icon name=IconName::ChevronDown class="chevron" />
          <span translate="no">{name.clone()}</span>
          {move || (is_collapsed() && mentions > 0).then(|| view! { <span class="badge-count">{mentions}</span> })}
        </button>
        <button
          class="icon-btn icon-btn-small"
          aria-pressed=move || is_muted.get().to_string()
          aria-label=toggle_label.clone()
          title=toggle_label.clone()
          on:click={
            let space_id = space_id.clone();
            move |_| {
              state.settings.prefs.update(|prefs| {
                let muted = prefs.notifications.is_space_muted(&space_id);
                prefs.notifications.set_space(&space_id, space_mute_entry(!muted));
              })
            }
          }
        >
          <Icon name=IconName::Bell />
        </button>
        <button
          class="icon-btn icon-btn-small"
          aria-label=move || i18n.t("space-settings-title")
          title=move || i18n.t("space-settings-title")
          on:click=move |_| state.overlay.set(Overlay::SpaceSettings(space))
        >
          <Icon name=IconName::Sliders />
        </button>
      </div>
      <ul id=list_id hidden=is_collapsed>
        {group.conversations.into_iter().map(|conv| view! { <li><SidebarItem conversation=conv /></li> }).collect_view()}
      </ul>
    </section>
  }
}

/// The notification entry of a muted (or unmuted) space: "mute space" silences it until undone.
pub fn space_mute_entry(is_muted: bool) -> crate::prefs::ConversationNotify {
  crate::prefs::ConversationNotify {
    maybe_level: None,
    mute: if is_muted {
      crate::prefs::MuteFor::Forever
    } else {
      crate::prefs::MuteFor::Off
    },
  }
}

#[component]
fn SidebarSection(
  kind: SectionKind,
  conversations: Vec<Conversation>,
  collapsed: RwSignal<HashSet<SectionKind>>,
) -> impl IntoView {
  let i18n = use_i18n();
  let is_collapsed = move || collapsed.with(|set| set.contains(&kind));
  let unread_hidden: u32 = conversations.iter().map(|conv| conv.mention_count).sum();
  let list_id = format!("sb-{}", kind.slug());

  let state = use_app_state();
  view! {
    <section class="sb-section">
      <div class="sb-section-row">
        <button
          class="sb-section-head"
          aria-expanded=move || (!is_collapsed()).to_string()
          aria-controls=list_id.clone()
          on:click=move |_| {
            collapsed.update(|set| {
              if !set.remove(&kind) {
                set.insert(kind);
              }
            });
          }
        >
          <Icon name=IconName::ChevronDown class="chevron" />
          <span>{move || i18n.t(kind.title_key())}</span>
          {move || (is_collapsed() && unread_hidden > 0).then(|| view! { <span class="badge-count">{unread_hidden}</span> })}
        </button>
        {(kind == SectionKind::Unreads)
          .then(|| {
            view! {
              <button
                class="icon-btn icon-btn-small sb-section-action"
                aria-label=move || i18n.t("sidebar-mark-all-read")
                title=move || i18n.t("sidebar-mark-all-read")
                on:click=move |_| state.mark_all_read()
              >
                <Icon name=IconName::Check />
              </button>
            }
          })}
      </div>
      <ul id=list_id hidden=is_collapsed>
        {if kind == SectionKind::Favorites {
          view! { <FavoriteItems conversations=conversations /> }.into_any()
        } else {
          conversations.into_iter().map(|conv| view! { <li><SidebarItem conversation=conv /></li> }).collect_view().into_any()
        }}
      </ul>
    </section>
  }
}

/// Favorites can be put in order by dragging, or with Ctrl+Shift+↑/↓ on the focused one.
#[component]
fn FavoriteItems(conversations: Vec<Conversation>) -> impl IntoView {
  let state = use_app_state();
  let shown: Vec<ConversationId> = conversations.iter().map(|conv| conv.id).collect();
  let shown = StoredValue::new(shown);
  let dragged = RwSignal::new(None::<ConversationId>);
  let move_to = move |moved: ConversationId, to: usize| {
    let order = shown.with_value(|shown| move_favorite(shown, moved, to));
    state.settings.favorite_order.set(order);
    // The list is rebuilt; keep the keyboard focus on the moved item.
    set_timeout(move || focus_sidebar_item(moved), std::time::Duration::ZERO);
  };
  conversations
    .into_iter()
    .enumerate()
    .map(|(index, conv)| {
      let id = conv.id;
      view! {
        <li
          draggable="true"
          on:dragstart=move |_| dragged.set(Some(id))
          on:dragend=move |_| dragged.set(None)
          on:dragover=move |ev| {
            if dragged.get_untracked().is_some() {
              ev.prevent_default();
            }
          }
          on:drop=move |ev| {
            if let Some(moving) = dragged.get_untracked() {
              ev.prevent_default();
              dragged.set(None);
              move_to(moving, index);
            }
          }
          on:keydown=move |ev| {
            if !(ev.ctrl_key() && ev.shift_key()) {
              return;
            }
            let maybe_to = match ev.key().as_str() {
              "ArrowUp" => index.checked_sub(1),
              "ArrowDown" => Some(index + 1),
              _ => None,
            };
            if let Some(to) = maybe_to {
              ev.prevent_default();
              move_to(id, to);
            }
          }
        >
          <SidebarItem conversation=conv />
        </li>
      }
    })
    .collect_view()
}

fn focus_sidebar_item(conversation: ConversationId) {
  let selector = format!("[data-conversation=\"{conversation}\"]");
  if let Some(element) = web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.query_selector(&selector).ok().flatten())
    .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
  {
    drop(element.focus());
  }
}

#[component]
pub fn SidebarItem(conversation: Conversation) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let id = conversation.id;
  let is_current = move || state.view.get() == MainView::Conversation(id);
  let unread = conversation.unread_count > 0 && !conversation.is_muted;
  let mentions = conversation.mention_count;
  let me = state.me();
  let maybe_other = conversation
    .member_ids
    .iter()
    .copied()
    .find(|member| *member != me.id)
    .filter(|_| conversation.kind == ConversationKind::Direct)
    .and_then(|member| state.read(|backend| backend.user(member)));

  let leading = match conversation.kind {
    ConversationKind::Public => view! { <Icon name=IconName::Hash /> }.into_any(),
    ConversationKind::Private => view! { <Icon name=IconName::Lock /> }.into_any(),
    ConversationKind::Integration => view! { <Icon name=IconName::Terminal /> }.into_any(),
    ConversationKind::Group => view! { <Icon name=IconName::Users /> }.into_any(),
    ConversationKind::Direct => {
      // Presence arrives later than the row is built, so it is read reactively.
      let maybe_other_id = maybe_other.as_ref().map(|user| user.id);
      let presence = move || {
        maybe_other_id
          .and_then(|member| state.read(|backend| backend.user(member)))
          .map_or(PresenceStatus::Offline, |user| user.presence)
      };
      view! { <span class="dm-dot">{move || view! { <PresenceDot presence=presence() /> }}</span> }.into_any()
    },
  };
  let external_probe = conversation.clone();
  let call_peer = (conversation.kind == ConversationKind::Direct)
    .then(|| {
      conversation
        .member_ids
        .iter()
        .copied()
        .find(|member| *member != me.id)
    })
    .flatten();
  let status_emoji = maybe_other
    .and_then(|user| user.maybe_custom_status)
    .map(|status| status.emoji);
  let name = conversation.name.clone();
  let aria_name = name.clone();
  let label = move || {
    if mentions > 0 {
      i18n.t_with(
        "sidebar-item-mentions",
        &[("name", &aria_name), ("count", &mentions.to_string())],
      )
    } else if unread {
      i18n.t_with("sidebar-item-unread", &[("name", &aria_name)])
    } else {
      aria_name.clone()
    }
  };

  view! {
    <button
      class="sb-item"
      class:current=is_current
      class:unread=unread
      class:muted=conversation.is_muted
      data-conversation=id.to_string()
      aria-current=move || is_current().then_some("page")
      aria-label=label
      on:click=move |_| state.open_conversation(id)
    >
      {leading} <span class="sb-name" translate="no">
        {name}
      </span> {status_emoji.map(|emoji| view! { <span class="sb-status" aria-hidden="true">{emoji}</span> })}
      {move || {
        state
          .read(|backend| crate::components::channel_view::has_external_members(backend, &external_probe))
          .then(|| view! { <span class="sb-external" title=i18n.t("external-badge-detail")><Icon name=IconName::User /></span> })
      }}
      {move || {
        call_peer
          .filter(|peer| state.read(|backend| backend.is_in_call(*peer)))
          .map(|_| view! { <span class="sb-call" title=i18n.t("presence-in-call")><Icon name=IconName::Phone /></span> })
      }}
      {(mentions > 0).then(|| view! { <span class="badge-count" aria-hidden="true">{mentions}</span> })}
    </button>
  }
}
