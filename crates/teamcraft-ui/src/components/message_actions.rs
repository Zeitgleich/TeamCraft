//! What a user can do with a message: the actions of the toolbar's "more" menu and of the touch
//! action sheet (quote, forward, pin, save, copy link and text, mark unread, edit, delete), the
//! forward dialog, the saved-items page and the pinned-messages panel.
//! The data and the actions come from the client core through the chat backend (`message_extras`,
//! `pinned_messages`, `saved_items`, ...).

use leptos::prelude::*;
use teamcraft_client_api::actions::MessageExtras;
use wasm_bindgen::JsCast;

use crate::clock::format_date_time;
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::compact_back::CompactBack;
use crate::components::empty_state::EmptyState;
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

/// Something the user can do with a message: the entries of the toolbar's "more" menu and of the
/// touch action sheet, which [`message_actions`] lists for a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageAction {
  Thread,
  Quote,
  Forward,
  Save,
  Unsave,
  Pin,
  Unpin,
  BookmarkFile,
  CopyLink,
  CopyText,
  MarkUnread,
  Edit,
  Delete,
}

/// Where the actions are listed: the "more" menu leaves out what the toolbar shows as buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionSurface {
  MoreMenu,
  Sheet,
}

/// What decides which actions a message offers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActionContext {
  pub is_mine: bool,
  pub in_thread: bool,
  pub is_pinned: bool,
  pub is_saved: bool,
  pub has_files: bool,
}

impl ActionContext {
  pub fn new(is_mine: bool, in_thread: bool, maybe_extras: Option<&MessageExtras>) -> Self {
    Self {
      is_mine,
      in_thread,
      is_pinned: maybe_extras.is_some_and(|extras| extras.is_pinned),
      is_saved: maybe_extras.is_some_and(|extras| extras.is_saved),
      has_files: maybe_extras.is_some_and(|extras| !extras.files.is_empty()),
    }
  }
}

/// The actions of a message, in menu order; edit and delete (own messages) come last.
pub fn message_actions(context: ActionContext, surface: ActionSurface) -> Vec<MessageAction> {
  let is_sheet = surface == ActionSurface::Sheet;
  let mut actions = Vec::new();
  if is_sheet && !context.in_thread {
    actions.push(MessageAction::Thread);
  }
  actions.push(MessageAction::Quote);
  if is_sheet {
    actions.push(MessageAction::Forward);
    actions.push(if context.is_saved {
      MessageAction::Unsave
    } else {
      MessageAction::Save
    });
  }
  actions.extend([
    MessageAction::CopyLink,
    MessageAction::CopyText,
    MessageAction::MarkUnread,
  ]);
  actions.push(if context.is_pinned {
    MessageAction::Unpin
  } else {
    MessageAction::Pin
  });
  if context.has_files {
    actions.push(MessageAction::BookmarkFile);
  }
  if context.is_mine {
    actions.extend([MessageAction::Edit, MessageAction::Delete]);
  }
  actions
}

impl MessageAction {
  pub fn label_key(self) -> &'static str {
    match self {
      Self::Thread => "action-reply",
      Self::Quote => "action-quote",
      Self::Forward => "action-forward",
      Self::Save => "action-save",
      Self::Unsave => "action-unsave",
      Self::Pin => "action-pin",
      Self::Unpin => "action-unpin",
      Self::BookmarkFile => "action-bookmark-file",
      Self::CopyLink => "action-copy-link",
      Self::CopyText => "action-copy-text",
      Self::MarkUnread => "action-mark-unread",
      Self::Edit => "action-edit",
      Self::Delete => "action-delete",
    }
  }

  pub fn icon(self) -> IconName {
    match self {
      Self::Thread => IconName::Threads,
      Self::Quote => IconName::Reply,
      Self::Forward => IconName::Forward,
      Self::Save | Self::Unsave | Self::BookmarkFile => IconName::Bookmark,
      Self::Pin | Self::Unpin => IconName::Star,
      Self::CopyLink => IconName::Link,
      Self::CopyText => IconName::Copy,
      Self::MarkUnread => IconName::EyeOff,
      Self::Edit => IconName::Compose,
      Self::Delete => IconName::Trash,
    }
  }

  /// The key that does the same on a focused message, shown in the tooltip.
  pub fn maybe_shortcut(self) -> Option<&'static str> {
    match self {
      Self::Thread => Some("T"),
      Self::Quote => Some("R"),
      _ => None,
    }
  }

  /// Edit and delete are set apart from the rest of the menu.
  pub fn starts_group(self) -> bool {
    self == Self::Edit
  }

  pub fn is_danger(self) -> bool {
    self == Self::Delete
  }
}

/// A label with its keyboard shortcut, for tooltips: "Reply in thread (T)".
pub fn with_shortcut(label: String, maybe_shortcut: Option<&str>) -> String {
  let i18n = use_i18n();
  match maybe_shortcut {
    Some(shortcut) => i18n.t_with(
      "action-with-shortcut",
      &[("action", &label), ("shortcut", shortcut)],
    ),
    None => label,
  }
}

/// Carries out an action on `message`. `maybe_extras` is what the message's row shows.
pub fn run_message_action(action: MessageAction, message: &Message, maybe_extras: Option<&MessageExtras>) {
  let state = use_app_state();
  let i18n = use_i18n();
  let (conversation, id) = (message.conversation, message.id);
  match action {
    MessageAction::Thread => {
      let thread = message
        .maybe_thread
        .unwrap_or_else(|| crate::model::thread_id_of(id));
      state.open_thread(conversation, thread);
    },
    MessageAction::Quote => {
      let author_name = state
        .read(|backend| backend.user(message.author))
        .map(|user| user.display_name)
        .unwrap_or_default();
      state.reply_target.set(Some(ReplyTarget {
        conversation,
        message: id,
        author_name,
        excerpt: excerpt(&message.body),
      }));
    },
    MessageAction::Forward => state.overlay.set(Overlay::Forward(conversation, id)),
    MessageAction::Save | MessageAction::Unsave => {
      let save = action == MessageAction::Save;
      state.write(|backend| backend.set_saved(conversation, id, save));
    },
    MessageAction::Pin | MessageAction::Unpin => {
      let pin = action == MessageAction::Pin;
      state.write(|backend| backend.set_pinned(conversation, id, pin));
    },
    MessageAction::BookmarkFile => {
      let Some(file) = maybe_extras.and_then(|extras| extras.files.first().cloned()) else {
        return;
      };
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
    },
    MessageAction::CopyLink => {
      // The toolbar asked for the link when it appeared, so a backend that loads it has it by now.
      match state.read(|backend| backend.permalink(conversation, id)) {
        Some(link) => copy_and_confirm(link),
        // Still loading, or the message isn't on the server yet (no ID to link to).
        None => state.show_error(i18n.t("action-copy-link-unready"), String::new()),
      }
    },
    MessageAction::CopyText => copy_and_confirm(message.body.clone()),
    MessageAction::MarkUnread => state.write(|backend| backend.mark_unread(conversation, id)),
    MessageAction::Edit => state.edit_request.set(Some(id)),
    MessageAction::Delete => state.delete_with_undo(conversation, id, i18n.t("message-deleted-toast")),
  }
}

/// Copies `text` and says whether it worked: "Copied", or an error when the browser refused.
pub(crate) fn copy_and_confirm(text: String) {
  let state = use_app_state();
  let i18n = use_i18n();
  let (copied, failed) = (i18n.t("action-copied"), i18n.t("action-copy-failed"));
  let Some(promise) = clipboard_write(&text) else {
    state.show_error(failed, String::new());
    return;
  };
  leptos::task::spawn_local(async move {
    // `show_error` does nothing once the shell is gone (issue #322); the toast checks first.
    match wasm_bindgen_futures::JsFuture::from(promise).await {
      Ok(_) => {
        if state.is_alive() {
          state.show_toast(copied);
        }
      },
      Err(_) => state.show_error(failed, String::new()),
    }
  });
}

/// Writes to the clipboard through the page's `navigator.clipboard` (web-sys keeps that API unstable).
pub(crate) fn copy_to_clipboard(text: &str) {
  // A refused write (no permission, insecure context) leaves the clipboard as it was.
  drop(clipboard_write(text));
}

/// Starts a clipboard write; `None` where the page has no clipboard API.
fn clipboard_write(text: &str) -> Option<js_sys::Promise> {
  let window = web_sys::window()?;
  let navigator = window.navigator();
  let clipboard = js_sys::Reflect::get(&navigator, &"clipboard".into()).ok()?;
  let write_text = js_sys::Reflect::get(&clipboard, &"writeText".into())
    .ok()?
    .dyn_into::<js_sys::Function>()
    .ok()?;
  write_text.call1(&clipboard, &text.into()).ok()?.dyn_into().ok()
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
        <CompactBack />
        <div class="panel-title">
          <h1>{move || i18n.t("saved-title")}</h1>
        </div>
      </header>
      <ul class="saved-list panel-scroll">
        <For each=move || items.get() key=|item| (item.message.id, item.saved_at) let:item>
          {
            let message = item.message.clone();
            let (conversation, id, maybe_thread) = (message.conversation, message.id, message.maybe_thread);
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
                  <button class="btn btn-small" on:click=move |_| state.open_message(conversation, id, maybe_thread)>
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
          <EmptyState icon=IconName::Bookmark title_key="saved-empty-title" body_key="saved-empty" as_item=true />
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
                <div class="saved-actions">
                  <button class="btn btn-small" on:click=move |_| state.open_message(conversation, id, None)>
                    {move || i18n.t("pins-jump")}
                  </button>
                  <button class="btn btn-small" on:click=move |_| state.write(|backend| backend.set_pinned(conversation, id, false))>
                    {move || i18n.t("action-unpin")}
                  </button>
                </div>
              </li>
            }
          }
        </For>
        <Show when=move || pinned.get().is_empty() fallback=|| ()>
          <EmptyState icon=IconName::Star title_key="pins-empty-title" body_key="pins-empty" as_item=true />
        </Show>
      </ul>
    </aside>
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_more_menu_leaves_out_the_toolbar_buttons() {
    let actions = message_actions(ActionContext::default(), ActionSurface::MoreMenu);
    assert_eq!(
      actions,
      [
        MessageAction::Quote,
        MessageAction::CopyLink,
        MessageAction::CopyText,
        MessageAction::MarkUnread,
        MessageAction::Pin,
      ]
    );
  }

  #[test]
  fn own_messages_end_with_edit_and_delete() {
    let context = ActionContext {
      is_mine: true,
      is_pinned: true,
      has_files: true,
      ..ActionContext::default()
    };
    let actions = message_actions(context, ActionSurface::MoreMenu);
    assert!(actions.contains(&MessageAction::Unpin) && actions.contains(&MessageAction::BookmarkFile));
    assert_eq!(
      actions[actions.len() - 2..],
      [MessageAction::Edit, MessageAction::Delete]
    );
    let others = message_actions(ActionContext::default(), ActionSurface::Sheet);
    assert!(!others.contains(&MessageAction::Edit) && !others.contains(&MessageAction::Delete));
  }

  #[test]
  fn the_sheet_has_everything_but_threads_inside_a_thread() {
    let saved = ActionContext {
      is_saved: true,
      ..ActionContext::default()
    };
    let sheet = message_actions(saved, ActionSurface::Sheet);
    assert_eq!(
      sheet[..4],
      [
        MessageAction::Thread,
        MessageAction::Quote,
        MessageAction::Forward,
        MessageAction::Unsave
      ]
    );
    let in_thread = ActionContext {
      in_thread: true,
      ..ActionContext::default()
    };
    assert!(!message_actions(in_thread, ActionSurface::Sheet).contains(&MessageAction::Thread));
  }

  #[test]
  fn excerpts_are_one_line_and_bounded() {
    assert_eq!(excerpt("a\n\n b"), "a b");
    assert_eq!(excerpt(&"x".repeat(500)).chars().count(), EXCERPT_CHARS + 1);
  }
}
