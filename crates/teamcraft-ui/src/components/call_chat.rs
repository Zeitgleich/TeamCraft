//! The chat next to a call's video (#936): the messages of the conversation the call belongs to in
//! a panel beside the stage (over it on a phone), with the conversation's own feed and composer. A
//! "Chat" button in the control bar opens and closes it and counts what is unread; while it is
//! closed, a new message from somebody else shows as a short toast that opens it.
//!
//! The feed is the conversation's [`ChannelFeed`], so reading works as in the app: opening the
//! panel marks the conversation read, and new messages are read while the feed's newest message is
//! in sight. The conversation below the call screen doesn't read them ([`BesideCall`]).

use std::time::Duration;

use leptos::prelude::*;

use crate::clock::now_ms;
use crate::components::composer::Composer;
use crate::components::count_badge::BadgeTone;
use crate::components::file_drop::{DropStrip, FileDrop};
use crate::components::icons::{Icon, IconName};
use crate::components::log_view::ChannelFeed;
use crate::components::message_list::BesideCall;
use crate::components::thread_panel::{ThreadPanel, conversation_label};
use crate::components::typing_indicator::TypingIndicator;
use crate::i18n::use_i18n;
use crate::model::{ConversationId, Message, MessageId, MessageKind, TimestampMs, UserId};
use crate::state::{AppState, SidePanel, use_app_state};

/// How long a message's toast stays.
const TOAST_MS: u64 = 5_000;
/// The newest messages looked at for news: more than arrive between two backend changes.
const WATCH_WINDOW: usize = 20;

/// A new message while the panel is closed: who wrote it and how it starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatToast {
  pub message: MessageId,
  pub sender: String,
  /// The text's first line; `None` for a message of files only.
  pub maybe_preview: Option<String>,
}

/// Finds the messages from others that arrived since the last look.
#[derive(Clone, Debug)]
pub struct ArrivalWatch {
  /// The newest message from somebody else seen so far.
  maybe_seen: Option<MessageId>,
  /// Whether a look saw messages: the first one that does only records them, except those sent
  /// after `since` (a conversation that was empty, or still loading, when the watch began).
  has_looked: bool,
  since: TimestampMs,
}

impl ArrivalWatch {
  pub fn new(since: TimestampMs) -> Self {
    Self {
      maybe_seen: None,
      has_looked: false,
      since,
    }
  }

  /// The newest message from somebody else in `tail` (a conversation's newest messages) that is
  /// new since the previous look; system lines and the user's own messages aren't news.
  pub fn look(&mut self, tail: &[Message], me: UserId) -> Option<Message> {
    if tail.is_empty() {
      return None;
    }
    let is_first = !self.has_looked;
    self.has_looked = true;
    let newest = tail
      .iter()
      .filter(|message| message.author != me && message.kind == MessageKind::User)
      .max_by_key(|message| message.id)?;
    if self.maybe_seen.is_some_and(|seen| newest.id <= seen) {
      return None;
    }
    self.maybe_seen = Some(newest.id);
    (!is_first || newest.sent_at >= self.since).then(|| newest.clone())
  }
}

/// The number on the Chat button: what is unread, while the panel is closed.
pub fn badge_count(is_open: bool, unread: u32) -> u32 {
  if is_open { 0 } else { unread }
}

/// The chat of the call on `conversation`: whether its panel is open, its unread count and toast.
#[derive(Clone, Copy)]
pub struct CallChat {
  pub conversation: ConversationId,
  pub is_open: RwSignal<bool>,
  pub toast: RwSignal<Option<ChatToast>>,
  /// The conversation's unread count and whether it asks for the user (a mention, a DM).
  unread: Memo<(u32, bool)>,
}

impl CallChat {
  /// Watches `conversation` for new messages while the call screen shows; `is_open` lives in the
  /// call's state, so the panel stays open across minimizing the call.
  pub fn new(state: AppState, conversation: ConversationId, is_open: RwSignal<bool>) -> Self {
    let chat = Self::unwatched(state, conversation, is_open);
    chat.watch(state);
    chat
  }

  fn unwatched(state: AppState, conversation: ConversationId, is_open: RwSignal<bool>) -> Self {
    let unread = Memo::new(move |_| {
      state
        .read(|backend| backend.conversation(conversation))
        .map(|conv| {
          let is_loud = conv.mention_count > 0 || conv.kind.is_direct_like();
          (conv.unread_count.max(conv.mention_count), is_loud)
        })
        .unwrap_or_default()
    });
    Self {
      conversation,
      is_open,
      toast: RwSignal::new(None),
      unread,
    }
  }

  pub fn badge(&self) -> u32 {
    badge_count(self.is_open.get(), self.unread.get().0)
  }

  pub fn badge_tone(&self) -> BadgeTone {
    BadgeTone::of(self.unread.get().1)
  }

  pub fn toggle(&self) {
    self.is_open.update(|open| *open = !*open);
    self.toast.set(None);
  }

  pub fn open(&self) {
    self.is_open.set(true);
    self.toast.set(None);
  }

  /// Shows `toast` unless the panel is open; it goes after [`TOAST_MS`] unless another came.
  pub fn show_toast(&self, toast: ChatToast) {
    let id = toast.message;
    if !self.put_toast(toast) {
      return;
    }
    let shown = self.toast;
    set_timeout(
      move || {
        if shown
          .try_with_untracked(|maybe| maybe.as_ref().is_some_and(|toast| toast.message == id))
          .unwrap_or(false)
        {
          shown.try_set(None);
        }
      },
      Duration::from_millis(TOAST_MS),
    );
  }

  /// Shows `toast` unless the panel is open; whether it shows.
  fn put_toast(&self, toast: ChatToast) -> bool {
    if self.is_open.get_untracked() {
      return false;
    }
    self.toast.set(Some(toast));
    true
  }

  /// Toasts every new message from somebody else. Runs when the number of messages changes, not
  /// on every change of the backend (a call changes it often).
  fn watch(self, state: AppState) {
    let conversation = self.conversation;
    let count = Memo::new(move |_| state.read(|backend| backend.message_count(conversation)));
    let watch = StoredValue::new(ArrivalWatch::new(now_ms()));
    let me = untrack(|| state.me().id);
    Effect::new(move |_| {
      let count = count.get();
      untrack(|| {
        let start = count.saturating_sub(WATCH_WINDOW);
        let tail = state.read(|backend| backend.messages(conversation, start, count - start));
        let Some(Some(message)) = watch.try_update_value(|watch| watch.look(&tail, me)) else {
          return;
        };
        let sender = state
          .read(|backend| backend.user(message.author))
          .map(|user| user.display_name)
          .unwrap_or_default();
        let preview = crate::notifier::preview(&message.body);
        self.show_toast(ChatToast {
          message: message.id,
          sender,
          maybe_preview: (!preview.is_empty()).then_some(preview),
        });
      });
    });
  }
}

/// The panel: the conversation's feed and composer, or a thread of it that was opened from there.
#[component]
pub fn CallChatPanel(chat: CallChat) -> impl IntoView {
  provide_context(BesideCall);
  let state = use_app_state();
  let i18n = use_i18n();
  let conversation = chat.conversation;
  let maybe_info = untrack(|| state.read(|backend| backend.conversation(conversation)));
  let can_post = maybe_info.as_ref().is_none_or(|conv| conv.can_post);
  let name = maybe_info
    .as_ref()
    .map(|conv| crate::components::channel_view::shown_name(i18n, conv))
    .unwrap_or_default();
  let subtitle = maybe_info.as_ref().map(conversation_label).unwrap_or_default();
  // Opening the panel reads the conversation, as opening it in the app does.
  if maybe_info.as_ref().is_some_and(|conv| conv.has_unread()) {
    state.write(|backend| backend.mark_read(conversation));
  }
  let zone = FileDrop::provide(can_post);
  let maybe_thread = Memo::new(move |_| match state.panel.get() {
    SidePanel::Thread {
      conversation: open,
      thread,
    } if open == conversation => Some(thread),
    _ => None,
  });
  view! {
    <aside
      class="call-chat"
      aria-label=move || i18n.t("call-chat-label")
      on:dragenter=move |ev| zone.on_dragenter(&ev)
      on:dragleave=move |_| zone.on_dragleave()
      on:dragover=move |ev| zone.on_dragover(&ev)
      on:drop=move |ev| {
        zone.on_drop(&ev);
      }
    >
      <header class="panel-header">
        <div class="panel-title">
          <h2>{move || i18n.t("call-chat")}</h2>
          <span class="panel-subtitle" translate="no">{subtitle}</span>
        </div>
        <button
          class="icon-btn"
          aria-label=move || i18n.t("call-chat-hide")
          title=move || i18n.t("call-chat-hide")
          on:click=move |_| chat.is_open.set(false)
        >
          <Icon name=IconName::Close />
        </button>
      </header>
      {move || match maybe_thread.get() {
        Some(thread) => view! {
          <ThreadPanel
            conversation=conversation
            thread=thread
            embedded=true
            maybe_on_close=Callback::new(move |()| state.panel.set(SidePanel::Closed))
          />
        }
        .into_any(),
        None => {
          let name = name.clone();
          view! {
            <div class="channel-messages">
              <ChannelFeed conversation=conversation />
              <TypingIndicator conversation=conversation />
              <DropStrip zone=zone />
              <Composer conversation=conversation can_post=can_post conversation_name=name />
            </div>
          }
          .into_any()
        },
      }}
    </aside>
  }
}

/// The toast of a new message while the panel is closed; selecting it opens the panel. The live
/// region is always in the page, so screen readers announce the toast when it arrives.
#[component]
pub fn CallChatToast(chat: CallChat) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <div class="call-chat-toasts" role="status">
      {move || {
        chat.toast.get().map(|toast| {
          let label = i18n.t_with("call-chat-toast-open", &[("name", &toast.sender)]);
          let preview = toast.maybe_preview.unwrap_or_else(|| i18n.t("call-chat-toast-files"));
          view! {
            <button class="call-chat-toast" aria-label=label on:click=move |_| chat.open()>
              <Icon name=IconName::Chat />
              <span class="call-chat-toast-text">
                <strong translate="no">{toast.sender}</strong>
                <span>{preview}</span>
              </span>
            </button>
          }
        })
      }}
    </div>
  }
}

#[cfg(test)]
mod tests {
  use std::rc::Rc;

  use super::*;
  use crate::backend::Backend;
  use crate::mock::MockBackend;
  use crate::model::id_from_raw;
  use crate::settings::Settings;

  const ME: u64 = 1;
  const BOB: u64 = 2;

  fn message(raw: u64, author: u64, sent_at: TimestampMs) -> Message {
    Message {
      id: id_from_raw(raw),
      conversation: id_from_raw(10),
      author: id_from_raw(author),
      kind: MessageKind::User,
      sent_at,
      body: format!("message {raw}"),
      is_edited: false,
      reactions: Vec::new(),
      reply_count: 0,
      maybe_thread: None,
    }
  }

  fn ids(found: Option<Message>) -> Option<u64> {
    found.map(|message| crate::model::raw_id(message.id))
  }

  #[test]
  fn the_first_look_records_and_later_ones_report_the_newest_from_others() {
    let mut watch = ArrivalWatch::new(1_000);
    let me = id_from_raw(ME);
    let mut tail = vec![message(1, BOB, 10), message(2, ME, 20)];
    assert_eq!(ids(watch.look(&tail, me)), None, "history is no news");
    tail.push(message(3, ME, 1_100));
    assert_eq!(ids(watch.look(&tail, me)), None, "the user's own message");
    tail.push(message(4, BOB, 1_200));
    tail.push(message(5, BOB, 1_300));
    assert_eq!(ids(watch.look(&tail, me)), Some(5), "the newest of two");
    assert_eq!(ids(watch.look(&tail, me)), None, "reported once");
  }

  #[test]
  fn system_lines_are_no_news() {
    let mut watch = ArrivalWatch::new(0);
    let me = id_from_raw(ME);
    let mut tail = vec![message(1, BOB, 10)];
    watch.look(&tail, me);
    let mut joined = message(2, BOB, 20);
    joined.kind = MessageKind::System(crate::model::SystemEvent::Joined);
    tail.push(joined);
    assert_eq!(ids(watch.look(&tail, me)), None);
  }

  /// A conversation that was empty or still loading when the call screen opened: what loads is
  /// history, and what was sent after the screen opened is news.
  #[test]
  fn an_empty_start_reports_only_what_came_after_it() {
    let me = id_from_raw(ME);
    let mut watch = ArrivalWatch::new(1_000);
    assert_eq!(ids(watch.look(&[], me)), None);
    assert_eq!(ids(watch.look(&[message(1, BOB, 500)], me)), None);

    let mut watch = ArrivalWatch::new(1_000);
    assert_eq!(ids(watch.look(&[], me)), None);
    assert_eq!(ids(watch.look(&[message(1, BOB, 1_500)], me)), Some(1));
  }

  #[test]
  fn the_badge_counts_only_while_the_panel_is_closed() {
    assert_eq!(badge_count(false, 3), 3);
    assert_eq!(badge_count(true, 3), 0);
    assert_eq!(badge_count(false, 0), 0);
  }

  /// A toast shows while the panel is closed; opening the panel removes it, and an open panel
  /// takes none. The badge counts the conversation's unread messages while the panel is closed.
  #[test]
  fn opening_the_panel_takes_the_toast_away() {
    let owner = Owner::new();
    owner.with(|| {
      let state = AppState::new(
        Rc::new(MockBackend::new(0)) as Rc<dyn Backend>,
        Settings::for_tests(),
        None,
      );
      let unread = state
        .read(|backend| backend.direct_conversations())
        .into_iter()
        .find(|conv| conv.unread_count > 0)
        .expect("the demo has an unread direct conversation");
      let is_open = RwSignal::new(false);
      let chat = CallChat::unwatched(state, unread.id, is_open);
      assert_eq!(chat.badge(), unread.unread_count.max(unread.mention_count));
      assert_eq!(
        chat.badge_tone(),
        BadgeTone::Mention,
        "a direct message asks for the user"
      );
      let toast = ChatToast {
        message: id_from_raw(7),
        sender: "Bob".into(),
        maybe_preview: Some("Hi".into()),
      };
      assert!(chat.put_toast(toast.clone()));
      assert_eq!(chat.toast.get_untracked(), Some(toast.clone()));
      chat.open();
      assert!(is_open.get_untracked());
      assert_eq!(chat.toast.get_untracked(), None);
      assert_eq!(chat.badge(), 0, "no badge on the open panel");
      assert!(!chat.put_toast(toast));
      assert_eq!(chat.toast.get_untracked(), None, "no toast over the open panel");
      chat.toggle();
      assert!(!is_open.get_untracked());
    });
  }
}
