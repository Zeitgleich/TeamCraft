use std::collections::HashSet;

use leptos::prelude::*;

use crate::components::avatar::PresenceDot;
use crate::components::huddle_bar::CallPill;
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::meetings::MeetingsHub;
use crate::model::{Conversation, ConversationKind, PresenceStatus};
use crate::sections::{SectionKind, build_sections};
use crate::state::{MainView, Overlay, use_app_state};

/// Channel sidebar: search, threads, then the categories of the current space.
#[component]
pub fn Sidebar() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let collapsed = RwSignal::new(HashSet::<SectionKind>::new());

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
  let conversations = Memo::new(move |_| {
    let space = state.space.get();
    state.read(|backend| {
      let mut list = backend.space_conversations(space);
      list.extend(backend.direct_conversations());
      list
    })
  });
  let show_all_dms = RwSignal::new(false);
  let sections = Memo::new(move |_| {
    let prefs = state.settings.prefs.get();
    let limit = if show_all_dms.get() {
      0
    } else {
      prefs.sidebar.dm_limit as usize
    };
    let mut list = conversations.get();
    // A conversation muted in the notification settings looks muted here too.
    for conversation in &mut list {
      conversation.is_muted |= prefs.notifications.is_muted(&conversation.id.to_string());
    }
    build_sections(&list, &state.settings.sidebar_options(limit))
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

  view! {
    <nav class="sidebar" aria-label=move || i18n.t("sidebar-label")>
      <div class="sidebar-head">
        <h2 class="space-name" translate="no">
          {move || space_name.get().unwrap_or_default()}
        </h2>
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
        class:current=move || state.view.get() == MainView::Search
        aria-current=move || (state.view.get() == MainView::Search).then_some("page")
        on:click=move |_| state.open_search(None)
      >
        <Icon name=IconName::Search />
        <span class="sb-name">{move || i18n.t("sidebar-search")}</span>
      </button>
      <div class="sidebar-scroll">
        <For each=move || sections.get() key=|section| {
          let state: Vec<_> = section
            .conversations
            .iter()
            .map(|c| (c.id, c.unread_count, c.mention_count, c.is_favorite))
            .collect();
          (section.kind, state)
        } let:section>
          <SidebarSection kind=section.kind conversations=section.conversations hidden=section.hidden_count show_all=show_all_dms collapsed=collapsed />
        </For>
      </div>
      <CallPill />
    </nav>
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

  view! {
    <section class="sb-section">
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
