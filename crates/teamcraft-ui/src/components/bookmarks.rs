//! Channel bookmarks: links and files under the header, with a dialog to add, edit, reorder and
//! remove them. The list is one encrypted message of the conversation, so every member sees the
//! same bar (core: `MessageContent::Bookmarks`).

use leptos::prelude::*;

use crate::channel_access::ChannelAccess;
use crate::components::icons::{Icon, IconName};
use crate::components::modal::Modal;
use crate::form_errors::{FieldError, FieldErrors, FormErrors};
use crate::i18n::use_i18n;
use crate::model::{Bookmark, BookmarkKind, Conversation, ConversationId};
use crate::state::{Overlay, use_app_state};

const BOOKMARKS_FORM_ID: &str = "bookmarks-form";

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

/// The fields of the edited list that can't be saved, in the order of the rows: each with the i18n
/// key of what to do (forms.md: every error at its field).
fn invalid_fields(entries: &[Bookmark]) -> Vec<(String, &'static str)> {
  let mut found = Vec::new();
  for (index, entry) in entries.iter().enumerate() {
    if entry.title.trim().is_empty() {
      found.push((title_field(index), "bookmarks-error-title"));
    }
    if entry.kind == BookmarkKind::Link && !is_openable(&entry.url) {
      found.push((url_field(index), "bookmarks-error-url"));
    }
  }
  found
}

fn title_field(index: usize) -> String {
  format!("bookmark-title-{index}")
}

fn url_field(index: usize) -> String {
  format!("bookmark-url-{index}")
}

#[component]
pub fn BookmarksBar(conversation: ConversationId, info: Memo<Option<Conversation>>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let bookmarks = move || info.get().map(|conv| conv.bookmarks).unwrap_or_default();
  // The bar is a message (posting) and changing it takes `manage_bookmarks` (guests have none).
  let access = ChannelAccess::new(state, conversation);
  let can_edit = move || access.message_permissions().can_manage_bookmarks;
  view! {
    <div class="bookmarks-bar" role="toolbar" aria-label=move || i18n.t("bookmarks-label")>
      <For each=bookmarks key=|bookmark| (bookmark.title.clone(), bookmark.url.clone(), bookmark.maybe_message) let:bookmark>
        {
          let target = bookmark.clone();
          let open = move |_| match target.kind {
            BookmarkKind::Link => {
              if is_openable(&target.url) {
                crate::external_links::open(&target.url);
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
              {(!bookmark.emoji.is_empty()).then(|| view! { <span aria-hidden="true">{bookmark.emoji.clone()}</span> })}
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
  let errors = FormErrors::new();
  let field_errors = move || {
    let mut found = FieldErrors::new();
    for (field_id, key) in list.with(|entries| invalid_fields(entries)) {
      found.push(field_id, i18n.t(key));
    }
    found
  };
  Effect::new(move |_| errors.follow(field_errors()));
  let save = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    let found = untrack(field_errors);
    if !found.is_empty() {
      errors.refuse(BOOKMARKS_FORM_ID, found);
      return;
    }
    let entries = list.get_untracked();
    state.write(|backend| backend.set_bookmarks(conversation, entries));
    state.close_overlay();
  };
  let add = move |_| {
    list.update(|entries| {
      entries.push(Bookmark {
        // No emoji by default: the bar shows the link or file icon, an emoji is the user's choice.
        emoji: String::new(),
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
        let title_id = title_field(index);
        let url_id = url_field(index);
        view! {
          <li>
            <div class="bookmark-row">
              <input class="input emoji-input" maxlength="8" aria-label=move || i18n.t("bookmarks-emoji") prop:value=entry.emoji.clone()
                on:change=move |ev| list.update(|entries| entries[index].emoji = event_target_value(&ev)) />
              <input id=title_id.clone() class="input" aria-label=move || i18n.t("bookmarks-title")
                aria-invalid=errors.invalid(title_id.clone()) aria-describedby=errors.described(title_id.clone()) placeholder=move || i18n.t("bookmarks-title") prop:value=entry.title.clone()
                on:change=move |ev| list.update(|entries| entries[index].title = event_target_value(&ev)) />
              {is_link.then(|| view! {
                <input id=url_id.clone() class="input" type="url" aria-label=move || i18n.t("bookmarks-url")
                  aria-invalid=errors.invalid(url_id.clone()) aria-describedby=errors.described(url_id.clone()) placeholder="https://" prop:value=entry.url.clone()
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
            </div>
            <FieldError errors=errors field=title_id.clone() />
            <FieldError errors=errors field=url_id.clone() />
          </li>
        }
      })
      .collect_view()
  };
  view! {
    <Modal labelled_by="bookmarks-title-heading">
      <form id=BOOKMARKS_FORM_ID on:submit=save>
        <header class="modal-head">
          <h2 id="bookmarks-title-heading">{move || i18n.t("bookmarks-edit-title")}</h2>
          <button type="button" class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
            <Icon name=IconName::Close />
          </button>
        </header>
        <div class="modal-body">
          <ul class="bookmark-edit-list">{rows}</ul>
          <button type="button" class="btn" on:click=add>{move || i18n.t("bookmarks-add")}</button>
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
  fn each_invalid_field_of_each_row_is_named() {
    let entries = [
      bookmark("Docs", "https://example.com"),
      bookmark(" ", "https://example.com"),
      bookmark("Wiki", "javascript:alert(1)"),
    ];
    assert_eq!(
      invalid_fields(&entries),
      [
        ("bookmark-title-1".to_owned(), "bookmarks-error-title"),
        ("bookmark-url-2".to_owned(), "bookmarks-error-url"),
      ]
    );
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

  fn bookmark(title: &str, url: &str) -> Bookmark {
    Bookmark {
      emoji: String::new(),
      title: title.to_owned(),
      kind: BookmarkKind::Link,
      url: url.to_owned(),
      maybe_message: None,
    }
  }
}
