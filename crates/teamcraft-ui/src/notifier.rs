//! Desktop notifications for mentions and direct messages.
//!
//! Message text only exists decrypted inside the app, so the decision to notify is made here, from
//! what the chat backend shows, and the shell merely displays the result. [`decide`] is the policy
//! (levels, mutes, keywords, do-not-disturb); [`Notifier`] watches the backend for new messages and
//! thread replies and applies it.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::clock::now_ms;
use teamcraft_proto::PushContentMode;

use crate::model::{
  Conversation, ConversationId, ConversationKind, Message, MessageId, PresenceStatus, ThreadId, User, UserId,
};
use crate::notify_sync::dnd_to_server;
use crate::prefs::{NotificationPrefs, NotifyLevel, ThreadReplies};

/// How many of the newest messages of a conversation are looked at per check.
const WATCH_WINDOW: usize = 10;
/// Preview length in characters.
const PREVIEW_CHARS: usize = 140;
const MS_PER_SECOND: i64 = 1000;
/// Words that notify the whole conversation.
pub const CHANNEL_MENTIONS: [&str; 3] = ["channel", "here", "all"];

/// Everything the policy looks at for one candidate message.
pub struct PolicyInput<'a> {
  pub prefs: &'a NotificationPrefs,
  pub me: &'a User,
  pub conversation: &'a Conversation,
  pub sender: UserId,
  pub body: &'a str,
  pub kind: CandidateKind,
  pub now: crate::model::TimestampMs,
  /// The window has focus and shows this conversation.
  pub is_viewing: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandidateKind {
  Message,
  /// A reply in a thread the user follows; carries who started the thread.
  ThreadReply {
    is_thread_mine: bool,
  },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
  Notify,
  Skip(SkipReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
  OwnMessage,
  Viewing,
  Muted,
  LevelNothing,
  NotMentioned,
  DoNotDisturb,
  ThreadNotFollowedByPreference,
}

pub fn decide(input: &PolicyInput) -> Decision {
  if input.sender == input.me.id {
    return Decision::Skip(SkipReason::OwnMessage);
  }
  if input.is_viewing {
    return Decision::Skip(SkipReason::Viewing);
  }
  let id = input.conversation.id.to_string();
  let maybe_space = input.conversation.maybe_space.map(|space| space.to_string());
  if input.prefs.is_muted_in(&id, maybe_space.as_deref()) {
    return Decision::Skip(SkipReason::Muted);
  }
  if input.me.presence == PresenceStatus::DoNotDisturb || is_in_dnd_schedule(input) {
    return Decision::Skip(SkipReason::DoNotDisturb);
  }
  let is_mentioned = is_mentioned(input.body, input.me, input.prefs);
  if let CandidateKind::ThreadReply { is_thread_mine } = input.kind {
    let allowed = match input.prefs.thread_replies {
      ThreadReplies::Participating => true,
      ThreadReplies::Started => is_thread_mine,
      ThreadReplies::MentionsOnly => is_mentioned,
    };
    if !allowed {
      return Decision::Skip(SkipReason::ThreadNotFollowedByPreference);
    }
  }
  match input.prefs.effective_level_in(&id, maybe_space.as_deref()) {
    NotifyLevel::Nothing => Decision::Skip(SkipReason::LevelNothing),
    NotifyLevel::All => Decision::Notify,
    NotifyLevel::Mentions => {
      let is_direct = matches!(
        input.conversation.kind,
        ConversationKind::Direct | ConversationKind::Group
      );
      let is_thread = matches!(input.kind, CandidateKind::ThreadReply { .. });
      // Replies in followed threads already passed the thread preference above.
      if is_direct || is_mentioned || is_thread {
        Decision::Notify
      } else {
        Decision::Skip(SkipReason::NotMentioned)
      }
    },
  }
}

fn is_in_dnd_schedule(input: &PolicyInput) -> bool {
  dnd_to_server(&input.prefs.dnd, None, input.now)
    .is_some_and(|schedule| schedule.silences(input.now / MS_PER_SECOND, input.sender))
}

/// `@username`, `@channel`/`@here`/`@all` (when enabled), the first name (when enabled) or a keyword.
pub fn is_mentioned(body: &str, me: &User, prefs: &NotificationPrefs) -> bool {
  let lower = body.to_lowercase();
  let words: Vec<&str> = lower
    .split(|c: char| !c.is_alphanumeric() && c != '@' && c != '_' && c != '-')
    .collect();
  let username = format!("@{}", me.username.to_lowercase());
  if words.contains(&username.as_str()) {
    return true;
  }
  if prefs.channel_mentions_notify
    && CHANNEL_MENTIONS
      .iter()
      .any(|word| words.contains(&format!("@{word}").as_str()))
  {
    return true;
  }
  let first_name = me.display_name.split_whitespace().next().map(str::to_lowercase);
  if prefs.first_name_is_mention && first_name.is_some_and(|name| words.contains(&name.as_str())) {
    return true;
  }
  prefs
    .keywords
    .iter()
    .map(|keyword| keyword.trim().to_lowercase())
    .filter(|keyword| !keyword.is_empty())
    .any(|keyword| lower.contains(&keyword))
}

/// Whether sending `body` would notify a whole large conversation and should be confirmed first:
/// the text has `@channel`/`@here`/`@all` and the conversation has more than `confirm_over` members
/// (`0` never asks).
pub fn needs_channel_confirmation(body: &str, member_count: usize, confirm_over: u32) -> bool {
  if confirm_over == 0 || member_count <= confirm_over as usize {
    return false;
  }
  let lower = body.to_lowercase();
  lower
    .split(|c: char| !c.is_alphanumeric() && c != '@' && c != '_' && c != '-')
    .any(|word| {
      CHANNEL_MENTIONS
        .iter()
        .any(|mention| word == format!("@{mention}"))
    })
}

/// What to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
  pub conversation: ConversationId,
  pub title: NotificationTitle,
  pub body: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotificationTitle {
  /// The workspace policy allows no names: "New message".
  Generic,
  /// A direct message: just the sender.
  Sender(String),
  InChannel {
    sender: String,
    channel: String,
  },
  ThreadReply {
    sender: String,
  },
}

pub fn preview(body: &str) -> String {
  let single_line = body.split_whitespace().collect::<Vec<_>>().join(" ");
  if single_line.chars().count() <= PREVIEW_CHARS {
    return single_line;
  }
  let cut: String = single_line.chars().take(PREVIEW_CHARS).collect();
  format!("{cut}…")
}

/// Watches the backend for new messages; the first sight of a conversation only records its state.
#[derive(Default)]
pub struct Notifier {
  seen_message: RefCell<HashMap<ConversationId, MessageId>>,
  seen_replies: RefCell<HashMap<(ConversationId, ThreadId), usize>>,
}

/// One new message found by the watcher, before the policy looks at it.
pub struct Found {
  pub conversation: Conversation,
  pub message: Message,
  pub kind: CandidateKind,
}

impl Notifier {
  /// New messages from others since the last call, oldest first.
  pub fn collect(&self, backend: &dyn crate::backend::Backend) -> Vec<Found> {
    let me = backend.me();
    let mut found = Vec::new();
    let mut conversations = Vec::new();
    for space in backend.spaces() {
      conversations.extend(backend.space_conversations(space.id));
    }
    conversations.extend(backend.direct_conversations());
    for conversation in &conversations {
      self.collect_messages(backend, conversation, &mut found);
    }
    for summary in backend.followed_threads() {
      let Some(conversation) = conversations
        .iter()
        .find(|conversation| conversation.id == summary.conversation)
      else {
        continue;
      };
      self.collect_replies(backend, conversation, &summary, me.id, &mut found);
    }
    found
  }

  fn collect_messages(
    &self,
    backend: &dyn crate::backend::Backend,
    conversation: &Conversation,
    found: &mut Vec<Found>,
  ) {
    let count = backend.message_count(conversation.id);
    if count == 0 {
      return;
    }
    let start = count.saturating_sub(WATCH_WINDOW);
    let window = backend.messages(conversation.id, start, WATCH_WINDOW);
    let Some(newest) = window.last().map(|message| message.id) else {
      // Still loading; try again on the next change.
      return;
    };
    let mut seen = self.seen_message.borrow_mut();
    match seen.insert(conversation.id, newest) {
      None => {},
      Some(previous) => {
        for message in window.into_iter().filter(|message| message.id > previous) {
          found.push(Found {
            conversation: conversation.clone(),
            message,
            kind: CandidateKind::Message,
          });
        }
      },
    }
  }

  fn collect_replies(
    &self,
    backend: &dyn crate::backend::Backend,
    conversation: &Conversation,
    summary: &crate::model::ThreadSummary,
    me: UserId,
    found: &mut Vec<Found>,
  ) {
    let key = (conversation.id, crate::model::thread_id_of(summary.root.id));
    let previous = self
      .seen_replies
      .borrow_mut()
      .insert(key, summary.reply_count as usize);
    let Some(previous) = previous else { return };
    if (summary.reply_count as usize) <= previous {
      return;
    }
    let thread = backend.thread(conversation.id, key.1);
    let new_replies = thread.into_iter().skip(1 + previous);
    for message in new_replies {
      found.push(Found {
        conversation: conversation.clone(),
        message,
        kind: CandidateKind::ThreadReply {
          is_thread_mine: summary.root.author == me,
        },
      });
    }
  }
}

/// Applies the policy to what the watcher found and builds what should be shown.
pub fn notifications_for(
  found: Vec<Found>,
  prefs: &NotificationPrefs,
  backend: &dyn crate::backend::Backend,
  is_viewing: impl Fn(ConversationId) -> bool,
) -> Vec<Notification> {
  let me = backend.me();
  let now = now_ms();
  let mode = backend.push_content_mode();
  found
    .into_iter()
    .filter_map(|item| {
      let input = PolicyInput {
        prefs,
        me: &me,
        conversation: &item.conversation,
        sender: item.message.author,
        body: &item.message.body,
        kind: item.kind,
        now,
        is_viewing: is_viewing(item.conversation.id),
      };
      if decide(&input) != Decision::Notify {
        return None;
      }
      let sender = backend
        .user(item.message.author)
        .map(|user| user.display_name)
        .unwrap_or_default();
      let title = match (item.kind, item.conversation.kind) {
        (CandidateKind::ThreadReply { .. }, _) => NotificationTitle::ThreadReply { sender },
        (CandidateKind::Message, ConversationKind::Direct) => NotificationTitle::Sender(sender),
        (CandidateKind::Message, _) => NotificationTitle::InChannel {
          sender,
          channel: item.conversation.name.clone(),
        },
      };
      Some(Notification {
        conversation: item.conversation.id,
        title,
        body: prefs.show_preview.then(|| preview(&item.message.body)),
      })
    })
    .map(|notification| limited_to(notification, &mode))
    .collect()
}

/// Cuts a notification down to what the verified workspace policy allows on a lock screen, whatever
/// the person's own "show message text" choice (the policy only ever takes away). An unknown mode
/// counts as the strictest.
pub fn limited_to(mut notification: Notification, mode: &PushContentMode) -> Notification {
  match mode {
    PushContentMode::Full => {},
    PushContentMode::Sender => notification.body = None,
    PushContentMode::Generic | PushContentMode::Unknown(_) => {
      notification.body = None;
      notification.title = NotificationTitle::Generic;
    },
  }
  notification
}

#[cfg(test)]
mod tests {
  use super::*;

  fn shown() -> Notification {
    Notification {
      conversation: id_from_raw(1),
      title: NotificationTitle::InChannel {
        sender: "Alice".into(),
        channel: "planning".into(),
      },
      body: Some("lunch at noon?".into()),
    }
  }

  #[test]
  fn a_backend_without_a_verified_policy_allows_only_generic_notifications() {
    let backend = crate::mock::MockBackend::new(NOW);
    assert_eq!(
      crate::backend::ChatBackend::push_content_mode(&backend),
      PushContentMode::Generic
    );
  }

  #[test]
  fn the_policy_limits_what_a_desktop_notification_shows() {
    assert_eq!(limited_to(shown(), &PushContentMode::Full), shown());
    let sender = limited_to(shown(), &PushContentMode::Sender);
    assert_eq!(sender.body, None);
    assert_eq!(sender.title, shown().title);
    for mode in [PushContentMode::Generic, PushContentMode::Unknown("x".into())] {
      let generic = limited_to(shown(), &mode);
      assert_eq!(generic.body, None);
      assert_eq!(generic.title, NotificationTitle::Generic);
    }
  }

  use crate::model::{SidebarCategory, id_from_raw};
  use crate::prefs::{ConversationNotify, DndSchedule, MuteFor};

  const NOW: i64 = 1_792_540_800_000 + 12 * 3_600_000;

  #[test]
  fn direct_messages_notify_at_the_mentions_level() {
    let prefs = NotificationPrefs::default();
    assert_eq!(
      decide(&input(&prefs, ConversationKind::Direct, "hello")),
      Decision::Notify
    );
  }

  #[test]
  fn channel_messages_need_a_mention_at_the_mentions_level() {
    let prefs = NotificationPrefs::default();
    assert_eq!(
      decide(&input(&prefs, ConversationKind::Public, "lunch?")),
      Decision::Skip(SkipReason::NotMentioned)
    );
    assert_eq!(
      decide(&input(&prefs, ConversationKind::Public, "ping @alice")),
      Decision::Notify
    );
  }

  #[test]
  fn level_all_notifies_for_everything_and_nothing_for_nothing() {
    let mut prefs = NotificationPrefs {
      desktop: NotifyLevel::All,
      ..NotificationPrefs::default()
    };
    assert_eq!(
      decide(&input(&prefs, ConversationKind::Public, "lunch?")),
      Decision::Notify
    );
    prefs.desktop = NotifyLevel::Nothing;
    assert_eq!(
      decide(&input(&prefs, ConversationKind::Direct, "hi")),
      Decision::Skip(SkipReason::LevelNothing)
    );
  }

  #[test]
  fn a_conversation_override_wins_over_the_global_level() {
    let mut prefs = NotificationPrefs::default();
    prefs.set_conversation(
      &conversation(ConversationKind::Public).id.to_string(),
      ConversationNotify {
        maybe_level: Some(NotifyLevel::All),
        mute: MuteFor::Off,
      },
    );
    assert_eq!(
      decide(&input(&prefs, ConversationKind::Public, "lunch?")),
      Decision::Notify
    );
  }

  #[test]
  fn muted_conversations_stay_quiet_even_when_mentioned() {
    let mut prefs = NotificationPrefs::default();
    prefs.set_conversation(
      &conversation(ConversationKind::Public).id.to_string(),
      ConversationNotify {
        maybe_level: None,
        mute: MuteFor::Forever,
      },
    );
    assert_eq!(
      decide(&input(&prefs, ConversationKind::Public, "@alice")),
      Decision::Skip(SkipReason::Muted)
    );
  }

  #[test]
  fn a_muted_space_silences_its_channels_unless_they_have_their_own_setting() {
    let space = id_from_raw::<crate::model::SpaceId>(3);
    let mut prefs = NotificationPrefs::default();
    prefs.set_space(
      &space.to_string(),
      ConversationNotify {
        maybe_level: Some(NotifyLevel::Nothing),
        mute: MuteFor::Off,
      },
    );
    let mut candidate = input(&prefs, ConversationKind::Public, "@alice");
    let mut in_space = candidate.conversation.clone();
    in_space.maybe_space = Some(space);
    candidate.conversation = Box::leak(Box::new(in_space));
    assert_eq!(decide(&candidate), Decision::Skip(SkipReason::Muted));

    let mut own = prefs.clone();
    own.set_conversation(
      &candidate.conversation.id.to_string(),
      ConversationNotify {
        maybe_level: Some(NotifyLevel::All),
        mute: MuteFor::Off,
      },
    );
    candidate.prefs = Box::leak(Box::new(own));
    assert_eq!(decide(&candidate), Decision::Notify);
  }

  #[test]
  fn the_conversation_in_front_does_not_notify() {
    let prefs = NotificationPrefs::default();
    let mut input = input(&prefs, ConversationKind::Direct, "hi");
    input.is_viewing = true;
    assert_eq!(decide(&input), Decision::Skip(SkipReason::Viewing));
  }

  #[test]
  fn the_do_not_disturb_schedule_silences() {
    let prefs = NotificationPrefs {
      dnd: DndSchedule {
        is_enabled: true,
        from: "00:00".into(),
        to: "23:59".into(),
        weekdays_only: false,
      },
      ..NotificationPrefs::default()
    };
    assert_eq!(
      decide(&input(&prefs, ConversationKind::Direct, "hi")),
      Decision::Skip(SkipReason::DoNotDisturb)
    );
  }

  #[test]
  fn manual_do_not_disturb_silences() {
    let prefs = NotificationPrefs::default();
    let mut input = input(&prefs, ConversationKind::Direct, "hi");
    let mut me = input.me.clone();
    me.presence = PresenceStatus::DoNotDisturb;
    input.me = &me;
    assert_eq!(decide(&input), Decision::Skip(SkipReason::DoNotDisturb));
  }

  #[test]
  fn own_messages_never_notify() {
    let prefs = NotificationPrefs::default();
    let mut input = input(&prefs, ConversationKind::Direct, "hi");
    input.sender = input.me.id;
    assert_eq!(decide(&input), Decision::Skip(SkipReason::OwnMessage));
  }

  #[test]
  fn thread_replies_follow_the_thread_preference() {
    let started = NotificationPrefs {
      thread_replies: ThreadReplies::Started,
      ..NotificationPrefs::default()
    };
    let mut reply = input(&started, ConversationKind::Public, "thanks");
    reply.kind = CandidateKind::ThreadReply {
      is_thread_mine: false,
    };
    assert_eq!(
      decide(&reply),
      Decision::Skip(SkipReason::ThreadNotFollowedByPreference)
    );
    reply.kind = CandidateKind::ThreadReply { is_thread_mine: true };
    assert_eq!(decide(&reply), Decision::Notify);
    let participating = NotificationPrefs::default();
    let mut reply = input(&participating, ConversationKind::Public, "thanks");
    reply.kind = CandidateKind::ThreadReply {
      is_thread_mine: false,
    };
    assert_eq!(decide(&reply), Decision::Notify);
  }

  #[test]
  fn mentions_are_whole_words() {
    let me = me();
    let prefs = NotificationPrefs {
      keywords: vec!["launch".into()],
      ..NotificationPrefs::default()
    };
    assert!(is_mentioned("hey @alice!", &me, &prefs));
    assert!(!is_mentioned("hey @alicejones", &me, &prefs));
    assert!(is_mentioned("@channel standup", &me, &prefs));
    assert!(is_mentioned("Alice, can you look?", &me, &prefs));
    assert!(is_mentioned("the Launch plan", &me, &prefs));
    assert!(!is_mentioned("nothing here", &me, &prefs));
  }

  #[test]
  fn channel_mentions_can_be_turned_off() {
    let prefs = NotificationPrefs {
      channel_mentions_notify: false,
      first_name_is_mention: false,
      ..NotificationPrefs::default()
    };
    assert!(!is_mentioned("@here please", &me(), &prefs));
  }

  #[test]
  fn large_conversations_ask_before_a_channel_mention() {
    assert!(needs_channel_confirmation("@channel standup", 25, 10));
    assert!(needs_channel_confirmation("hi @Here!", 11, 10));
    assert!(!needs_channel_confirmation("@channel standup", 10, 10));
    assert!(!needs_channel_confirmation("@channel standup", 25, 0));
    assert!(!needs_channel_confirmation("mail me@channel.example", 25, 10));
    assert!(!needs_channel_confirmation("no mention", 25, 10));
  }

  #[test]
  fn previews_are_one_line_and_bounded() {
    assert_eq!(preview("a\n\n b   c"), "a b c");
    let long = "x".repeat(300);
    assert_eq!(preview(&long).chars().count(), PREVIEW_CHARS + 1);
  }

  fn input<'a>(prefs: &'a NotificationPrefs, kind: ConversationKind, body: &'a str) -> PolicyInput<'a> {
    // Leaked: tests need `&'a` values for a whole call and the sizes are tiny.
    let me: &'a User = Box::leak(Box::new(me()));
    let conversation: &'a Conversation = Box::leak(Box::new(conversation(kind)));
    PolicyInput {
      prefs,
      me,
      conversation,
      sender: id_from_raw(2),
      body,
      kind: CandidateKind::Message,
      now: NOW,
      is_viewing: false,
    }
  }

  fn me() -> User {
    User {
      id: id_from_raw(1),
      username: "alice".into(),
      display_name: "Alice Owner".into(),
      presence: PresenceStatus::Online,
      maybe_custom_status: None,
      avatar_hue: 10,
      is_guest: false,
      is_bot: false,
      maybe_organization: None,
    }
  }

  fn conversation(kind: ConversationKind) -> Conversation {
    Conversation {
      id: id_from_raw(7),
      maybe_space: None,
      kind,
      category: SidebarCategory::Channels,
      name: "launch".into(),
      topic: String::new(),
      member_ids: Vec::new(),
      unread_count: 0,
      mention_count: 0,
      is_favorite: false,
      is_muted: false,
      can_post: true,
      bookmarks: Vec::new(),
    }
  }
}
