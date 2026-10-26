//! Desktop notifications for mentions and direct messages.
//!
//! Message text only exists decrypted inside the app, so the decision to notify is made here, from
//! what the chat backend shows, and the shell merely displays the result. [`decide`] is the policy
//! (levels, mutes, keywords, do-not-disturb); [`Notifier`] watches the backend for new messages and
//! thread replies and applies it.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use crate::clock::{local_utc_offset_minutes, now_ms};
use teamcraft_client_api::actions::MessagePriority;
pub use teamcraft_client_api::notify::needs_channel_confirmation;
use teamcraft_client_api::notify::{self, MentionRules};
use teamcraft_proto::{DndCandidate, PushContentMode};

use crate::model::{
  Conversation, ConversationId, ConversationKind, Message, MessageId, PresenceStatus, ThreadId, User, UserId,
  is_pending,
};
use crate::notify_sync::{LocalZone, dnd_to_server};
use crate::prefs::{NotificationPrefs, NotifyLevel, ThreadReplies};

/// How many of the newest messages of a conversation are looked at per check.
const WATCH_WINDOW: usize = 10;
/// Preview length in characters.
const PREVIEW_CHARS: usize = 140;
const MS_PER_SECOND: i64 = 1000;

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
  /// The sender marked the message urgent.
  pub is_urgent: bool,
  /// The device's offset from UTC now; the schedule is checked in the device's own time.
  pub utc_offset_minutes: i16,
  /// When a quick do-not-disturb chosen on this device ends.
  pub maybe_dnd_until: Option<crate::model::TimestampMs>,
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
  if is_silenced_by_dnd(input) {
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

/// The second check after the server's (notifications.md, "DND"): manual DND (until it ends) or a
/// schedule window silences, unless the sender is on the allow list or an urgent message may break
/// through.
fn is_silenced_by_dnd(input: &PolicyInput) -> bool {
  let zone = LocalZone {
    utc_offset_minutes: input.utc_offset_minutes,
    maybe_time_zone: None,
  };
  let maybe_schedule = dnd_to_server(&input.prefs.dnd, &zone, None);
  let lets_through = maybe_schedule.as_ref().is_some_and(|schedule| {
    schedule.lets_through(DndCandidate {
      sender: input.sender,
      is_urgent: input.is_urgent,
      is_direct: matches!(
        input.conversation.kind,
        ConversationKind::Direct | ConversationKind::Group
      ),
    })
  });
  if lets_through {
    return false;
  }
  let is_manual = input.me.presence == PresenceStatus::DoNotDisturb
    && input.maybe_dnd_until.is_none_or(|until| input.now < until);
  is_manual
    || maybe_schedule.is_some_and(|schedule| {
      schedule.is_active_at(input.now.div_euclid(MS_PER_SECOND), input.utc_offset_minutes)
    })
}

/// `@username`, `@channel`/`@here`/`@all` (when enabled), the first name (when enabled) or a keyword
/// (the rules are shared with the mobile apps).
pub fn is_mentioned(body: &str, me: &User, prefs: &NotificationPrefs) -> bool {
  notify::is_mentioned(
    body,
    &MentionRules {
      username: &me.username,
      display_name: &me.display_name,
      first_name_is_mention: prefs.first_name_is_mention,
      channel_mentions_notify: prefs.channel_mentions_notify,
      keywords: &prefs.keywords,
    },
  )
}

/// What to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
  pub conversation: ConversationId,
  pub title: NotificationTitle,
  pub body: Option<String>,
  /// A reply from the notification goes to the conversation: offered for messages in conversations
  /// the user may post in, not for thread replies (the answer belongs in the thread).
  pub can_reply: bool,
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

/// Watches the backend for new messages. The first look only records the state; a conversation
/// that appears later (a new direct message, a channel the user was added to) reports its unread
/// messages that are newer than everything seen before it.
#[derive(Default)]
pub struct Notifier {
  seen_message: RefCell<HashMap<ConversationId, MessageId>>,
  seen_replies: RefCell<HashMap<(ConversationId, ThreadId), usize>>,
  /// The newest message the server confirmed in any conversation, as of the previous look.
  maybe_newest_seen: Cell<Option<MessageId>>,
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
    let conversations = all_conversations(backend);
    for conversation in &conversations {
      self.collect_messages(backend, conversation, &mut found);
    }
    let newest_seen = self
      .seen_message
      .borrow()
      .values()
      .copied()
      .filter(|id| !is_pending(*id))
      .max();
    if newest_seen.is_some() {
      self.maybe_newest_seen.set(newest_seen);
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
    let maybe_previous = self.seen_message.borrow_mut().insert(conversation.id, newest);
    let new = new_messages(
      window,
      maybe_previous,
      self.maybe_newest_seen.get(),
      conversation.unread_count,
    );
    for message in new {
      found.push(Found {
        conversation: conversation.clone(),
        message,
        kind: CandidateKind::Message,
      });
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

/// Channels of every space and the direct conversations.
pub fn all_conversations(backend: &dyn crate::backend::Backend) -> Vec<Conversation> {
  let mut conversations = Vec::new();
  for space in backend.spaces() {
    conversations.extend(backend.space_conversations(space.id));
  }
  conversations.extend(backend.direct_conversations());
  conversations
}

/// Finds conversations that became read (here or on another device), so their notifications can be
/// withdrawn. The first look reports every read conversation: push notifications the service worker
/// showed while the app was closed may still be on screen.
#[derive(Default)]
pub struct ReadWatcher {
  maybe_unread: RefCell<Option<HashMap<ConversationId, u32>>>,
}

impl ReadWatcher {
  pub fn newly_read(&self, conversations: &[(ConversationId, u32)]) -> Vec<ConversationId> {
    let current: HashMap<ConversationId, u32> = conversations.iter().copied().collect();
    let previous = self.maybe_unread.replace(Some(current));
    conversations
      .iter()
      .filter(|(_, unread)| *unread == 0)
      .filter(|(id, _)| {
        previous
          .as_ref()
          .is_none_or(|previous| previous.get(id).is_some_and(|before| *before > 0))
      })
      .map(|(id, _)| *id)
      .collect()
  }
}

/// The messages of `window` (a conversation's newest, oldest first) to report: those after the
/// newest one seen in it before, or, in a conversation seen for the first time, its unread ones
/// newer than everything seen elsewhere (none on the first look, which only records the state).
fn new_messages(
  window: Vec<Message>,
  maybe_previous: Option<MessageId>,
  maybe_newest_elsewhere: Option<MessageId>,
  unread_count: u32,
) -> Vec<Message> {
  match (maybe_previous, maybe_newest_elsewhere) {
    (Some(previous), _) => window
      .into_iter()
      .filter(|message| message.id > previous)
      .collect(),
    (None, Some(newest_elsewhere)) => {
      let unread = usize::try_from(unread_count).unwrap_or(usize::MAX);
      let skipped = window.len().saturating_sub(unread);
      window
        .into_iter()
        .skip(skipped)
        .filter(|message| message.id > newest_elsewhere)
        .collect()
    },
    (None, None) => Vec::new(),
  }
}

/// Applies the policy to what the watcher found and builds what should be shown.
pub fn notifications_for(
  found: Vec<Found>,
  prefs: &NotificationPrefs,
  backend: &dyn crate::backend::Backend,
  is_viewing: impl Fn(ConversationId) -> bool,
  maybe_dnd_until: Option<crate::model::TimestampMs>,
) -> Vec<Notification> {
  let me = backend.me();
  let now = now_ms();
  let utc_offset_minutes = local_utc_offset_minutes(now);
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
        is_urgent: is_urgent(backend, &item.message),
        utc_offset_minutes,
        maybe_dnd_until,
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
        can_reply: item.kind == CandidateKind::Message && item.conversation.can_post,
      })
    })
    .map(|notification| limited_to(notification, &mode))
    .collect()
}

fn is_urgent(backend: &dyn crate::backend::Backend, message: &Message) -> bool {
  backend
    .message_extras(message.conversation, &[message.id])
    .first()
    .is_some_and(|extras| extras.priority == MessagePriority::Urgent)
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
      can_reply: true,
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

  mod new_messages {
    use super::*;
    use crate::model::{MessageKind, raw_id};

    #[test]
    fn a_known_conversation_reports_what_came_after_the_last_look() {
      let window = vec![message(5), message(6), message(7)];
      assert_eq!(ids(new_messages(window, Some(id_from_raw(5)), None, 0)), [6, 7]);
    }

    #[test]
    fn the_first_look_only_records_the_state() {
      let window = vec![message(5), message(6)];
      assert_eq!(ids(new_messages(window, None, None, 2)), Vec::<u64>::new());
    }

    #[test]
    fn a_new_conversation_reports_its_unread_messages_newer_than_everything_seen() {
      let window = vec![message(3), message(8), message(9)];
      assert_eq!(
        ids(new_messages(window.clone(), None, Some(id_from_raw(7)), 5)),
        [8, 9]
      );
      assert_eq!(ids(new_messages(window, None, Some(id_from_raw(7)), 1)), [9]);
    }

    fn ids(messages: Vec<Message>) -> Vec<u64> {
      messages.iter().map(|message| raw_id(message.id)).collect()
    }

    fn message(raw: u64) -> Message {
      Message {
        id: id_from_raw(raw),
        conversation: id_from_raw(1),
        author: id_from_raw(2),
        kind: MessageKind::User,
        sent_at: 0,
        body: String::new(),
        is_edited: false,
        reactions: Vec::new(),
        reply_count: 0,
        maybe_thread: None,
      }
    }
  }

  mod withdrawal {
    use super::*;

    #[test]
    fn the_first_look_reports_every_read_conversation() {
      let watcher = ReadWatcher::default();
      assert_eq!(
        watcher.newly_read(&[(id_from_raw(1), 0), (id_from_raw(2), 3)]),
        vec![id_from_raw(1)]
      );
    }

    #[test]
    fn a_conversation_is_reported_once_when_its_unread_count_drops_to_zero() {
      let watcher = ReadWatcher::default();
      watcher.newly_read(&[(id_from_raw(1), 0), (id_from_raw(2), 3)]);
      assert_eq!(
        watcher.newly_read(&[(id_from_raw(1), 0), (id_from_raw(2), 3)]),
        Vec::new()
      );
      assert_eq!(
        watcher.newly_read(&[(id_from_raw(1), 0), (id_from_raw(2), 0)]),
        vec![id_from_raw(2)]
      );
      assert_eq!(
        watcher.newly_read(&[(id_from_raw(1), 0), (id_from_raw(2), 0)]),
        Vec::new()
      );
    }

    #[test]
    fn a_new_conversation_without_unread_messages_is_not_reported() {
      let watcher = ReadWatcher::default();
      watcher.newly_read(&[(id_from_raw(1), 2)]);
      assert_eq!(
        watcher.newly_read(&[(id_from_raw(1), 2), (id_from_raw(9), 0)]),
        Vec::new()
      );
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

  mod dnd {
    use super::*;
    use teamcraft_proto::{DndWindow, Weekday};

    #[test]
    fn a_schedule_window_silences() {
      let prefs = scheduled(vec![midday(vec![Weekday::Wednesday])]);
      assert_eq!(
        decide(&input(&prefs, ConversationKind::Direct, "hi")),
        Decision::Skip(SkipReason::DoNotDisturb)
      );
    }

    #[test]
    fn only_the_window_on_todays_weekday_counts() {
      // NOW is a Wednesday.
      let prefs = scheduled(vec![midday(vec![Weekday::Monday, Weekday::Thursday])]);
      assert_eq!(
        decide(&input(&prefs, ConversationKind::Direct, "hi")),
        Decision::Notify
      );
    }

    #[test]
    fn any_of_several_windows_silences() {
      let prefs = scheduled(vec![
        midday(vec![Weekday::Monday]),
        midday(vec![Weekday::Wednesday]),
      ]);
      assert_eq!(
        decide(&input(&prefs, ConversationKind::Direct, "hi")),
        Decision::Skip(SkipReason::DoNotDisturb)
      );
    }

    #[test]
    fn the_device_offset_places_the_window() {
      // 12:00 UTC is 15:00 at +180; the window ends at 14:00 local time.
      let prefs = scheduled(vec![midday(vec![Weekday::Wednesday])]);
      let mut candidate = input(&prefs, ConversationKind::Direct, "hi");
      candidate.utc_offset_minutes = 180;
      assert_eq!(decide(&candidate), Decision::Notify);
    }

    #[test]
    fn manual_do_not_disturb_silences_until_it_ends() {
      let prefs = NotificationPrefs::default();
      let mut candidate = input(&prefs, ConversationKind::Direct, "hi");
      let me = in_dnd(candidate.me);
      candidate.me = &me;
      assert_eq!(decide(&candidate), Decision::Skip(SkipReason::DoNotDisturb));
      candidate.maybe_dnd_until = Some(NOW + 1);
      assert_eq!(decide(&candidate), Decision::Skip(SkipReason::DoNotDisturb));
      candidate.maybe_dnd_until = Some(NOW);
      assert_eq!(decide(&candidate), Decision::Notify);
    }

    #[test]
    fn allowed_people_break_through_schedule_and_manual_dnd() {
      let mut prefs = scheduled(vec![midday(vec![Weekday::Wednesday])]);
      prefs.dnd.allow_user_ids = vec![id_from_raw(2)];
      let mut candidate = input(&prefs, ConversationKind::Direct, "hi");
      assert_eq!(decide(&candidate), Decision::Notify);
      let me = in_dnd(candidate.me);
      candidate.me = &me;
      assert_eq!(decide(&candidate), Decision::Notify);
      candidate.sender = id_from_raw(3);
      assert_eq!(decide(&candidate), Decision::Skip(SkipReason::DoNotDisturb));
    }

    #[test]
    fn allowed_people_break_through_manual_dnd_without_a_schedule() {
      let mut prefs = NotificationPrefs::default();
      prefs.dnd.allow_user_ids = vec![id_from_raw(2)];
      let mut candidate = input(&prefs, ConversationKind::Direct, "hi");
      let me = in_dnd(candidate.me);
      candidate.me = &me;
      assert_eq!(decide(&candidate), Decision::Notify);
    }

    #[test]
    fn urgent_messages_break_through_only_when_allowed() {
      let quiet = scheduled(vec![midday(vec![Weekday::Wednesday])]);
      let mut candidate = input(&quiet, ConversationKind::Direct, "hi");
      candidate.is_urgent = true;
      assert_eq!(decide(&candidate), Decision::Skip(SkipReason::DoNotDisturb));
      let mut urgent = quiet.clone();
      urgent.dnd.allows_urgent = true;
      candidate.prefs = &urgent;
      assert_eq!(decide(&candidate), Decision::Notify);
      candidate.is_urgent = false;
      assert_eq!(decide(&candidate), Decision::Skip(SkipReason::DoNotDisturb));
    }

    #[test]
    fn breaking_through_dnd_still_respects_mutes() {
      let mut prefs = scheduled(vec![midday(vec![Weekday::Wednesday])]);
      prefs.dnd.allows_urgent = true;
      prefs.set_conversation(
        &conversation(ConversationKind::Direct).id.to_string(),
        ConversationNotify {
          maybe_level: None,
          mute: MuteFor::Forever,
        },
      );
      let mut candidate = input(&prefs, ConversationKind::Direct, "hi");
      candidate.is_urgent = true;
      assert_eq!(decide(&candidate), Decision::Skip(SkipReason::Muted));
    }

    fn scheduled(windows: Vec<DndWindow>) -> NotificationPrefs {
      NotificationPrefs {
        dnd: DndSchedule {
          is_enabled: true,
          windows,
          ..DndSchedule::default()
        },
        ..NotificationPrefs::default()
      }
    }

    /// 10:00 to 14:00 local time.
    fn midday(weekdays: Vec<Weekday>) -> DndWindow {
      DndWindow {
        weekdays,
        start_minute: 10 * 60,
        end_minute: 14 * 60,
      }
    }

    fn in_dnd(me: &User) -> User {
      User {
        presence: PresenceStatus::DoNotDisturb,
        ..me.clone()
      }
    }
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
      is_urgent: false,
      utc_offset_minutes: 0,
      maybe_dnd_until: None,
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
      device_setup: crate::model::DeviceSetup::Unknown,
      maybe_email: None,
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
      is_catching_up: false,
      is_notes_to_self: false,
      maybe_image_version: None,
    }
  }
}
