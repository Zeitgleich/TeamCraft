//! What a user can do with a message: the "more" menu (quote, forward, pin, save, copy link, mark
//! unread, edit, delete), the forward dialog, the saved-items page and the pinned-messages panel.
//! The data and the actions come from the client core through the chat backend (`message_extras`,
//! `pinned_messages`, `saved_items`, ...).

use leptos::prelude::*;
use teamcraft_client_api::actions::MessageExtras;
use wasm_bindgen::JsCast;

use crate::clock::format_date_time;
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::icons::{Icon, IconName};
use crate::components::modal::Modal;
use crate::components::rich_text::RichText;
use crate::i18n::use_i18n;
use crate::model::{ConversationId, Message, MessageId, User};
use crate::state::{Overlay, ReplyTarget, SidePanel, use_app_state};

/// How many characters of a quoted message the composer shows.
const EXCERPT_CHARS: usize = 120;

pub fn excerpt(body: &str) -> String {
  let single_line = body.split_whitespace().collect::<Vec<_>>().join(" ");
  if single_line.chars().count() <= EXCERPT_CHARS {
    single_line
  } else {
    format!("{}…", single_line.chars().take(EXCERPT_CHARS).collect::<String>())
  }
}

/// The ⋯ button of a message and its menu.
#[component]
pub fn MessageMenu(
  message: Message,
  #[prop(into)] extras: Signal<Option<MessageExtras>>,
  is_mine: bool,
  in_thread: bool,
  on_edit: Callback<()>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let is_open = RwSignal::new(false);
  let conversation = message.conversation;
  let id = message.id;
  let thread_root = message
    .maybe_thread
    .unwrap_or_else(|| crate::model::thread_id_of(message.id));
  let author_name = state
    .read(|backend| backend.user(message.author))
    .map(|user| user.display_name)
    .unwrap_or_default();
  let body = StoredValue::new(message.body.clone());
  let author_name = StoredValue::new(author_name);
  let is_pinned = move || extras.get().is_some_and(|extras| extras.is_pinned);
  let is_saved = move || extras.get().is_some_and(|extras| extras.is_saved);
  let close = move || {
    is_open.set(false);
  };

  // Opening the menu starts fetching the permalink so that "Copy link" has it by the time it is used.
  let toggle = move |_| {
    let opening = !is_open.get_untracked();
    is_open.set(opening);
    if opening {
      state.read(|backend| drop(backend.permalink(conversation, id)));
    }
  };
  let copy_link = move |_| {
    if let Some(link) = state.read(|backend| backend.permalink(conversation, id)) {
      copy_to_clipboard(&link);
    }
    close();
  };

  view! {
    <div class="msg-menu-wrap">
      <button
        class="icon-btn"
        aria-haspopup="menu"
        aria-expanded=move || is_open.get().to_string()
        aria-label=move || i18n.t("message-more")
        title=move || i18n.t("message-more")
        on:click=toggle
      >
        <Icon name=IconName::More />
      </button>
      <Show when=move || is_open.get() fallback=|| ()>
        <div class="popover-backdrop" on:mousedown=move |_| close()></div>
        <div class="popover msg-menu" role="menu" aria-label=move || i18n.t("message-more")>
          {(!in_thread)
            .then(|| {
              view! {
                <button class="menu-item" role="menuitem" on:click=move |_| { state.open_thread(conversation, thread_root); close(); }>
                  <Icon name=IconName::Reply />
                  <span class="menu-label">{move || i18n.t("action-reply")}</span>
                </button>
              }
            })}
          <button
            class="menu-item"
            role="menuitem"
            on:click=move |_| {
              state.reply_target.set(Some(ReplyTarget {
                conversation,
                message: id,
                author_name: author_name.get_value(),
                excerpt: excerpt(&body.get_value()),
              }));
              close();
            }
          >
            <Icon name=IconName::Link />
            <span class="menu-label">{move || i18n.t("action-quote")}</span>
          </button>
          <button class="menu-item" role="menuitem" on:click=move |_| { state.overlay.set(Overlay::Forward(conversation, id)); close(); }>
            <Icon name=IconName::Send />
            <span class="menu-label">{move || i18n.t("action-forward")}</span>
          </button>
          <button
            class="menu-item"
            role="menuitem"
            on:click=move |_| {
              let pin = !is_pinned();
              state.write(|backend| backend.set_pinned(conversation, id, pin));
              close();
            }
          >
            <Icon name=IconName::Star />
            <span class="menu-label">{move || if is_pinned() { i18n.t("action-unpin") } else { i18n.t("action-pin") }}</span>
          </button>
          <button
            class="menu-item"
            role="menuitem"
            on:click=move |_| {
              let save = !is_saved();
              state.write(|backend| backend.set_saved(conversation, id, save));
              close();
            }
          >
            <Icon name=IconName::Bookmark />
            <span class="menu-label">{move || if is_saved() { i18n.t("action-unsave") } else { i18n.t("action-save") }}</span>
          </button>
          <Show when=move || extras.get().is_some_and(|extras| !extras.files.is_empty()) fallback=|| ()>
            <button
              class="menu-item"
              role="menuitem"
              on:click=move |_| {
                let Some(file) = extras.get_untracked().and_then(|extras| extras.files.into_iter().next()) else { return };
                let mut bookmarks = state
                  .read(|backend| backend.conversation(conversation))
                  .map(|conv| conv.bookmarks)
                  .unwrap_or_default();
                bookmarks.push(crate::model::Bookmark {
                  emoji: "📎".to_owned(),
                  title: file.file_name,
                  kind: crate::model::BookmarkKind::File,
                  url: String::new(),
                  maybe_message: Some(id),
                });
                state.write(|backend| backend.set_bookmarks(conversation, bookmarks));
                close();
              }
            >
              <Icon name=IconName::Bookmark />
              <span class="menu-label">{move || i18n.t("action-bookmark-file")}</span>
            </button>
          </Show>
          <button class="menu-item" role="menuitem" on:click=copy_link>
            <Icon name=IconName::Link />
            <span class="menu-label">{move || i18n.t("action-copy-link")}</span>
          </button>
          <button
            class="menu-item"
            role="menuitem"
            on:click=move |_| {
              state.write(|backend| backend.mark_unread(conversation, id));
              close();
            }
          >
            <Icon name=IconName::EyeOff />
            <span class="menu-label">{move || i18n.t("action-mark-unread")}</span>
          </button>
          {is_mine
            .then(|| {
              view! {
                <div class="menu-sep" role="separator"></div>
                <button class="menu-item" role="menuitem" on:click=move |_| { on_edit.run(()); close(); }>
                  <Icon name=IconName::Sliders />
                  <span class="menu-label">{move || i18n.t("action-edit")}</span>
                </button>
                <button
                  class="menu-item menu-danger"
                  role="menuitem"
                  on:click=move |_| {
                    state.delete_with_undo(conversation, id, i18n.t("message-deleted-toast"));
                    close();
                  }
                >
                  <Icon name=IconName::Trash />
                  <span class="menu-label">{move || i18n.t("action-delete")}</span>
                </button>
              }
            })}
        </div>
      </Show>
    </div>
  }
}

/// Writes to the clipboard through the page's `navigator.clipboard` (web-sys keeps that API unstable).
pub(crate) fn copy_to_clipboard(text: &str) {
  let Some(window) = web_sys::window() else { return };
  let navigator = window.navigator();
  let Ok(clipboard) = js_sys::Reflect::get(&navigator, &"clipboard".into()) else {
    return;
  };
  let Ok(write_text) = js_sys::Reflect::get(&clipboard, &"writeText".into()) else {
    return;
  };
  let Ok(write_text) = write_text.dyn_into::<js_sys::Function>() else {
    return;
  };
  // A refused write (no permission, insecure context) leaves the clipboard as it was.
  drop(write_text.call1(&clipboard, &text.into()));
}

/// All versions of an edited message, newest first, with the changed words marked.
#[component]
pub fn EditHistoryDialog(conversation: ConversationId, message: MessageId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let locale = i18n.locale().tag();
  let current = state
    .read(|backend| backend.thread(conversation, crate::model::thread_id_of(message)))
    .into_iter()
    .find(|candidate| candidate.id == message)
    .map(|found| found.body)
    .or_else(|| {
      state
        .read(|backend| backend.messages(conversation, 0, usize::MAX))
        .into_iter()
        .find(|candidate| candidate.id == message)
        .map(|found| found.body)
    })
    .unwrap_or_default();
  let edits = state
    .read(|backend| backend.message_extras(conversation, &[message]))
    .into_iter()
    .find(|extras| extras.message == message)
    .map(|extras| extras.edits)
    .unwrap_or_default();
  // Versions oldest to newest: every earlier body, then the current one.
  let mut versions: Vec<(Option<i64>, String)> = edits
    .iter()
    .map(|edit| (Some(edit.replaced_at), edit.body.clone()))
    .collect();
  versions.push((None, current));
  let rows: Vec<_> = versions
    .iter()
    .enumerate()
    .map(|(index, (replaced_at, body))| {
      let before = if index == 0 {
        ""
      } else {
        versions[index - 1].1.as_str()
      };
      (*replaced_at, crate::diff::word_diff(before, body), index == 0)
    })
    .rev()
    .collect();
  view! {
    <Modal labelled_by="history-title">
      <header class="modal-head">
        <h2 id="history-title">{move || i18n.t("history-title")}</h2>
        <button type="button" class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
          <Icon name=IconName::Close />
        </button>
      </header>
      <ol class="modal-body history-versions">
        {rows
          .into_iter()
          .map(|(replaced_at, diff, is_first)| {
            let label = match replaced_at {
              None => i18n.t("history-current"),
              Some(at) => i18n.t_with("history-replaced", &[("time", &format_date_time(at, locale))]),
            };
            view! {
              <li>
                <h3>{label}</h3>
                <p translate="no">
                  {diff
                    .into_iter()
                    .filter(|(change, _)| *change != crate::diff::Change::Removed || !is_first)
                    .map(|(change, word)| {
                      let class = match change {
                        crate::diff::Change::Same => "",
                        crate::diff::Change::Added => "diff-added",
                        crate::diff::Change::Removed => "diff-removed",
                      };
                      view! { <span class=class>{word}</span>" " }
                    })
                    .collect_view()}
                </p>
              </li>
            }
          })
          .collect_view()}
      </ol>
    </Modal>
  }
}

/// Pick a conversation, add a comment, forward.
#[component]
pub fn ForwardDialog(from: ConversationId, message: MessageId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let filter = RwSignal::new(String::new());
  let comment = RwSignal::new(String::new());
  let chosen = RwSignal::new(None::<ConversationId>);
  let targets = Memo::new(move |_| {
    let needle = filter.get().trim().to_lowercase();
    state.read(|backend| {
      let mut all = Vec::new();
      for space in backend.spaces() {
        all.extend(backend.space_conversations(space.id));
      }
      all.extend(backend.direct_conversations());
      all.retain(|conversation| {
        conversation.can_post && (needle.is_empty() || conversation.name.to_lowercase().contains(&needle))
      });
      all
    })
  });
  let send = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    let Some(to) = chosen.get_untracked() else { return };
    let note = comment.get_untracked();
    state.write(|backend| backend.forward_message(from, message, to, &note));
    state.close_overlay();
  };
  view! {
    <Modal labelled_by="forward-title" class="modal-small">
      <form on:submit=send>
        <header class="modal-head">
          <h2 id="forward-title">{move || i18n.t("forward-title")}</h2>
          <button type="button" class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
            <Icon name=IconName::Close />
          </button>
        </header>
        <div class="modal-body">
          <label class="visually-hidden" for="forward-filter">{move || i18n.t("forward-filter")}</label>
          <input
            id="forward-filter"
            class="input"
            data-autofocus
            placeholder=move || i18n.t("forward-filter")
            prop:value=move || filter.get()
            on:input=move |ev| filter.set(event_target_value(&ev))
          />
          <ul class="forward-list" role="listbox" aria-label=move || i18n.t("forward-title")>
            <For each=move || targets.get() key=|conversation| conversation.id let:conversation>
              {
                let id = conversation.id;
                view! {
                  <li role="option" aria-selected=move || (chosen.get() == Some(id)).to_string()>
                    <button
                      type="button"
                      class="forward-item"
                      class:current=move || chosen.get() == Some(id)
                      on:click=move |_| chosen.set(Some(id))
                    >
                      <span translate="no">{conversation.name.clone()}</span>
                    </button>
                  </li>
                }
              }
            </For>
          </ul>
          <label for="forward-comment">{move || i18n.t("forward-comment")}</label>
          <textarea
            id="forward-comment"
            class="input"
            rows="2"
            prop:value=move || comment.get()
            on:input=move |ev| comment.set(event_target_value(&ev))
          ></textarea>
        </div>
        <footer class="modal-foot">
          <span class="grow"></span>
          <button type="button" class="btn" on:click=move |_| state.close_overlay()>{move || i18n.t("cancel")}</button>
          <button type="submit" class="btn btn-primary" prop:disabled=move || chosen.get().is_none()>
            {move || i18n.t("forward-send")}
          </button>
        </footer>
      </form>
    </Modal>
  }
}

/// Everything the user saved, newest first.
#[component]
pub fn SavedView() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let locale = i18n.locale().tag();
  let items = Memo::new(move |_| {
    let mut items = state.read(|backend| backend.saved_items());
    items.sort_by_key(|item| std::cmp::Reverse(item.saved_at));
    items
  });
  view! {
    <section class="saved-view" aria-label=move || i18n.t("saved-title")>
      <header class="panel-header">
        <div class="panel-title">
          <h1>{move || i18n.t("saved-title")}</h1>
        </div>
      </header>
      <ul class="saved-list panel-scroll">
        <For each=move || items.get() key=|item| (item.message.id, item.saved_at) let:item>
          {
            let message = item.message.clone();
            let (conversation, id) = (message.conversation, message.id);
            let author: Option<User> = state.read(|backend| backend.user(message.author));
            view! {
              <li class="saved-item">
                <div class="saved-meta">
                  {author.map(|user| view! { <Avatar user=user size=AvatarSize::Small /> })}
                  <span translate="no">{item.conversation_name.clone()}</span>
                  <time>{format_date_time(message.sent_at, locale)}</time>
                </div>
                <div class="saved-body"><RichText body=message.body.clone() me=state.me().username /></div>
                <div class="saved-actions">
                  <button class="btn btn-small" on:click=move |_| state.open_conversation(conversation)>
                    {move || i18n.t("saved-open")}
                  </button>
                  <button class="btn btn-small" on:click=move |_| state.write(|backend| backend.set_saved(conversation, id, false))>
                    {move || i18n.t("action-unsave")}
                  </button>
                </div>
              </li>
            }
          }
        </For>
        <Show when=move || items.get().is_empty() fallback=|| ()>
          <li class="empty-state">{move || i18n.t("saved-empty")}</li>
        </Show>
      </ul>
    </section>
  }
}

/// The pinned messages of the open conversation.
#[component]
pub fn PinsPanel(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let locale = i18n.locale().tag();
  let pinned = Memo::new(move |_| state.read(|backend| backend.pinned_messages(conversation)));
  view! {
    <aside class="side-panel" aria-label=move || i18n.t("pins-title")>
      <header class="panel-header">
        <div class="panel-title"><h2>{move || i18n.t("pins-title")}</h2></div>
        <button class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.panel.set(SidePanel::Closed)>
          <Icon name=IconName::Close />
        </button>
      </header>
      <ul class="panel-scroll saved-list">
        <For each=move || pinned.get() key=|message| message.id let:message>
          {
            let id = message.id;
            let author = state.read(|backend| backend.user(message.author)).map(|user| user.display_name).unwrap_or_default();
            view! {
              <li class="saved-item">
                <div class="saved-meta">
                  <span translate="no">{author}</span>
                  <time>{format_date_time(message.sent_at, locale)}</time>
                </div>
                <div class="saved-body"><RichText body=message.body.clone() me=state.me().username /></div>
                <button class="btn btn-small" on:click=move |_| state.write(|backend| backend.set_pinned(conversation, id, false))>
                  {move || i18n.t("action-unpin")}
                </button>
              </li>
            }
          }
        </For>
        <Show when=move || pinned.get().is_empty() fallback=|| ()>
          <li class="empty-state">{move || i18n.t("pins-empty")}</li>
        </Show>
      </ul>
    </aside>
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn excerpts_are_one_line_and_bounded() {
    assert_eq!(excerpt("a\n\n b"), "a b");
    assert_eq!(excerpt(&"x".repeat(500)).chars().count(), EXCERPT_CHARS + 1);
  }
}
