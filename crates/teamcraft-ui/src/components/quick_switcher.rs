use leptos::prelude::*;

use crate::components::icons::{Icon, IconName};
use crate::components::modal::Modal;
use crate::i18n::use_i18n;
use crate::keys::{ENTER, ESCAPE, SEARCH_MESSAGES, ShortcutKeys};
use crate::model::{Conversation, ConversationKind};
use crate::navigation::fuzzy_score;
use crate::state::use_app_state;

const MAX_RESULTS: usize = 12;

/// Ctrl/Cmd+K: jump to any conversation by typing some letters of its name; recently visited ones
/// come first.
#[component]
pub fn QuickSwitcher() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let query = RwSignal::new(String::new());
  let selected = RwSignal::new(0_usize);

  let results = Memo::new(move |_| {
    let needle = query.get().trim().to_lowercase();
    let (list, spaces) = state.read(|backend| {
      let spaces = backend.spaces();
      let mut list: Vec<Conversation> = Vec::new();
      for space in &spaces {
        list.extend(backend.space_conversations(space.id));
      }
      list.extend(backend.direct_conversations());
      (list, spaces)
    });
    let recent = state.settings.recent_conversations.get();
    let current = state.current_conversation_id();
    let mut ranked: Vec<(u32, usize, bool, String, Conversation)> = list
      .into_iter()
      .filter_map(|conv| {
        let score = fuzzy_score(&needle, &conv.name)?;
        // Recently visited first; the open conversation is where the user already is, so it goes last.
        let recency = if Some(conv.id) == current {
          usize::MAX
        } else {
          recent
            .iter()
            .position(|id| *id == conv.id)
            .unwrap_or(usize::MAX - 1)
        };
        Some((
          score,
          recency,
          conv.unread_count == 0,
          conv.name.to_lowercase(),
          conv,
        ))
      })
      .collect();
    ranked.sort_by(|a, b| (a.0, a.1, a.2, &a.3).cmp(&(b.0, b.1, b.2, &b.3)));
    let mut list: Vec<Conversation> = ranked.into_iter().map(|entry| entry.4).collect();
    list.truncate(MAX_RESULTS);
    list
      .into_iter()
      .map(|conv| {
        let space_name = conv
          .maybe_space
          .and_then(|id| spaces.iter().find(|space| space.id == id))
          .map(|space| space.name.clone());
        (conv, space_name)
      })
      .collect::<Vec<_>>()
  });

  Effect::new(move |_| {
    query.track();
    selected.set(0);
  });

  let open_selected = move || {
    let list = results.get_untracked();
    if let Some((conv, _)) = list.get(selected.get_untracked()) {
      state.open_conversation(conv.id);
    }
  };

  let on_keydown = move |ev: web_sys::KeyboardEvent| {
    let count = results.with_untracked(Vec::len);
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
        // Ctrl/Cmd+Enter searches messages for the text instead of jumping to a conversation.
        if ev.ctrl_key() || ev.meta_key() {
          state.open_search(Some(query.get_untracked().trim()));
        } else {
          open_selected();
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
        <Icon name=IconName::Search />
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
          let list = results.get();
          if list.is_empty() {
            return view! { <li class="switcher-empty">{move || i18n.t("switcher-empty")}</li> }.into_any();
          }
          list
            .into_iter()
            .enumerate()
            .map(|(index, (conv, space_name))| {
              let id = conv.id;
              let icon = match conv.kind {
                ConversationKind::Public => IconName::Hash,
                ConversationKind::Private => IconName::Lock,
                ConversationKind::Integration => IconName::Terminal,
                ConversationKind::Direct | ConversationKind::Group => IconName::Users,
              };
              view! {
                <li
                  id=format!("switcher-opt-{index}")
                  role="option"
                  aria-selected=move || (selected.get() == index).to_string()
                  class:active=move || selected.get() == index
                  on:mousemove=move |_| selected.set(index)
                  on:click=move |_| state.open_conversation(id)
                >
                  <Icon name=icon />
                  <span class="switcher-name" translate="no">{conv.name.clone()}</span>
                  {space_name.map(|name| view! { <span class="switcher-space" translate="no">{name}</span> })}
                  {(conv.unread_count > 0).then(|| view! { <span class="unread-dot" aria-hidden="true"></span> })}
                </li>
              }
            })
            .collect_view()
            .into_any()
        }}
      </ul>
      <footer class="switcher-foot">
        <span><kbd>"↑"</kbd><kbd>"↓"</kbd>{move || i18n.t("switcher-hint-navigate")}</span>
        <span><ShortcutKeys shortcut=ENTER />{move || i18n.t("switcher-hint-open")}</span>
        <button
          class="link-btn"
          on:click=move |_| state.open_search(Some(query.get_untracked().trim()))
        >
          <ShortcutKeys shortcut=SEARCH_MESSAGES />
          {move || i18n.t("switcher-search-messages")}
        </button>
        <span><ShortcutKeys shortcut=ESCAPE />{move || i18n.t("switcher-hint-close")}</span>
      </footer>
    </Modal>
  }
}
