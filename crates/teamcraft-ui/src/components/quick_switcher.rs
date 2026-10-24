//! The one search of the top bar (Ctrl/Cmd+K, Ctrl/Cmd+F, Ctrl/Cmd+Shift+F): people, threads,
//! channels, spaces and messages, grouped by kind in that order. Opened without text it lists the
//! conversations visited last. Messages and threads are searched on this device (end-to-end
//! encrypted: nothing in plain text goes to the server); people also come from the server's
//! directory, so a colleague who shares nothing with the user yet is found.

use leptos::prelude::*;

use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::icons::{Icon, IconName};
use crate::components::modal::Modal;
use crate::components::space_tile::SpaceTile;
use crate::i18n::use_i18n;
use crate::keys::{ENTER, ESCAPE, SEARCH_MESSAGES, ShortcutKeys};
use crate::model::{
  Conversation, ConversationId, ConversationKind, DeviceSetup, JoinableSpace, Space, SpaceId, ThreadSummary,
  User, UserId, thread_id_of,
};
use crate::navigation::fuzzy_score;
use crate::reactive::use_debounced;
use crate::search::{SearchHit, parse_query};
use crate::state::{AppState, use_app_state};

/// Most entries per group.
const MAX_PEOPLE: usize = 5;
const MAX_THREADS: usize = 3;
const MAX_CHANNELS: usize = 6;
const MAX_SPACES: usize = 3;
const MAX_MESSAGES: usize = 3;
/// Conversations listed before anything is typed.
const MAX_RECENT: usize = 8;
/// The directory and the message index are asked once typing paused this long.
const DEBOUNCE_MS: u64 = 250;

/// The kinds of results, in the order they are listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GroupKind {
  /// Nothing typed: the conversations visited last.
  Recent,
  People,
  Threads,
  Channels,
  Spaces,
  Messages,
}

impl GroupKind {
  fn label_key(self) -> &'static str {
    match self {
      Self::Recent => "switcher-group-recent",
      Self::People => "switcher-group-people",
      Self::Threads => "switcher-group-threads",
      Self::Channels => "switcher-group-channels",
      Self::Spaces => "switcher-group-spaces",
      Self::Messages => "switcher-group-messages",
    }
  }
}

/// One line of the search.
#[derive(Clone, Debug, PartialEq)]
enum SwitcherItem {
  /// Opens (or starts) the DM with the person.
  Person(User),
  Thread {
    thread: ThreadSummary,
    conversation_name: String,
  },
  Conversation {
    conversation: Conversation,
    maybe_space_name: Option<String>,
  },
  Space(Space),
  /// An open space the user isn't in: opening it joins.
  JoinableSpace(JoinableSpace),
  Message(SearchHit),
  /// "All messages for …": the search page with its filters.
  AllMessages(String),
}

#[derive(Clone, Debug, PartialEq)]
struct Group {
  kind: GroupKind,
  items: Vec<SwitcherItem>,
}

/// Everything the search looks through, read from the backend in one go.
struct Sources {
  me: UserId,
  people: Vec<User>,
  threads: Vec<ThreadSummary>,
  conversations: Vec<Conversation>,
  spaces: Vec<Space>,
  joinable_spaces: Vec<JoinableSpace>,
  message_hits: Vec<SearchHit>,
  /// Most recently visited first.
  recent: Vec<ConversationId>,
  maybe_current: Option<ConversationId>,
}

impl Sources {
  fn empty(me: UserId) -> Self {
    Self {
      me,
      people: Vec::new(),
      threads: Vec::new(),
      conversations: Vec::new(),
      spaces: Vec::new(),
      joinable_spaces: Vec::new(),
      message_hits: Vec::new(),
      recent: Vec::new(),
      maybe_current: None,
    }
  }
}

#[component]
pub fn QuickSwitcher() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let query = RwSignal::new(state.switcher_query.get_untracked());
  state.switcher_query.set(String::new());
  let debounced_query = use_debounced(query, DEBOUNCE_MS);
  let selected = RwSignal::new(0_usize);

  // The directory (a server request) and the message index (a scan of the local messages in the
  // browser) only see the text once typing paused; names of what is on screen match on every key.
  let found_people = Memo::new(move |_| {
    let slow_needle = debounced_query.get().trim().to_owned();
    if slow_needle.is_empty() {
      return Vec::new();
    }
    state.read(|backend| backend.search_people(&slow_needle))
  });
  let message_hits = Memo::new(move |_| {
    let slow_needle = debounced_query.get().trim().to_owned();
    if slow_needle.is_empty() {
      return Vec::new();
    }
    state.read(|backend| backend.search(&parse_query(&slow_needle)).hits)
  });
  let groups = Memo::new(move |_| {
    let needle = query.get().trim().to_owned();
    let sources = read_sources(state, &needle, found_people.get(), message_hits.get());
    build_groups(&needle, sources)
  });
  let items = Memo::new(move |_| {
    groups
      .get()
      .into_iter()
      .flat_map(|group| group.items)
      .collect::<Vec<_>>()
  });

  Effect::new(move |_| {
    query.track();
    selected.set(0);
  });

  let on_keydown = move |ev: web_sys::KeyboardEvent| {
    let count = items.with_untracked(Vec::len);
    match ev.key().as_str() {
      "ArrowDown" => {
        ev.prevent_default();
        if count > 0 {
          selected.update(|index| *index = (*index + 1) % count);
        }
      },
      "ArrowUp" => {
        ev.prevent_default();
        if count > 0 {
          selected.update(|index| *index = (*index + count - 1) % count);
        }
      },
      "Enter" => {
        ev.prevent_default();
        // Ctrl/Cmd+Enter opens the message search for the text instead.
        if ev.ctrl_key() || ev.meta_key() {
          state.open_search(Some(query.get_untracked().trim()));
        } else if let Some(item) = items.with_untracked(|list| list.get(selected.get_untracked()).cloned()) {
          open_item(state, &item);
        }
      },
      _ => {},
    }
  };

  view! {
    <Modal labelled_by="quick-switcher-title" class="modal-switcher">
      <h2 id="quick-switcher-title" class="visually-hidden">
        {move || i18n.t("switcher-title")}
      </h2>
      <div class="switcher-input">
        // Phones: the search is a full-screen sheet, so it needs a visible way out.
        <button
          type="button"
          class="icon-btn compact-only"
          aria-label=move || i18n.t("back")
          title=move || i18n.t("back")
          on:click=move |_| state.close_overlay()
        >
          <Icon name=IconName::ChevronLeft />
        </button>
        <Icon name=IconName::Search class="switcher-search-icon" />
        <input
          type="text"
          role="combobox"
          aria-expanded="true"
          aria-controls="switcher-results"
          aria-autocomplete="list"
          aria-activedescendant=move || format!("switcher-opt-{}", selected.get())
          aria-label=move || i18n.t("switcher-input-label")
          placeholder=move || i18n.t("switcher-placeholder")
          autocomplete="off"
          spellcheck="false"
          name="switcher"
          data-autofocus
          prop:value=move || query.get()
          on:input=move |ev| query.set(event_target_value(&ev))
          on:keydown=on_keydown
        />
      </div>
      <ul id="switcher-results" class="switcher-results" role="listbox" aria-label=move || i18n.t("switcher-results")>
        {move || {
          let list = groups.get();
          let has_results = list.iter().any(|group| group.kind != GroupKind::Messages || group.items.len() > 1);
          let mut index = 0_usize;
          let rows = list
            .into_iter()
            .map(|group| {
              let header = i18n.t(group.kind.label_key());
              let rows = group
                .items
                .into_iter()
                .map(|item| {
                  let row = view! { <SwitcherRow item=item index=index selected=selected /> };
                  index += 1;
                  row
                })
                .collect_view();
              view! {
                <li role="presentation" class="switcher-group">{header}</li>
                {rows}
              }
            })
            .collect_view();
          view! {
            {(!has_results).then(|| view! { <li role="presentation" class="switcher-empty">{move || i18n.t("switcher-empty")}</li> })}
            {rows}
          }
        }}
      </ul>
      <footer class="switcher-foot">
        <span class="switcher-hint"><kbd>"↑"</kbd><kbd>"↓"</kbd>{move || i18n.t("switcher-hint-navigate")}</span>
        <span class="switcher-hint"><ShortcutKeys shortcut=ENTER />{move || i18n.t("switcher-hint-open")}</span>
        <button
          type="button"
          class="switcher-hint switcher-hint-action"
          on:click=move |_| state.open_search(Some(query.get_untracked().trim()))
        >
          <ShortcutKeys shortcut=SEARCH_MESSAGES />
          {move || i18n.t("switcher-search-messages")}
        </button>
        <span class="switcher-hint"><ShortcutKeys shortcut=ESCAPE />{move || i18n.t("switcher-hint-close")}</span>
      </footer>
    </Modal>
  }
}

/// One result: icon or avatar, name and what it is (space, person, thread of a channel, …).
#[component]
fn SwitcherRow(item: SwitcherItem, index: usize, selected: RwSignal<usize>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let (leading, name, context, is_unread) = match &item {
    SwitcherItem::Person(user) => {
      let context = if user.device_setup == DeviceSetup::NoDevices {
        i18n.t("directory-no-devices")
      } else {
        i18n.t("switcher-context-person")
      };
      let avatar =
        view! { <Avatar user=user.clone() size=AvatarSize::Small show_presence=true /> }.into_any();
      (avatar, user.display_name.clone(), context, false)
    },
    SwitcherItem::Thread {
      thread,
      conversation_name,
    } => (
      view! { <Icon name=IconName::Threads /> }.into_any(),
      first_line(&thread.root.body),
      i18n.t_with("switcher-context-thread", &[("channel", conversation_name)]),
      thread.unread_replies > 0,
    ),
    SwitcherItem::Conversation {
      conversation,
      maybe_space_name,
    } => {
      let icon = match conversation.kind {
        ConversationKind::Public => IconName::Hash,
        ConversationKind::Private => IconName::Lock,
        ConversationKind::Integration => IconName::Terminal,
        ConversationKind::Direct | ConversationKind::Group => IconName::Users,
      };
      let context = match (conversation.kind, maybe_space_name) {
        (_, Some(space)) => space.clone(),
        (ConversationKind::Direct, None) => i18n.t("switcher-context-direct"),
        _ => i18n.t("switcher-context-group"),
      };
      (
        view! { <Icon name=icon /> }.into_any(),
        conversation.name.clone(),
        context,
        conversation.unread_count > 0,
      )
    },
    SwitcherItem::Space(space) => (
      view! { <SpaceTile space=space.clone() /> }.into_any(),
      space.name.clone(),
      i18n.t("switcher-context-space"),
      space.unread_count > 0,
    ),
    SwitcherItem::JoinableSpace(space) => (
      view! { <Icon name=IconName::Layers /> }.into_any(),
      space.name.clone(),
      i18n.t("switcher-context-join"),
      false,
    ),
    SwitcherItem::Message(hit) => {
      let author = state
        .read(|backend| backend.user(hit.message.author))
        .map(|user| user.display_name)
        .unwrap_or_default();
      (
        view! { <Icon name=IconName::Search /> }.into_any(),
        first_line(&hit.message.body),
        [author, hit.conversation_name.clone()]
          .into_iter()
          .filter(|part| !part.is_empty())
          .collect::<Vec<_>>()
          .join(" · "),
        false,
      )
    },
    SwitcherItem::AllMessages(query) => (
      view! { <Icon name=IconName::Search /> }.into_any(),
      i18n.t_with("switcher-all-messages", &[("query", query)]),
      String::new(),
      false,
    ),
  };
  let is_action = matches!(item, SwitcherItem::AllMessages(_));
  view! {
    <li
      id=format!("switcher-opt-{index}")
      role="option"
      class="switcher-row"
      class:switcher-action=is_action
      aria-selected=move || (selected.get() == index).to_string()
      class:active=move || selected.get() == index
      on:mousemove=move |_| selected.set(index)
      on:click=move |_| open_item(state, &item)
    >
      <span class="switcher-lead" aria-hidden="true">{leading}</span>
      <span class="switcher-name" translate=if is_action { "yes" } else { "no" }>{name}</span>
      {(!context.is_empty()).then(|| view! { <span class="switcher-context" translate="no">{context}</span> })}
      {is_unread.then(|| view! { <span class="unread-dot" aria-hidden="true"></span> })}
    </li>
  }
}

/// Reads what the search looks through: what the backend holds locally, plus the people and
/// messages found for the debounced text.
fn read_sources(state: AppState, needle: &str, people: Vec<User>, message_hits: Vec<SearchHit>) -> Sources {
  let recent = state.settings.recent_conversations.get();
  let maybe_current = state.current_conversation_id();
  let me = state.me().id;
  state.read(|backend| {
    let spaces = backend.spaces();
    let mut conversations: Vec<Conversation> = Vec::new();
    for space in &spaces {
      conversations.extend(backend.space_conversations(space.id));
    }
    conversations.extend(backend.direct_conversations());
    if needle.is_empty() {
      // The people of the recent DMs only, not everybody the backend knows.
      let people = recent
        .iter()
        .filter_map(|id| conversations.iter().find(|conversation| conversation.id == *id))
        .filter(|conversation| conversation.kind == ConversationKind::Direct)
        .filter_map(|conversation| conversation.member_ids.iter().find(|member| **member != me))
        .filter_map(|other| backend.user(*other))
        .collect();
      return Sources {
        me,
        people,
        conversations,
        spaces,
        recent,
        maybe_current,
        ..Sources::empty(me)
      };
    }
    Sources {
      me,
      people,
      threads: backend.followed_threads(),
      conversations,
      joinable_spaces: backend.joinable_spaces(),
      spaces,
      message_hits,
      recent,
      maybe_current,
    }
  })
}

/// The groups for the typed text, in their fixed order, leaving out empty ones. A query always
/// ends with "all messages for …".
fn build_groups(needle: &str, sources: Sources) -> Vec<Group> {
  if needle.is_empty() {
    return recent_group(sources).into_iter().collect();
  }
  let space_name = |id: Option<SpaceId>| {
    id.and_then(|id| sources.spaces.iter().find(|space| space.id == id))
      .map(|space| space.name.clone())
  };
  let conversation_name = |id: ConversationId| {
    sources
      .conversations
      .iter()
      .find(|conversation| conversation.id == id)
      .map(|conversation| conversation.name.clone())
      .unwrap_or_default()
  };
  let people = matching_people(needle, sources.people.clone(), sources.me);
  let threads: Vec<SwitcherItem> = matching_threads(needle, sources.threads.clone())
    .into_iter()
    .map(|thread| SwitcherItem::Thread {
      conversation_name: conversation_name(thread.conversation),
      thread,
    })
    .collect();
  let channels: Vec<SwitcherItem> = matching_channels(
    needle,
    sources.conversations.clone(),
    &sources.recent,
    sources.maybe_current,
  )
  .into_iter()
  .map(|conversation| SwitcherItem::Conversation {
    maybe_space_name: space_name(conversation.maybe_space),
    conversation,
  })
  .collect();
  let spaces = matching_spaces(needle, &sources.spaces, &sources.joinable_spaces);
  let mut messages: Vec<SwitcherItem> = sources
    .message_hits
    .iter()
    .take(MAX_MESSAGES)
    .cloned()
    .map(SwitcherItem::Message)
    .collect();
  messages.push(SwitcherItem::AllMessages(needle.to_owned()));
  [
    (
      GroupKind::People,
      people.into_iter().map(SwitcherItem::Person).collect(),
    ),
    (GroupKind::Threads, threads),
    (GroupKind::Channels, channels),
    (GroupKind::Spaces, spaces),
    (GroupKind::Messages, messages),
  ]
  .into_iter()
  .filter(|(_, items)| !items.is_empty())
  .map(|(kind, items)| Group { kind, items })
  .collect()
}

/// The conversations visited last; a DM shows as its person.
fn recent_group(sources: Sources) -> Option<Group> {
  let items: Vec<SwitcherItem> = sources
    .recent
    .iter()
    .filter(|id| Some(**id) != sources.maybe_current)
    .filter_map(|id| {
      sources
        .conversations
        .iter()
        .find(|conversation| conversation.id == *id)
    })
    .take(MAX_RECENT)
    .map(|conversation| {
      let maybe_person = (conversation.kind == ConversationKind::Direct)
        .then(|| {
          conversation
            .member_ids
            .iter()
            .find(|member| **member != sources.me)
            .and_then(|other| sources.people.iter().find(|user| user.id == *other))
        })
        .flatten();
      match maybe_person {
        Some(person) => SwitcherItem::Person(person.clone()),
        None => SwitcherItem::Conversation {
          maybe_space_name: conversation
            .maybe_space
            .and_then(|id| sources.spaces.iter().find(|space| space.id == id))
            .map(|space| space.name.clone()),
          conversation: conversation.clone(),
        },
      }
    })
    .collect();
  (!items.is_empty()).then_some(Group {
    kind: GroupKind::Recent,
    items,
  })
}

fn open_item(state: AppState, item: &SwitcherItem) {
  match item {
    SwitcherItem::Person(user) => {
      // A new DM opens once the core made it; the search closes now either way.
      state.close_overlay();
      state.open_direct(user.id);
    },
    SwitcherItem::Thread { thread, .. } => {
      state.open_conversation(thread.conversation);
      state.open_thread(thread.conversation, thread_id_of(thread.root.id));
    },
    SwitcherItem::Conversation { conversation, .. } => state.open_conversation(conversation.id),
    SwitcherItem::Space(space) => {
      state.open_space(space.id);
      state.close_overlay();
    },
    SwitcherItem::JoinableSpace(space) => {
      let id = space.id;
      state.write(|backend| backend.join_space(id));
      state.open_space(id);
      state.close_overlay();
    },
    SwitcherItem::Message(hit) => {
      state.open_message(hit.message.conversation, hit.message.id, hit.message.maybe_thread)
    },
    SwitcherItem::AllMessages(query) => state.open_search(Some(query)),
  }
}

/// The people matching the typed text, best first, at most [`MAX_PEOPLE`]: not the user, no bots,
/// each once. Opening one opens the DM, so 1:1 DMs aren't listed as channels.
fn matching_people(needle: &str, people: Vec<User>, me: UserId) -> Vec<User> {
  if needle.trim().is_empty() {
    return Vec::new();
  }
  let mut ranked: Vec<(u32, String, User)> = people
    .into_iter()
    .filter(|user| user.id != me && !user.is_bot)
    .filter_map(|user| {
      let by_name = fuzzy_score(needle, &user.display_name);
      let by_username = fuzzy_score(needle, &user.username);
      let score = by_name.into_iter().chain(by_username).min()?;
      Some((score, user.display_name.to_lowercase(), user))
    })
    .collect();
  ranked.sort_by(|a, b| (a.0, &a.1, a.2.id).cmp(&(b.0, &b.1, b.2.id)));
  let mut seen = std::collections::HashSet::new();
  ranked
    .into_iter()
    .map(|entry| entry.2)
    .filter(|user| seen.insert(user.id))
    .take(MAX_PEOPLE)
    .collect()
}

/// Followed threads whose root message contains the text (decrypted on this device), newest
/// reply first.
fn matching_threads(needle: &str, mut threads: Vec<ThreadSummary>) -> Vec<ThreadSummary> {
  let needle = needle.trim().to_lowercase();
  threads.retain(|thread| thread.root.body.to_lowercase().contains(&needle));
  threads.sort_by_key(|thread| std::cmp::Reverse(thread.last_reply_at));
  threads.truncate(MAX_THREADS);
  threads
}

/// Channels and group DMs by name, recently visited first; the open one goes last.
fn matching_channels(
  needle: &str,
  conversations: Vec<Conversation>,
  recent: &[ConversationId],
  maybe_current: Option<ConversationId>,
) -> Vec<Conversation> {
  let mut ranked: Vec<(u32, usize, bool, String, Conversation)> = conversations
    .into_iter()
    .filter(|conversation| conversation.kind != ConversationKind::Direct)
    .filter_map(|conversation| {
      let score = fuzzy_score(needle, &conversation.name)?;
      let recency = if Some(conversation.id) == maybe_current {
        usize::MAX
      } else {
        recent
          .iter()
          .position(|id| *id == conversation.id)
          .unwrap_or(usize::MAX - 1)
      };
      Some((
        score,
        recency,
        conversation.unread_count == 0,
        conversation.name.to_lowercase(),
        conversation,
      ))
    })
    .collect();
  ranked.sort_by(|a, b| (a.0, a.1, a.2, &a.3).cmp(&(b.0, b.1, b.2, &b.3)));
  ranked
    .into_iter()
    .map(|entry| entry.4)
    .take(MAX_CHANNELS)
    .collect()
}

/// The user's spaces by name, then open spaces they could join.
fn matching_spaces(needle: &str, spaces: &[Space], joinable: &[JoinableSpace]) -> Vec<SwitcherItem> {
  let mut own: Vec<(u32, &Space)> = spaces
    .iter()
    .filter_map(|space| Some((fuzzy_score(needle, &space.name)?, space)))
    .collect();
  own.sort_by_key(|(score, space)| (*score, space.name.to_lowercase()));
  let mut open: Vec<(u32, &JoinableSpace)> = joinable
    .iter()
    .filter(|candidate| !spaces.iter().any(|space| space.id == candidate.id))
    .filter_map(|space| Some((fuzzy_score(needle, &space.name)?, space)))
    .collect();
  open.sort_by_key(|(score, space)| (*score, space.name.to_lowercase()));
  own
    .into_iter()
    .map(|(_, space)| SwitcherItem::Space(space.clone()))
    .chain(
      open
        .into_iter()
        .map(|(_, space)| SwitcherItem::JoinableSpace(space.clone())),
    )
    .take(MAX_SPACES)
    .collect()
}

/// The first line of a message, for a one-line result.
fn first_line(body: &str) -> String {
  body.lines().next().unwrap_or_default().trim().to_owned()
}

#[cfg(test)]
mod tests {
  use crate::model::{Message, MessageKind, PresenceStatus, SidebarCategory, id_from_raw};

  use super::*;

  const ME: u64 = 1;

  #[test]
  fn people_match_by_name_or_username_best_first() {
    let people = vec![
      person(2, "Marja Jansen", "mjansen"),
      person(3, "Jan Kahl", "jkahl"),
      person(4, "Bob", "bob"),
    ];
    let found = matching_people("jan", people, id_from_raw(ME));
    let names: Vec<&str> = found.iter().map(|user| user.display_name.as_str()).collect();
    assert_eq!(names, vec!["Jan Kahl", "Marja Jansen"]);
  }

  #[test]
  fn the_user_bots_and_duplicates_are_left_out() {
    let mut bot = person(5, "Jan Bot", "janbot");
    bot.is_bot = true;
    let people = vec![
      person(ME, "Jan Me", "me"),
      bot,
      person(3, "Jan", "jan"),
      person(3, "Jan", "jan"),
    ];
    let found = matching_people("jan", people, id_from_raw(ME));
    assert_eq!(
      found.iter().map(|user| user.id).collect::<Vec<_>>(),
      vec![id_from_raw(3)]
    );
  }

  #[test]
  fn the_people_list_is_capped() {
    let people: Vec<User> = (2..20).map(|id| person(id, &format!("Jan {id}"), "")).collect();
    assert_eq!(matching_people("jan", people, id_from_raw(ME)).len(), MAX_PEOPLE);
  }

  #[test]
  fn groups_come_in_a_fixed_order_and_end_with_all_messages() {
    let sources = Sources {
      me: id_from_raw(ME),
      people: vec![person(3, "Design Lead", "dlead")],
      threads: vec![thread(20, 10, "Design review on Friday")],
      conversations: vec![conversation(10, ConversationKind::Public, "design", &[])],
      spaces: vec![space(30, "Design")],
      message_hits: vec![hit(40, 10, "the design is ready")],
      ..Sources::empty(id_from_raw(ME))
    };
    let groups = build_groups("design", sources);
    let kinds: Vec<GroupKind> = groups.iter().map(|group| group.kind).collect();
    assert_eq!(
      kinds,
      vec![
        GroupKind::People,
        GroupKind::Threads,
        GroupKind::Channels,
        GroupKind::Spaces,
        GroupKind::Messages
      ]
    );
    let messages = &groups[4].items;
    assert!(matches!(messages[0], SwitcherItem::Message(_)));
    assert_eq!(messages.last(), Some(&SwitcherItem::AllMessages("design".into())));
  }

  #[test]
  fn a_query_without_matches_still_offers_the_message_search() {
    let groups = build_groups("zzz", Sources::empty(id_from_raw(ME)));
    assert_eq!(
      groups,
      vec![Group {
        kind: GroupKind::Messages,
        items: vec![SwitcherItem::AllMessages("zzz".into())],
      }]
    );
  }

  #[test]
  fn one_to_one_dms_are_found_as_their_person_not_as_a_channel() {
    let sources = Sources {
      me: id_from_raw(ME),
      people: vec![person(3, "Jan", "jan")],
      conversations: vec![conversation(10, ConversationKind::Direct, "Jan", &[ME, 3])],
      ..Sources::empty(id_from_raw(ME))
    };
    let groups = build_groups("jan", sources);
    assert_eq!(groups[0].kind, GroupKind::People);
    assert!(groups.iter().all(|group| group.kind != GroupKind::Channels));
  }

  #[test]
  fn nothing_typed_lists_recent_conversations_with_dms_as_people() {
    let sources = Sources {
      me: id_from_raw(ME),
      people: vec![person(3, "Jan", "jan")],
      conversations: vec![
        conversation(10, ConversationKind::Direct, "Jan", &[ME, 3]),
        conversation(11, ConversationKind::Public, "general", &[]),
        conversation(12, ConversationKind::Public, "random", &[]),
      ],
      recent: vec![id_from_raw(12), id_from_raw(10), id_from_raw(11)],
      maybe_current: Some(id_from_raw(12)),
      ..Sources::empty(id_from_raw(ME))
    };
    let groups = build_groups("", sources);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].kind, GroupKind::Recent);
    // The open conversation is where the user already is.
    assert!(matches!(&groups[0].items[0], SwitcherItem::Person(user) if user.id == id_from_raw(3)));
    assert!(
      matches!(&groups[0].items[1], SwitcherItem::Conversation { conversation, .. } if conversation.name == "general")
    );
    assert_eq!(groups[0].items.len(), 2);
  }

  #[test]
  fn threads_match_their_root_message() {
    let threads = vec![thread(20, 10, "Release plan"), thread(21, 10, "Lunch?")];
    let found = matching_threads("release", threads);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].root.body, "Release plan");
  }

  #[test]
  fn own_spaces_come_before_joinable_ones() {
    let joinable = vec![JoinableSpace {
      id: id_from_raw(31),
      name: "Sales team".into(),
      maybe_description: None,
    }];
    let found = matching_spaces("sales", &[space(30, "Sales")], &joinable);
    assert!(matches!(&found[0], SwitcherItem::Space(space) if space.name == "Sales"));
    assert!(matches!(&found[1], SwitcherItem::JoinableSpace(space) if space.name == "Sales team"));
  }

  fn person(id: u64, name: &str, username: &str) -> User {
    User {
      id: id_from_raw(id),
      username: username.into(),
      display_name: name.into(),
      presence: PresenceStatus::Offline,
      maybe_custom_status: None,
      avatar_hue: 0,
      is_guest: false,
      is_bot: false,
      maybe_organization: None,
      device_setup: DeviceSetup::Unknown,
    }
  }

  fn conversation(id: u64, kind: ConversationKind, name: &str, members: &[u64]) -> Conversation {
    Conversation {
      id: id_from_raw(id),
      maybe_space: None,
      kind,
      category: SidebarCategory::Channels,
      name: name.into(),
      topic: String::new(),
      member_ids: members.iter().map(|member| id_from_raw(*member)).collect(),
      unread_count: 0,
      mention_count: 0,
      is_favorite: false,
      is_muted: false,
      can_post: true,
      bookmarks: Vec::new(),
    }
  }

  fn space(id: u64, name: &str) -> Space {
    Space {
      id: id_from_raw(id),
      name: name.into(),
      hue: 0,
      maybe_color: None,
      maybe_image_version: None,
      unread_count: 0,
      mention_count: 0,
    }
  }

  fn message(id: u64, conversation: u64, body: &str) -> Message {
    Message {
      id: id_from_raw(id),
      conversation: id_from_raw(conversation),
      author: id_from_raw(2),
      kind: MessageKind::User,
      sent_at: 0,
      body: body.into(),
      is_edited: false,
      reactions: Vec::new(),
      reply_count: 0,
      maybe_thread: None,
    }
  }

  fn thread(id: u64, conversation: u64, body: &str) -> ThreadSummary {
    ThreadSummary {
      conversation: id_from_raw(conversation),
      root: message(id, conversation, body),
      reply_count: 1,
      unread_replies: 0,
      last_reply_at: 0,
      participant_ids: Vec::new(),
      has_mention: false,
    }
  }

  fn hit(id: u64, conversation: u64, body: &str) -> SearchHit {
    SearchHit {
      message: message(id, conversation, body),
      conversation_name: "design".into(),
      maybe_space_name: None,
      snippet: Vec::new(),
    }
  }
}
