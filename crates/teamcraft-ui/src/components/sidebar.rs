use std::collections::HashSet;

use leptos::prelude::*;

use crate::components::avatar::PresenceDot;
use crate::components::huddle_bar::CallPill;
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::meetings::MeetingsHub;
use crate::model::{Conversation, ConversationKind, PresenceStatus, SpaceId};
use crate::sections::{GroupedSidebar, SectionKind, SpaceGroup, build_grouped, build_sections};
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

  let space_name = Memo::new(move |_| {
    let space = state.space.get();
    state.read(|backend| {
      backend
        .spaces()
        .into_iter()
        .find(|candidate| candidate.id == space)
        .map(|s| s.name)
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
    state.read(|backend| {
      let mut list = if is_grouped {
        backend
          .spaces()
          .into_iter()
          .flat_map(|space| backend.space_conversations(space.id))
          .collect()
      } else {
        backend.space_conversations(space)
      };
      list.extend(backend.direct_conversations());
      list
    })
  });
  let collapsed_spaces =
    RwSignal::new(load_json::<HashSet<SpaceId>>(KEY_COLLAPSED_SPACES).unwrap_or_default());
  Effect::new(move |_| collapsed_spaces.with(|set| store_json(KEY_COLLAPSED_SPACES, set)));
  let show_all_dms = RwSignal::new(false);
  // Conversations with the notification settings applied: one muted there (or through its space)
  // looks muted here too.
  let listed = Memo::new(move |_| {
    let prefs = state.settings.prefs.get();
    let mut list = conversations.get();
    for conversation in &mut list {
      let maybe_space = conversation.maybe_space.map(|space| space.to_string());
      conversation.is_muted |= prefs
        .notifications
        .is_muted_in(&conversation.id.to_string(), maybe_space.as_deref());
    }
    list
  });
  let dm_limit = move || {
    if show_all_dms.get() {
      0
    } else {
      state.settings.prefs.with(|prefs| prefs.sidebar.dm_limit as usize)
    }
  };
  let sections =
    Memo::new(move |_| build_sections(&listed.get(), &state.settings.sidebar_options(dm_limit())));
  let grouped = Memo::new(move |_| {
    build_grouped(
      &spaces.get(),
      &listed.get(),
      &state.settings.sidebar_options(dm_limit()),
    )
  });
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
    <nav class="sidebar" aria-label=move || i18n.t("sidebar-label")>
      <div class="sidebar-head">
        <h2 class="space-name" translate="no">
          {move || if is_grouped.get() { i18n.t("sidebar-all-spaces") } else { space_name.get().unwrap_or_default() }}
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
      <button class="jump-btn" on:click=move |_| state.overlay.set(Overlay::QuickSwitcher)>
        <Icon name=IconName::Search />
        <span class="jump-label">{move || i18n.t("sidebar-jump")}</span>
        <kbd>{shortcut_label("K")}</kbd>
      </button>
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
      <button
        class="sb-item"
        class:current=move || state.view.get() == MainView::Search
        aria-current=move || (state.view.get() == MainView::Search).then_some("page")
        on:click=move |_| state.open_search(None)
      >
        <Icon name=IconName::Search />
        <span class="sb-name">{move || i18n.t("sidebar-search")}</span>
      </button>
      <div class="sidebar-scroll">
        {move || {
          if is_grouped.get() {
            view! { <GroupedSections grouped=grouped show_all=show_all_dms collapsed=collapsed collapsed_spaces=collapsed_spaces /> }
              .into_any()
          } else {
            view! {
              <For each=move || sections.get() key=section_key let:section>
                <SidebarSection kind=section.kind conversations=section.conversations hidden=section.hidden_count show_all=show_all_dms collapsed=collapsed />
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

/// The grouped layout: unread and favorite conversations, one collapsible group per space, then
/// the direct messages.
#[component]
fn GroupedSections(
  grouped: Memo<GroupedSidebar>,
  show_all: RwSignal<bool>,
  collapsed: RwSignal<HashSet<SectionKind>>,
  collapsed_spaces: RwSignal<HashSet<SpaceId>>,
) -> impl IntoView {
  view! {
    <For each=move || grouped.get().top key=section_key let:section>
      <SidebarSection kind=section.kind conversations=section.conversations hidden=section.hidden_count show_all=show_all collapsed=collapsed />
    </For>
    <For
      each=move || grouped.get().spaces
      key=|group| (group.space, group.conversations.iter().map(|c| (c.id, c.unread_count, c.mention_count)).collect::<Vec<_>>())
      let:group
    >
      <SpaceGroupSection group=group collapsed=collapsed_spaces />
    </For>
    <For each=move || grouped.get().direct key=section_key let:section>
      <SidebarSection kind=section.kind conversations=section.conversations hidden=section.hidden_count show_all=show_all collapsed=collapsed />
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
  hidden: usize,
  show_all: RwSignal<bool>,
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
        {conversations.into_iter().map(|conv| view! { <li><SidebarItem conversation=conv /></li> }).collect_view()}
      </ul>
      {(hidden > 0)
        .then(|| {
          view! {
            <button class="sb-more" hidden=is_collapsed on:click=move |_| show_all.set(true)>
              {move || i18n.t_count("sidebar-show-more", hidden)}
            </button>
          }
        })}
    </section>
  }
}

#[component]
fn SidebarItem(conversation: Conversation) -> impl IntoView {
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

/// "Ctrl K" or "⌘K" depending on the platform.
pub fn shortcut_label(key: &str) -> String {
  let is_mac = web_sys::window()
    .and_then(|window| window.navigator().platform().ok())
    .is_some_and(|platform| platform.to_ascii_lowercase().contains("mac"));
  if is_mac {
    format!("⌘{key}")
  } else {
    format!("Ctrl+{key}")
  }
}
