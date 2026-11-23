//! Spaces and channels chosen by name in the Administration area's settings (issue #958): the chosen
//! ones as pills, a search field, and the matches below it. The form keeps the IDs, so what is sent
//! doesn't change; an ID the console can't name (deleted, or not loaded yet) stays visible as such.

use leptos::prelude::*;

use crate::admin::AdminChannelQuery;
use crate::components::admin_fields::{list_text, parse_list};
use crate::components::icons::{Icon, IconName};
use crate::form_errors::error_id;
use crate::i18n::use_i18n;
use crate::model::{ConversationId, ConversationKind};
use crate::reactive::use_debounced;
use crate::state::use_app_state;

/// Typing asks the server for matching channels after this pause.
const SEARCH_DEBOUNCE_MS: u64 = 250;
/// Matches shown at once; typing more narrows them.
const MAX_MATCHES: usize = 8;

/// What a picker chooses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickKind {
  Spaces,
  /// Public channels: the only ones a new account can be added to without an invitation.
  PublicChannels,
}

/// `value` holds the chosen IDs as the form's list text (one per line), so checking and sending
/// stay those of a list of IDs.
#[component]
pub fn IdPicker(
  id: &'static str,
  label: &'static str,
  kind: PickKind,
  #[prop(into)] value: Signal<String>,
  on_input: Callback<String>,
  #[prop(into, optional)] error: Signal<Option<&'static str>>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let filter = RwSignal::new(String::new());
  let input = NodeRef::<leptos::html::Input>::new();
  let label_id = format!("{id}-label");
  let list_id = format!("{id}-matches");
  let chosen = Memo::new(move |_| parse_list(&value.get()));
  let known = Memo::new(move |_| match kind {
    PickKind::Spaces => state.read(|backend| {
      backend
        .admin_spaces()
        .into_iter()
        .map(|space| PickItem {
          id: space.id.to_string(),
          name: space.name,
          maybe_context: None,
          is_pickable: !space.is_archived,
        })
        .collect::<Vec<_>>()
    }),
    PickKind::PublicChannels => state.read(|backend| {
      backend
        .admin_channels()
        .into_iter()
        .chain(backend.admin_referenced_channels())
        .map(|channel| PickItem {
          id: channel.id.to_string(),
          is_pickable: channel.kind == ConversationKind::Public && !channel.is_archived,
          name: channel.name,
          maybe_context: Some(channel.space_name).filter(|name| !name.is_empty()),
        })
        .collect::<Vec<_>>()
    }),
  });
  if kind == PickKind::PublicChannels {
    use_server_channel_search(filter);
    // Chosen channels beyond the loaded page are read by ID, so they show by name.
    Effect::new(move |_| {
      let ids: Vec<ConversationId> = chosen.get().iter().filter_map(|text| text.parse().ok()).collect();
      if !ids.is_empty() {
        state.read(|backend| backend.admin_resolve_references(ids, Vec::new()));
      }
    });
  }
  let pills = Memo::new(move |_| chosen.with(|chosen| known.with(|known| chosen_items(chosen, known))));
  let found = Memo::new(move |_| {
    chosen.with(|chosen| known.with(|known| filter.with(|text| matches(known, chosen, text, MAX_MATCHES))))
  });
  let is_searching = move || filter.with(|text| !text.trim().is_empty());
  let set_chosen = move |ids: Vec<String>| on_input.run(list_text(&ids));
  let add = move |item_id: String| {
    let mut ids = chosen.get_untracked();
    if !ids.contains(&item_id) {
      ids.push(item_id);
      set_chosen(ids);
    }
    filter.set(String::new());
    if let Some(element) = input.get_untracked() {
      drop(element.focus());
    }
  };
  let remove = move |item_id: String| {
    let mut ids = chosen.get_untracked();
    ids.retain(|other| *other != item_id);
    set_chosen(ids);
  };
  let on_keydown = move |ev: leptos::ev::KeyboardEvent| match ev.key().as_str() {
    // Enter adds the first match instead of submitting the settings form.
    "Enter" => {
      ev.prevent_default();
      if let Some(first) = found.with_untracked(|list| list.first().map(|item| item.id.clone())) {
        add(first);
      }
    },
    "Escape" if is_searching() => {
      ev.prevent_default();
      filter.set(String::new());
    },
    "Backspace" if !is_searching() => {
      let mut ids = chosen.get_untracked();
      if ids.pop().is_some() {
        set_chosen(ids);
      }
    },
    _ => {},
  };
  let (empty_key, placeholder_key, unknown_key, remove_key, no_match_key) = match kind {
    PickKind::Spaces => (
      "settings-picker-spaces-none",
      "settings-picker-spaces-search",
      "settings-picker-space-unknown",
      "settings-picker-remove",
      "settings-picker-spaces-no-match",
    ),
    PickKind::PublicChannels => (
      "settings-picker-channels-none",
      "settings-picker-channels-search",
      "settings-picker-channel-unknown",
      "settings-picker-remove",
      "settings-picker-channels-no-match",
    ),
  };
  let icon = match kind {
    PickKind::Spaces => IconName::Layers,
    PickKind::PublicChannels => IconName::Hash,
  };
  let described = error_id(id);
  let group_label_id = label_id.clone();
  let list_for_input = list_id.clone();

  view! {
    // forms.md: a group of controls carries the error state on its container, which takes focus.
    <div
      class="field id-picker"
      role="group"
      tabindex="-1"
      id=id
      aria-labelledby=group_label_id
      aria-invalid=move || error.get().map(|_| "true")
      aria-describedby=move || error.get().map(|_| described.clone())
    >
      <label id=label_id for=format!("{id}-search")>{move || i18n.t(label)}</label>
      <ul class="token-field" aria-label=move || i18n.t(label)>
        <For each=move || pills.get() key=|(item_id, maybe_item)| (item_id.clone(), maybe_item.is_some()) let:pill>
          {
            let (item_id, maybe_item) = pill;
            let is_unknown = maybe_item.is_none();
            let title = item_id.clone();
            let name = maybe_item.map_or_else(|| i18n.t(unknown_key), |item| item.name);
            let remove_label = {
              let name = name.clone();
              move || i18n.t_with(remove_key, &[("name", &name)])
            };
            view! {
              <li class="token" class:token-unknown=is_unknown title=title>
                <Icon name=icon />
                <span class="token-name" translate="no">{name}</span>
                <button
                  type="button"
                  class="token-remove"
                  aria-label=remove_label.clone()
                  title=remove_label
                  on:click=move |_| remove(item_id.clone())
                >
                  <Icon name=IconName::Close />
                </button>
              </li>
            }
          }
        </For>
        <li class="token-entry">
          <input
            id=format!("{id}-search")
            node_ref=input
            class="token-input"
            type="search"
            role="combobox"
            aria-autocomplete="list"
            aria-expanded=move || is_searching().to_string()
            aria-controls=list_for_input
            autocomplete="off"
            spellcheck="false"
            placeholder=move || i18n.t(placeholder_key)
            prop:value=move || filter.get()
            on:input=move |ev| filter.set(event_target_value(&ev))
            on:keydown=on_keydown
          />
        </li>
      </ul>
      <Show when=move || pills.with(Vec::is_empty) && !is_searching() fallback=|| ()>
        <p class="muted-text small">{move || i18n.t(empty_key)}</p>
      </Show>
      <ul id=list_id class="forward-list id-picker-matches" role="listbox" hidden=move || !is_searching()>
        <For each=move || found.get() key=|item| item.id.clone() let:item>
          {
            let item_id = item.id.clone();
            view! {
              <li
                role="option"
                aria-selected="false"
                class="person-item"
                on:mousedown=|ev| ev.prevent_default()
                on:click=move |_| add(item_id.clone())
              >
                <Icon name=icon />
                <span class="person-name" translate="no">{item.name.clone()}</span>
                {item.maybe_context.clone().map(|context| view! { <span class="muted-text person-handle" translate="no">{context}</span> })}
                <span class="person-check" aria-hidden="true"><Icon name=IconName::Plus /></span>
              </li>
            }
          }
        </For>
        <Show when=move || found.with(Vec::is_empty) fallback=|| ()>
          <li class="empty-state">{move || i18n.t(no_match_key)}</li>
        </Show>
      </ul>
      <p class="field-error" id=error_id(id)>{move || error.get().map(|key| i18n.t(key))}</p>
    </div>
  }
}

/// A space or channel the picker can name.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PickItem {
  id: String,
  name: String,
  /// The space a channel is in.
  maybe_context: Option<String>,
  /// Offered among the matches: not archived, and for channels public. A chosen one that isn't
  /// (archived since) still shows by name.
  is_pickable: bool,
}

/// The console lists channels page by page (issue #1241), so the typed words go to the server's
/// search. The Channels page's own query is put back when the picker goes away.
fn use_server_channel_search(filter: RwSignal<String>) {
  let state = use_app_state();
  let before = state.read(|backend| backend.admin_channel_query());
  let debounced = use_debounced(filter, SEARCH_DEBOUNCE_MS);
  Effect::new(move |previous: Option<()>| {
    let text = debounced.get();
    // The first run only subscribes: an untouched picker leaves the query alone.
    if previous.is_some() {
      state.write(|backend| {
        backend.admin_set_channel_query(AdminChannelQuery {
          text: text.trim().to_owned(),
          ..AdminChannelQuery::default()
        });
      });
    }
  });
  on_cleanup(move || {
    if debounced.try_get_untracked().is_some_and(|text| !text.is_empty()) {
      state.write(|backend| backend.admin_set_channel_query(before));
    }
  });
}

/// The chosen IDs in order, each with what the console knows of it.
fn chosen_items(chosen: &[String], known: &[PickItem]) -> Vec<(String, Option<PickItem>)> {
  chosen
    .iter()
    .map(|id| (id.clone(), known.iter().find(|item| item.id == *id).cloned()))
    .collect()
}

/// The known items not chosen yet whose name (or space) holds every typed word, ignoring case;
/// names that start with the words first.
fn matches(known: &[PickItem], chosen: &[String], typed: &str, limit: usize) -> Vec<PickItem> {
  let typed = typed.trim().to_lowercase();
  if typed.is_empty() {
    return Vec::new();
  }
  let words: Vec<&str> = typed.split_whitespace().collect();
  let mut found: Vec<&PickItem> = known
    .iter()
    .filter(|item| item.is_pickable && !chosen.contains(&item.id))
    .filter(|item| {
      let haystack = format!(
        "{} {}",
        item.name.to_lowercase(),
        item.maybe_context.as_deref().unwrap_or_default().to_lowercase()
      );
      words.iter().all(|word| haystack.contains(word))
    })
    .collect();
  found.sort_by_key(|item| {
    (
      !item.name.to_lowercase().starts_with(&typed),
      item.name.to_lowercase(),
    )
  });
  found.into_iter().take(limit).cloned().collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn nothing_is_offered_before_typing() {
    assert!(matches(&items(), &[], "  ", MAX_MATCHES).is_empty());
  }

  #[test]
  fn every_word_must_match_the_name_or_space_and_chosen_ones_are_left_out() {
    let found = matches(&items(), &[], "general", MAX_MATCHES);
    assert_eq!(names(&found), ["general", "Team general"]);
    let found = matches(&items(), &["1".to_owned()], "GENERAL", MAX_MATCHES);
    assert_eq!(names(&found), ["Team general"]);
    let found = matches(&items(), &[], "space b gen", MAX_MATCHES);
    assert_eq!(names(&found), ["Team general"]);
  }

  #[test]
  fn names_that_start_with_the_text_come_first_and_the_list_is_bounded() {
    let found = matches(&items(), &[], "r", 2);
    assert_eq!(names(&found), ["random", "general"]);
  }

  #[test]
  fn an_id_the_console_cannot_name_stays_as_unknown() {
    let pills = chosen_items(&["2".to_owned(), "99".to_owned()], &items());
    assert_eq!(
      pills[0].1.as_ref().map(|item| item.name.as_str()),
      Some("Team general")
    );
    assert_eq!(pills[1], ("99".to_owned(), None));
    // Archived since it was chosen: not offered again, but still named.
    let pills = chosen_items(&["4".to_owned()], &items());
    assert_eq!(
      pills[0].1.as_ref().map(|item| item.name.as_str()),
      Some("general archive")
    );
  }

  fn items() -> Vec<PickItem> {
    vec![
      item("1", "general", "Space A"),
      item("2", "Team general", "Space B"),
      item("3", "random", "Space A"),
      PickItem {
        is_pickable: false,
        ..item("4", "general archive", "Space A")
      },
    ]
  }

  fn item(id: &str, name: &str, space: &str) -> PickItem {
    PickItem {
      id: id.to_owned(),
      name: name.to_owned(),
      maybe_context: Some(space.to_owned()),
      is_pickable: true,
    }
  }

  fn names(items: &[PickItem]) -> Vec<&str> {
    items.iter().map(|item| item.name.as_str()).collect()
  }
}
