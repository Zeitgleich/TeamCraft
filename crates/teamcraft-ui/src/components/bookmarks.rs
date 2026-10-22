//! Channel bookmarks: links and files under the header, with a dialog to add, edit, reorder and
//! remove them. The list is one encrypted message of the conversation, so every member sees the
//! same bar (core: `MessageContent::Bookmarks`).

use leptos::prelude::*;

use crate::components::icons::{Icon, IconName};
use crate::components::modal::Modal;
use crate::i18n::use_i18n;
use crate::model::{Bookmark, BookmarkKind, Conversation, ConversationId};
use crate::state::{Overlay, use_app_state};

const DEFAULT_EMOJI: &str = "🔖";

/// Only web links open from a bookmark; anything else (`javascript:`, `data:`) is refused (the
/// rule the mobile apps share, teamcraft-client-api `links`).
pub fn is_openable(url: &str) -> bool {
  teamcraft_client_api::links::is_openable_link(url)
}

/// Moves entry `index` by `delta` places, keeping it inside the list.
pub fn move_entry<T>(list: &mut [T], index: usize, delta: isize) {
  let target = index as isize + delta;
  if index < list.len() && target >= 0 && (target as usize) < list.len() {
    list.swap(index, target as usize);
  }
}

#[component]
pub fn BookmarksBar(conversation: ConversationId, info: Memo<Option<Conversation>>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let bookmarks = move || info.get().map(|conv| conv.bookmarks).unwrap_or_default();
  let can_edit = move || info.get().is_some_and(|conv| conv.can_post);
  view! {
    <div class="bookmarks-bar" role="toolbar" aria-label=move || i18n.t("bookmarks-label")>
      <For each=bookmarks key=|bookmark| (bookmark.title.clone(), bookmark.url.clone(), bookmark.maybe_message) let:bookmark>
        {
          let target = bookmark.clone();
          let open = move |_| match target.kind {
            BookmarkKind::Link => {
              if is_openable(&target.url)
                && let Some(window) = web_sys::window()
              {
                drop(window.open_with_url_and_target_and_features(&target.url, "_blank", "noopener,noreferrer"));
              }
            },
            BookmarkKind::File => {
              if let Some(message) = target.maybe_message {
                state.write(|backend| backend.download_file(conversation, message));
              }
            },
          };
          view! {
            <button class="bookmark" title=bookmark.title.clone() on:click=open>
              <span aria-hidden="true">{bookmark.emoji.clone()}</span>
              <Icon name=if bookmark.kind == BookmarkKind::Link { IconName::Link } else { IconName::File } />
              <span class="bookmark-title" translate="no">{bookmark.title.clone()}</span>
            </button>
          }
        }
      </For>
      <Show when=can_edit fallback=|| ()>
        <button class="bookmark bookmark-add" on:click=move |_| state.overlay.set(Overlay::Bookmarks(conversation))>
          <Icon name=IconName::Plus />
          <span>{move || i18n.t("bookmarks-edit")}</span>
        </button>
      </Show>
    </div>
  }
}

/// Edits the bar: rename, change the link, reorder, remove, add.
#[component]
pub fn BookmarksDialog(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let current = state
    .read(|backend| backend.conversation(conversation))
    .map(|conv| conv.bookmarks)
    .unwrap_or_default();
  let list = RwSignal::new(current);
  let error = RwSignal::new(None::<String>);
  let save = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    let entries = list.get_untracked();
    if entries.iter().any(|entry| {
      entry.title.trim().is_empty() || (entry.kind == BookmarkKind::Link && !is_openable(&entry.url))
    }) {
      error.set(Some(i18n.t("bookmarks-invalid")));
      return;
    }
    state.write(|backend| backend.set_bookmarks(conversation, entries));
    state.close_overlay();
  };
  let add = move |_| {
    list.update(|entries| {
      entries.push(Bookmark {
        emoji: DEFAULT_EMOJI.to_owned(),
        title: String::new(),
        kind: BookmarkKind::Link,
        url: "https://".to_owned(),
        maybe_message: None,
      });
    });
  };
  // Rows are rebuilt on every change: the number of bookmarks is small.
  let rows = move || {
    list
      .get()
      .into_iter()
      .enumerate()
      .map(|(index, entry)| {
        let is_link = entry.kind == BookmarkKind::Link;
        view! {
          <li class="bookmark-row">
            <input class="input emoji-input" maxlength="8" aria-label=move || i18n.t("bookmarks-emoji") prop:value=entry.emoji.clone()
              on:change=move |ev| list.update(|entries| entries[index].emoji = event_target_value(&ev)) />
            <input class="input" aria-label=move || i18n.t("bookmarks-title") placeholder=move || i18n.t("bookmarks-title") prop:value=entry.title.clone()
              on:change=move |ev| list.update(|entries| entries[index].title = event_target_value(&ev)) />
            {is_link.then(|| view! {
              <input class="input" type="url" aria-label=move || i18n.t("bookmarks-url") placeholder="https://" prop:value=entry.url.clone()
                on:change=move |ev| list.update(|entries| entries[index].url = event_target_value(&ev)) />
            })}
            <button type="button" class="icon-btn" aria-label=move || i18n.t("bookmarks-up") on:click=move |_| list.update(|entries| move_entry(entries, index, -1))>
              <span aria-hidden="true">"↑"</span>
            </button>
            <button type="button" class="icon-btn" aria-label=move || i18n.t("bookmarks-down") on:click=move |_| list.update(|entries| move_entry(entries, index, 1))>
              <span aria-hidden="true">"↓"</span>
            </button>
            <button type="button" class="icon-btn" aria-label=move || i18n.t("bookmarks-remove") on:click=move |_| list.update(|entries| { if index < entries.len() { entries.remove(index); } })>
              <Icon name=IconName::Trash />
            </button>
          </li>
        }
      })
      .collect_view()
  };
  view! {
    <Modal labelled_by="bookmarks-title-heading">
      <form on:submit=save>
        <header class="modal-head">
          <h2 id="bookmarks-title-heading">{move || i18n.t("bookmarks-edit-title")}</h2>
          <button type="button" class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
            <Icon name=IconName::Close />
          </button>
        </header>
        <div class="modal-body">
          <ul class="bookmark-edit-list">{rows}</ul>
          <button type="button" class="btn" on:click=add>{move || i18n.t("bookmarks-add")}</button>
          {move || error.get().map(|message| view! { <p class="field-error" role="alert">{message}</p> })}
        </div>
        <footer class="modal-foot">
          <span class="grow"></span>
          <button type="button" class="btn" on:click=move |_| state.close_overlay()>{move || i18n.t("cancel")}</button>
          <button type="submit" class="btn btn-primary">{move || i18n.t("save")}</button>
        </footer>
      </form>
    </Modal>
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_web_links_open() {
    assert!(is_openable("https://example.com/a"));
    assert!(!is_openable("javascript:alert(1)"));
    assert!(!is_openable("data:text/html,x"));
  }

  #[test]
  fn entries_move_within_the_list() {
    let mut list = vec!['a', 'b', 'c'];
    move_entry(&mut list, 0, 1);
    assert_eq!(list, ['b', 'a', 'c']);
    move_entry(&mut list, 0, -1);
    move_entry(&mut list, 2, 1);
    assert_eq!(list, ['b', 'a', 'c']);
  }
}
