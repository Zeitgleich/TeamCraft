use std::collections::HashSet;
use std::time::Duration;

use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::channel_access::{can_create_channel_in, can_invite_to, is_guest};
use crate::components::avatar::PresenceDot;
use crate::components::channel_glyph::ChannelGlyph;
use crate::components::count_badge::{BadgeTone, CountBadge};
use crate::components::huddle_bar::CallPill;
use crate::components::icons::{Icon, IconName};
use crate::components::space_tile::SpaceTile;
use crate::i18n::use_i18n;
use crate::meetings::MeetingsHub;
use crate::model::{Conversation, ConversationId, ConversationKind, PresenceStatus, SpaceId};
use crate::reactive::{items_by_key, keyed_item};
use crate::sections::{
  GroupedSidebar, RowBadgeTone, Section, SectionKind, SpaceGroup, badge_count, build_grouped, build_sections,
  move_favorite, row_badge,
};
use crate::settings::{load_json, store_json};
use crate::state::{MainView, Overlay, use_app_state};

/// Sidebar sections and spaces the user collapsed, remembered on this device.
const KEY_COLLAPSED_SECTIONS: &str = "teamcraft.sidebar_collapsed";
const KEY_COLLAPSED_SPACES: &str = "teamcraft.sidebar_collapsed_spaces";
/// Whether the global entries (calls, meetings, people, ...) are folded away on a phone (#703).
const KEY_ENTRIES_COLLAPSED: &str = "teamcraft.sidebar_entries_collapsed";
/// How long the rows of a closing section stay: the exit (`--dur-exit`, 100 ms) plus a frame, in
/// case `animationend` never comes (the sidebar is hidden).
const SECTION_EXIT: Duration = Duration::from_millis(140);

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
        // The head shows no counts: a new message must not build its tile (picture) again.
        .map(|mut space| {
          space.unread_count = 0;
          space.mention_count = 0;
          space
        })
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
  // A local filter of the list below, not the global search.
  let filter = ListFilter::new();
  let query = filter.query;
  let entries = EntriesDisclosure::new();
  let listed = Memo::new(move |_| {
    let needle = query.get().trim().to_lowercase();
    let list = state.with_mutes(conversations.get());
    if needle.is_empty() {
      list
    } else {
      list
        .into_iter()
        .filter(|conversation| conversation.name.to_lowercase().contains(&needle))
        .collect()
    }
  });
  // The conversation opened while unread stays in the "Unreads" section until another one opens,
  // so the list doesn't shift under the pointer (#890). Read untracked: the count dropping to
  // zero because the conversation is being read must not release it.
  Effect::new(move |_| {
    let held = match state.view.get() {
      MainView::Conversation(id) if state.held_unread.get_untracked() == Some(id) => Some(id),
      MainView::Conversation(id) => untrack(|| {
        state
          .read(|backend| backend.conversation(id))
          .filter(|conversation| conversation.unread_count > 0 && !conversation.is_muted)
          .map(|conversation| conversation.id)
      }),
      _ => None,
    };
    if state.held_unread.get_untracked() != held {
      state.held_unread.set(held);
    }
  });
  let sections = Memo::new(move |_| {
    build_sections(
      &listed.get(),
      &state.settings.sidebar_options(state.held_unread.get()),
    )
  });
  let grouped = Memo::new(move |_| {
    build_grouped(
      &spaces.get(),
      &listed.get(),
      &state.settings.sidebar_options(state.held_unread.get()),
    )
  });
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
        <EntriesToggle entries=entries controls="sidebar-entries" />
        <ListFilterToggle filter=filter field_id="sidebar-filter" label_key="sidebar-filter" />
        <Show when=move || !is_guest(state) fallback=|| ()>
          <button class="icon-btn" aria-label=move || i18n.t("browse-title") title=move || i18n.t("browse-title") on:click=move |_| state.overlay.set(Overlay::BrowseChannels(state.space.get_untracked()))>
            <Icon name=IconName::List />
          </button>
        </Show>
        <Show when=move || can_create_channel_in(state, state.space.get()) fallback=|| ()>
          <button class="icon-btn" aria-label=move || i18n.t("channel-new-title") title=move || i18n.t("channel-new-title") on:click=move |_| state.overlay.set(Overlay::NewChannel(state.space.get_untracked()))>
            <Icon name=IconName::Plus />
          </button>
        </Show>
        <Show when=move || !is_grouped.get() fallback=|| ()>
          <Show when=move || can_invite_to(state, state.space.get()) fallback=|| ()>
            <button class="icon-btn" aria-label=move || i18n.t("invite-title") title=move || i18n.t("invite-title") on:click=move |_| state.overlay.set(Overlay::InvitePeople(state.space.get_untracked()))>
              <Icon name=IconName::UserPlus />
            </button>
          </Show>
          <button class="icon-btn" aria-label=move || i18n.t("space-settings-title") title=move || i18n.t("space-settings-title") on:click=move |_| state.overlay.set(Overlay::SpaceSettings(state.space.get_untracked()))>
            <Icon name=IconName::Sliders />
          </button>
        </Show>
      </div>
      <GlobalEntries entries=entries id="sidebar-entries" />
      <ListFilterField filter=filter id="sidebar-filter" label_key="sidebar-filter" />
      <div class="sidebar-scroll">
        {move || {
          if is_grouped.get() {
            view! { <GroupedSections grouped=grouped collapsed=collapsed collapsed_spaces=collapsed_spaces /> }
              .into_any()
          } else {
            view! { <SectionList sections=sections.into() collapsed=collapsed /> }.into_any()
          }
        }}
      </div>
      <CallPill />
    </nav>
  }
}

/// The entries that are not about one space: calls, meetings, people, saved items. Home and the
/// space sidebar both start with them; threads and activity are on the rail, search in the top bar.
#[component]
pub fn GlobalEntries(entries: EntriesDisclosure, id: &'static str) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();

  // One row of pills on a phone (#703), folded away there when the user chose so; a list on wide
  // layouts, which ignore the folding.
  view! {
    <div class="sb-entries" id=id class:is-collapsed=move || entries.is_collapsed.get()>
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
        class:current=move || state.view.get() == MainView::Files
        aria-current=move || (state.view.get() == MainView::Files).then_some("page")
        on:click=move |_| state.open_files()
      >
        <Icon name=IconName::Paperclip />
        <span class="sb-name">{move || i18n.t("sidebar-files")}</span>
      </button>
    </div>
  }
}

/// Whether the global entries are folded away on a phone (#703). Remembered on this device, the
/// same for Home and the space sidebar; wide layouts always show the entries.
#[derive(Clone, Copy)]
pub struct EntriesDisclosure {
  is_collapsed: RwSignal<bool>,
}

impl EntriesDisclosure {
  pub fn new() -> Self {
    let is_collapsed = RwSignal::new(load_json::<bool>(KEY_ENTRIES_COLLAPSED).unwrap_or(false));
    Effect::new(move |previous: Option<bool>| {
      let now = is_collapsed.get();
      // Nothing to write for the value just read.
      if previous.is_some() {
        store_json(KEY_ENTRIES_COLLAPSED, &now);
      }
      now
    });
    Self { is_collapsed }
  }

  fn toggle(self) {
    self.is_collapsed.update(|collapsed| *collapsed = !*collapsed);
  }
}

impl Default for EntriesDisclosure {
  fn default() -> Self {
    Self::new()
  }
}

/// The chevron beside a sidebar's title that folds its global entries away on a phone; wide layouts
/// hide it.
#[component]
pub fn EntriesToggle(entries: EntriesDisclosure, controls: &'static str) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <button
      type="button"
      class="icon-btn sb-entries-toggle"
      class:is-collapsed=move || entries.is_collapsed.get()
      aria-label=move || i18n.t("sidebar-shortcuts")
      title=move || i18n.t("sidebar-shortcuts")
      aria-controls=controls
      aria-expanded=move || (!entries.is_collapsed.get()).to_string()
      on:click=move |_| entries.toggle()
    >
      <Icon name=IconName::ChevronDown />
    </button>
  }
}

/// The text filter of a sidebar's list (#703). Wide layouts always show its field; on a phone the
/// field waits behind a search button in the sidebar's header until it is used, so the list starts
/// near the top of the screen. It stays while it holds text.
#[derive(Clone, Copy)]
pub struct ListFilter {
  pub query: RwSignal<String>,
  is_open: RwSignal<bool>,
  wrap: NodeRef<html::Div>,
  field: NodeRef<html::Input>,
  toggle: NodeRef<html::Button>,
}

impl ListFilter {
  pub fn new() -> Self {
    Self {
      query: RwSignal::new(String::new()),
      is_open: RwSignal::new(false),
      wrap: NodeRef::new(),
      field: NodeRef::new(),
      toggle: NodeRef::new(),
    }
  }

  /// Shows the field and puts the caret in it. The class goes on by hand before the focus: the
  /// reactive class follows a moment later, and a hidden field can't take focus (on iOS, the
  /// keyboard only opens for a focus inside the tap).
  fn open(self) {
    if let Some(wrap) = self.wrap.get_untracked() {
      drop(wrap.class_list().add_1("is-open"));
    }
    self.is_open.set(true);
    if let Some(field) = self.field.get_untracked() {
      drop(field.focus());
    }
  }

  fn close_if_empty(self) {
    if self.query.with_untracked(String::is_empty) {
      self.is_open.set(false);
    }
  }

  /// Hides the field from the keyboard; the focus goes back to the button that opened it.
  fn close(self) {
    self.is_open.set(false);
    if let Some(toggle) = self.toggle.get_untracked() {
      drop(toggle.focus());
    }
  }
}

impl Default for ListFilter {
  fn default() -> Self {
    Self::new()
  }
}

/// The search button that opens a [`ListFilter`] on a phone; wide layouts hide it.
#[component]
pub fn ListFilterToggle(
  filter: ListFilter,
  field_id: &'static str,
  label_key: &'static str,
) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <button
      type="button"
      class="icon-btn sb-filter-toggle"
      aria-label=move || i18n.t(label_key)
      title=move || i18n.t(label_key)
      aria-controls=field_id
      node_ref=filter.toggle
      aria-expanded=move || filter.is_open.get().to_string()
      // Keeps the focus in an open field, so its blur doesn't close it under the press.
      on:mousedown=|ev| ev.prevent_default()
      on:click=move |_| filter.open()
    >
      <Icon name=IconName::Search />
    </button>
  }
}

/// The field of a [`ListFilter`]. Escape clears it, a second Escape (or leaving it empty) hides it
/// again on a phone.
#[component]
pub fn ListFilterField(filter: ListFilter, id: &'static str, label_key: &'static str) -> impl IntoView {
  let i18n = use_i18n();
  let query = filter.query;
  view! {
    <div
      class="chats-filter"
      class:is-open=move || filter.is_open.get()
      class:has-query=move || !query.with(String::is_empty)
      node_ref=filter.wrap
    >
      <label class="visually-hidden" for=id>{move || i18n.t(label_key)}</label>
      <input
        id=id
        class="input"
        type="search"
        autocomplete="off"
        spellcheck="false"
        placeholder=move || i18n.t(label_key)
        node_ref=filter.field
        prop:value=move || query.get()
        on:input=move |ev| query.set(event_target_value(&ev))
        on:blur=move |_| filter.close_if_empty()
        on:keydown=move |ev| {
          if ev.key() != "Escape" {
            return;
          }
          if !query.with_untracked(String::is_empty) {
            ev.stop_propagation();
            query.set(String::new());
          } else if filter.is_open.get_untracked() {
            ev.stop_propagation();
            filter.close();
          }
        }
      />
    </div>
  }
}

/// The sections of the sidebar, each built once: a section and its rows stay while their channels
/// change (new messages, unread counts, presence), so nothing on screen is built again that didn't
/// change (issue #687). A section only goes when it has no channels left.
#[component]
fn SectionList(sections: Signal<Vec<Section>>, collapsed: RwSignal<HashSet<SectionKind>>) -> impl IntoView {
  view! {
    <For
      each=move || sections.with(|sections| sections.iter().map(|section| section.kind).collect::<Vec<_>>())
      key=|kind| *kind
      let:kind
    >
      <SidebarSection kind=kind conversations=section_conversations(sections, kind) collapsed=collapsed />
    </For>
  }
}

/// The channels of the section of `kind`; the last ones once it is gone (it is about to be removed).
fn section_conversations(sections: Signal<Vec<Section>>, kind: SectionKind) -> Memo<Vec<Conversation>> {
  Memo::new(move |previous: Option<&Vec<Conversation>>| {
    sections
      .with(|sections| {
        sections
          .iter()
          .find(|section| section.kind == kind)
          .map(|section| section.conversations.clone())
      })
      .or_else(|| previous.cloned())
      .unwrap_or_default()
  })
}

/// The grouped layout: unread and favorite channels, then one collapsible group per space.
#[component]
fn GroupedSections(
  grouped: Memo<GroupedSidebar>,
  collapsed: RwSignal<HashSet<SectionKind>>,
  collapsed_spaces: RwSignal<HashSet<SpaceId>>,
) -> impl IntoView {
  let top = Signal::derive(move || grouped.with(|grouped| grouped.top.clone()));
  view! {
    <SectionList sections=top collapsed=collapsed />
    <For
      each=move || grouped.with(|grouped| grouped.spaces.iter().map(|group| group.space).collect::<Vec<_>>())
      key=|space| *space
      let:space
    >
      <SpaceGroupSection space=space grouped=grouped collapsed=collapsed_spaces />
    </For>
  }
}

/// The channels of one space, under a header with the space's name, a mute toggle and its settings.
#[component]
fn SpaceGroupSection(
  space: SpaceId,
  grouped: Memo<GroupedSidebar>,
  collapsed: RwSignal<HashSet<SpaceId>>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  // Like `section_conversations`: the last one once the space has no group any more.
  let group = Memo::new(move |previous: Option<&Option<SpaceGroup>>| {
    grouped
      .with(|grouped| grouped.spaces.iter().find(|group| group.space == space).cloned())
      .or_else(|| previous.cloned().flatten())
  });
  let conversations = Memo::new(move |_| {
    group
      .with(|group| group.as_ref().map(|group| group.conversations.clone()))
      .unwrap_or_default()
  });
  let space_id = space.to_string();
  let is_collapsed = move || collapsed.with(|set| set.contains(&space));
  let mentions =
    Memo::new(move |_| conversations.with(|list| list.iter().map(|conv| conv.mention_count).sum::<u32>()));
  let list_id = format!("sb-space-{space_id}");
  let phase = RwSignal::new(SectionPhase::Still);
  let is_muted = {
    let space_id = space_id.clone();
    Memo::new(move |_| {
      state
        .settings
        .prefs
        .with(|prefs| prefs.notifications.is_space_muted(&space_id))
    })
  };
  let name = Memo::new(move |_| {
    group
      .with(|group| group.as_ref().map(|group| group.name.clone()))
      .unwrap_or_default()
  });
  let toggle_label = move || {
    i18n.t_with(
      if is_muted.get() {
        "space-unmute"
      } else {
        "space-mute"
      },
      &[("name", &name.get())],
    )
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
              if set.remove(&space) {
                phase.set(SectionPhase::Opening);
              } else {
                set.insert(space);
                close_section(phase);
              }
            });
          }
        >
          <Icon name=IconName::ChevronDown class="chevron" />
          <span translate="no">{move || name.get()}</span>
          {move || {
            let mentions = mentions.get();
            (is_collapsed() && mentions > 0).then(|| view! { <span class="badge-count">{mentions}</span> })
          }}
        </button>
        <button
          class="icon-btn icon-btn-small"
          aria-pressed=move || is_muted.get().to_string()
          aria-label=toggle_label
          title=toggle_label
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
        <Show when=move || can_invite_to(state, space) fallback=|| ()>
          <button
            class="icon-btn icon-btn-small"
            aria-label=move || i18n.t("invite-title")
            title=move || i18n.t("invite-title")
            on:click=move |_| state.overlay.set(Overlay::InvitePeople(space))
          >
            <Icon name=IconName::UserPlus />
          </button>
        </Show>
        <button
          class="icon-btn icon-btn-small"
          aria-label=move || i18n.t("space-settings-title")
          title=move || i18n.t("space-settings-title")
          on:click=move |_| state.overlay.set(Overlay::SpaceSettings(space))
        >
          <Icon name=IconName::Sliders />
        </button>
      </div>
      <ul
        id=list_id
        hidden=move || is_collapsed() && phase.get() != SectionPhase::Closing
        class:is-opening=move || phase.get() == SectionPhase::Opening
        // A closed list keeps its exit's end while hidden: dropping both at once can draw it for a frame.
        class:is-closing=move || phase.get() == SectionPhase::Closing || is_collapsed()
        on:animationend=move |ev| end_phase(&ev, phase)
      >
        <ConversationRows conversations=conversations />
      </ul>
    </section>
  }
}

/// The rows of a section, one per conversation, each built once and updated in place.
#[component]
fn ConversationRows(conversations: Memo<Vec<Conversation>>) -> impl IntoView {
  let by_id = items_by_key(conversations.into(), |conv: &Conversation| conv.id);
  view! {
    <For each=move || conversations.get() key=|conv| conv.id let:conv>
      <li><SidebarItem conversation=keyed_item(by_id, conv.id, conv) /></li>
    </For>
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
  conversations: Memo<Vec<Conversation>>,
  collapsed: RwSignal<HashSet<SectionKind>>,
) -> impl IntoView {
  let i18n = use_i18n();
  let is_collapsed = move || collapsed.with(|set| set.contains(&kind));
  // Hidden mentions and unread DMs stay red (like every other badge); plain unread messages in
  // channels that aren't muted are quiet.
  let mentions_hidden = Memo::new(move |_| conversations.with(|list| badge_count(list)));
  let unread_hidden = Memo::new(move |_| {
    conversations.with(|list| {
      list
        .iter()
        .filter(|conv| {
          !conv.is_muted && !matches!(conv.kind, ConversationKind::Direct | ConversationKind::Group)
        })
        .map(|conv| conv.unread_count.saturating_sub(conv.mention_count))
        .sum::<u32>()
    })
  });
  let list_id = format!("sb-{}", kind.slug());
  // Only a click animates the rows, not a re-render with new counts.
  let phase = RwSignal::new(SectionPhase::Still);

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
              if set.remove(&kind) {
                phase.set(SectionPhase::Opening);
              } else {
                set.insert(kind);
                close_section(phase);
              }
            });
          }
        >
          <Icon name=IconName::ChevronDown class="chevron" />
          <span>{move || i18n.t(kind.title_key())}</span>
          {move || {
            let mentions = mentions_hidden.get();
            (is_collapsed() && mentions > 0).then(|| view! { <span class="badge-count">{mentions}</span> })
          }}
          {move || {
            let unread = unread_hidden.get();
            (is_collapsed() && unread > 0).then(|| view! { <span class="badge-count is-quiet">{unread}</span> })
          }}
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
      <ul
        id=list_id
        hidden=move || is_collapsed() && phase.get() != SectionPhase::Closing
        class:is-opening=move || phase.get() == SectionPhase::Opening
        // A closed list keeps its exit's end while hidden: dropping both at once can draw it for a frame.
        class:is-closing=move || phase.get() == SectionPhase::Closing || is_collapsed()
        on:animationend=move |ev| end_phase(&ev, phase)
      >
        {if kind == SectionKind::Favorites {
          view! { <FavoriteItems conversations=conversations /> }.into_any()
        } else {
          view! { <ConversationRows conversations=conversations /> }.into_any()
        }}
      </ul>
      // The end of the channel list says where the others are.
      {(kind == SectionKind::Channels && !is_guest(state))
        .then(|| {
          view! {
            <button
              class="sb-more sb-footer"
              hidden=is_collapsed
              on:click=move |_| state.overlay.set(Overlay::BrowseChannels(state.space.get_untracked()))
            >
              {move || i18n.t("browse-title")}
            </button>
          }
        })}
    </section>
  }
}

/// Favorites can be put in order by dragging, or with Ctrl+Shift+↑/↓ on the focused one.
#[component]
fn FavoriteItems(conversations: Memo<Vec<Conversation>>) -> impl IntoView {
  let state = use_app_state();
  let by_id = items_by_key(conversations.into(), |conv: &Conversation| conv.id);
  let dragged = RwSignal::new(None::<ConversationId>);
  // Positions are read when they are needed: the rows stay while the order changes.
  let index_of =
    move |id: ConversationId| conversations.with_untracked(|list| list.iter().position(|conv| conv.id == id));
  let move_to = move |moved: ConversationId, to: usize| {
    let shown: Vec<ConversationId> =
      conversations.with_untracked(|list| list.iter().map(|conv| conv.id).collect());
    state
      .settings
      .favorite_order
      .set(move_favorite(&shown, moved, to));
    // The moved row is put in its new place; keep the keyboard focus on it.
    set_timeout(move || focus_sidebar_item(moved), std::time::Duration::ZERO);
  };
  view! {
    <For each=move || conversations.get() key=|conv| conv.id let:conv>
      {
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
                if let Some(index) = index_of(id) {
                  move_to(moving, index);
                }
              }
            }
            on:keydown=move |ev| {
              if !(ev.ctrl_key() && ev.shift_key()) {
                return;
              }
              let Some(index) = index_of(id) else {
                return;
              };
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
            <SidebarItem conversation=keyed_item(by_id, id, conv) />
          </li>
        }
      }
    </For>
  }
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

/// The largest unread count a sidebar row shows; above it the badge says "99+" (#891).
const SIDEBAR_BADGE_CAP: u32 = 99;

/// One conversation of the sidebar. Built once per row; what it shows follows `conversation`, so a
/// new count or presence changes the badge or the dot, not the whole row (issue #687).
#[component]
pub fn SidebarItem(#[prop(into)] conversation: Signal<Conversation>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  // A conversation keeps its id and kind.
  let (id, kind) = conversation.with_untracked(|conv| (conv.id, conv.kind));
  let is_current = move || state.view.get() == MainView::Conversation(id);
  let is_muted = Memo::new(move |_| conversation.with(|conv| conv.is_muted));
  let is_unread = Memo::new(move |_| conversation.with(|conv| conv.unread_count > 0 && !conv.is_muted));
  let mentions = Memo::new(move |_| conversation.with(|conv| conv.mention_count));
  let unread = Memo::new(move |_| conversation.with(|conv| conv.unread_count));
  let badge = Memo::new(move |_| row_badge(unread.get(), mentions.get(), is_muted.get()));
  let badge_count = Signal::derive(move || badge.get().map_or(0, |badge| badge.count));
  let badge_tone = Signal::derive(move || match badge.get().map(|badge| badge.tone) {
    Some(RowBadgeTone::Quiet) => BadgeTone::Unread,
    _ => BadgeTone::Mention,
  });
  let is_catching_up = Memo::new(move |_| conversation.with(|conv| conv.is_catching_up));
  let name = Memo::new(move |_| conversation.with(|conv| conv.name.clone()));
  let me = state.me().id;
  // The other person of a DM.
  let maybe_peer = Memo::new(move |_| {
    (kind == ConversationKind::Direct)
      .then(|| conversation.with(|conv| conv.member_ids.iter().copied().find(|member| *member != me)))
      .flatten()
  });
  let maybe_other = Memo::new(move |_| {
    maybe_peer
      .get()
      .and_then(|member| state.read(|backend| backend.user(member)))
  });

  let leading = match kind {
    ConversationKind::Public | ConversationKind::Private => {
      let fallback = if kind == ConversationKind::Public {
        IconName::Hash
      } else {
        IconName::Lock
      };
      let version = Memo::new(move |_| conversation.with(|conv| conv.maybe_image_version.clone()));
      view! { <ChannelGlyph conversation=id maybe_version=version fallback=fallback /> }.into_any()
    },
    ConversationKind::Integration => view! { <Icon name=IconName::Terminal /> }.into_any(),
    ConversationKind::Group => view! { <Icon name=IconName::Users /> }.into_any(),
    ConversationKind::Direct => {
      // Presence arrives later than the row is built, so it is read reactively.
      let presence = Memo::new(move |_| {
        maybe_other.with(|maybe_user| {
          maybe_user
            .as_ref()
            .map_or(PresenceStatus::Offline, |user| user.presence)
        })
      });
      view! { <span class="dm-dot">{move || view! { <PresenceDot presence=presence.get() /> }}</span> }
        .into_any()
    },
  };
  let status_emoji = Memo::new(move |_| {
    maybe_other.with(|maybe_user| {
      maybe_user
        .as_ref()
        .and_then(|user| user.maybe_custom_status.as_ref())
        .map(|status| status.emoji.clone())
    })
  });
  let has_external = Memo::new(move |_| {
    conversation
      .with(|conv| state.read(|backend| crate::components::channel_view::has_external_members(backend, conv)))
  });
  let is_peer_in_call = Memo::new(move |_| {
    maybe_peer
      .get()
      .is_some_and(|peer| state.read(|backend| backend.is_in_call(peer)))
  });
  let label = move || {
    let aria_name = name.get();
    let mentions = mentions.get();
    let unread = unread.get();
    let label = if is_catching_up.get() {
      i18n.t_with("sidebar-item-catching-up", &[("name", &aria_name)])
    } else if mentions > 0 && !is_muted.get() && unread > mentions {
      // More unread messages than mentions, so the unread count is always plural.
      i18n.t_count_with(
        "sidebar-item-unread-mentions",
        mentions as usize,
        &[("name", &aria_name), ("unread", &unread.to_string())],
      )
    } else if mentions > 0 {
      i18n.t_with(
        "sidebar-item-mentions",
        &[("name", &aria_name), ("count", &mentions.to_string())],
      )
    } else if unread > 0 {
      i18n.t_count_with("sidebar-item-unread", unread as usize, &[("name", &aria_name)])
    } else {
      aria_name
    };
    if is_muted.get() {
      i18n.t_with("sidebar-item-muted-label", &[("label", &label)])
    } else {
      label
    }
  };

  view! {
    <button
      class="sb-item"
      class:current=is_current
      class:unread=move || is_unread.get()
      class:muted=move || is_muted.get()
      class:catching-up=move || is_catching_up.get()
      data-conversation=id.to_string()
      aria-current=move || is_current().then_some("page")
      aria-label=label
      on:click=move |_| state.open_conversation(id)
    >
      {leading} <span class="sb-name" translate="no">
        {move || name.get()}
      </span> {move || status_emoji.get().map(|emoji| view! { <span class="sb-status" aria-hidden="true">{emoji}</span> })}
      {move || {
        has_external
          .get()
          .then(|| view! { <span class="sb-external" title=i18n.t("external-badge-detail")><Icon name=IconName::User /></span> })
      }}
      {move || {
        is_peer_in_call
          .get()
          .then(|| view! { <span class="sb-call" title=i18n.t("presence-in-call")><Icon name=IconName::Phone /></span> })
      }}
      {move || is_muted.get().then(|| view! { <span class="sb-muted-icon" title=i18n.t("sidebar-item-muted")><Icon name=IconName::BellOff /></span> })}
      {move || is_catching_up.get().then(|| view! { <span class="sb-catching-up" aria-hidden="true" title=i18n.t("catching-up")><Icon name=IconName::Clock /></span> })}
      <CountBadge count=badge_count tone=badge_tone is_decorative=true maybe_cap=SIDEBAR_BADGE_CAP />
    </button>
  }
}

/// How the rows of a sidebar section move after a click on its head: they slide in when it opens
/// and out when it closes (#371); the list is only hidden once the exit played.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SectionPhase {
  Still,
  Opening,
  Closing,
}

/// Plays the exit of a section the user just collapsed; its rows stay until it ended.
fn close_section(phase: RwSignal<SectionPhase>) {
  phase.set(SectionPhase::Closing);
  set_timeout(
    move || {
      phase.try_update(|phase| {
        if *phase == SectionPhase::Closing {
          *phase = SectionPhase::Still;
        }
      });
    },
    SECTION_EXIT,
  );
}

/// Ends a section's opening or closing when its own animation ended. `animationend` bubbles: a
/// badge animating inside the list must not end it early (issue #328).
fn end_phase(ev: &leptos::ev::AnimationEvent, phase: RwSignal<SectionPhase>) {
  if ev.target() == ev.current_target() {
    phase.set(SectionPhase::Still);
  }
}
