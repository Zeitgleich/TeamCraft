//! The activity feed of the UI adapter: mentions, reactions to the user's messages and replies in
//! their threads, built from the decrypted local messages (the server never sees content).
//!
//! Whether an entry is unread is not stored anywhere: it follows from the read position of its
//! target, the conversation's or the thread's (notifications.md, "Activity feed"). Reading
//! the message anywhere, on any device, therefore clears the entry.

use std::collections::{HashMap, HashSet};

use teamcraft_client_api::activity::{ActivityFeed, ActivityItem, ActivityKind};
use teamcraft_client_api::model::{ConversationId, Id, MessageId, ThreadId, UserId};
use teamcraft_proto::ClientMessageId;

use crate::imported::Authorship;
use crate::read_state;
use crate::readable_text::readable_text;
use crate::store::{LocalStore, MessageContent, StoreError, StoredMessage, Written};
use crate::ui_backend::{CoreChatBackend, CoreCommand, ui_message_id};

/// Entries kept in the feed.
const MAX_ITEMS: usize = 200;
/// Characters of the message text shown in an entry.
const EXCERPT_CHARS: usize = 140;

/// What the server says the user has read in one conversation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConversationReads {
  /// The conversation's read position.
  pub maybe_last_read: Option<MessageId>,
  /// The server's counts for the conversation are zero: every top-level message is read, which
  /// holds on this device before the new position itself arrived.
  pub is_caught_up: bool,
  /// Unread replies per followed thread (the thread's own read position, as a count).
  pub thread_unread: HashMap<ThreadId, u32>,
}

impl<S: LocalStore> CoreChatBackend<S> {
  pub(crate) fn ui_activity(&self) -> ActivityFeed {
    let unread = self.read(read_state::unread(self.store_ref()));
    let mut threads: HashMap<ConversationId, HashMap<ThreadId, u32>> = HashMap::new();
    for thread in self.read(read_state::threads(self.store_ref())) {
      threads
        .entry(thread.conversation_id)
        .or_default()
        .insert(thread.thread_id, thread.unread_count);
    }
    let responses: HashMap<_, _> = self
      .conversations_raw()
      .into_iter()
      .map(|response| (response.id, response))
      .collect();
    let ids = self.conversation_ids();
    let maybe_importer = self.importer();
    let mut cache = self.activity.borrow_mut();
    cache.retain(|conversation, _| ids.contains(conversation));
    let mut items = Vec::new();
    for conversation in ids {
      let reads = ConversationReads {
        maybe_last_read: responses
          .get(&conversation)
          .and_then(|response| response.maybe_my_membership.as_ref())
          .and_then(|membership| membership.maybe_last_read_message_id),
        is_caught_up: unread.as_ref().is_some_and(|unread| {
          unread
            .conversations
            .iter()
            .all(|entry| entry.conversation_id != conversation)
        }),
        thread_unread: threads.remove(&conversation).unwrap_or_default(),
      };
      // The entries follow from the messages and the read state alone: a conversation neither
      // changed is not read again (issue #1000: this read every message of every conversation).
      let maybe_revision = self.store_ref().message_revision(conversation);
      if let (Some(revision), Some(kept)) = (maybe_revision, cache.get(&conversation))
        && kept.revision == revision
        && kept.reads == reads
      {
        items.extend(kept.items.iter().cloned());
        continue;
      }
      let maybe_relevant = cache.remove(&conversation).map(|kept| kept.relevant);
      let relevant = self.relevant_messages(conversation, maybe_relevant);
      let is_bot = |user| self.read(crate::integration_sources::is_known_bot(self.store_ref(), user));
      let fresh = items_of(
        self.me_id(),
        maybe_importer,
        &is_bot,
        &reads,
        conversation,
        &relevant.in_order(),
        relevant.maybe_newest_top_level,
      );
      if let Some(revision) = maybe_revision {
        cache.insert(
          conversation,
          KeptActivity {
            revision,
            reads,
            items: fresh.clone(),
            relevant,
          },
        );
      }
      items.extend(fresh);
    }
    drop(cache);
    feed_of(items).with_join_requests(self.ui_join_requests())
  }

  /// The messages of `conversation` entries can come from: those kept before, brought up to date
  /// with what the store's journal says was written since (one lookup per written message), or
  /// read whole when there are none or the journal can't tell.
  fn relevant_messages(
    &self,
    conversation: ConversationId,
    maybe_kept: Option<RelevantMessages>,
  ) -> RelevantMessages {
    let me = self.me_id();
    let store = self.store_ref();
    // Taken before reading: a write in between is followed again next time, which changes nothing.
    let maybe_cursor = store.written_cursor();
    if let (Some(mut kept), Some(cursor)) = (maybe_kept, maybe_cursor)
      && let Some(from) = kept.maybe_cursor
      && let Some(written) = store.written_since(conversation, from)
    {
      match kept.follow(me, store, conversation, &written) {
        Ok(true) => {
          kept.maybe_cursor = Some(cursor);
          return kept;
        },
        Ok(false) => {},
        Err(error) => tracing::debug!(%error, "local store read failed, reading the conversation again"),
      }
    }
    let mut relevant = RelevantMessages::default();
    self.with_all_messages(conversation, &mut |messages| {
      relevant = RelevantMessages::of(me, maybe_cursor, messages);
    });
    relevant
  }

  /// Reads every entry by moving the read positions of their targets: the conversation's for
  /// entries in the conversation, the thread's for entries in a followed thread (the shell that
  /// can reach the server's thread endpoint does those, [`Self::ui_activity`] shows the result).
  pub(crate) fn ui_mark_activity_read(&self) {
    let feed = self.ui_activity();
    let followed: HashSet<(ConversationId, ThreadId)> = self
      .read(read_state::threads(self.store_ref()))
      .into_iter()
      .map(|thread| (thread.conversation_id, thread.thread_id))
      .collect();
    let mut through: HashMap<ConversationId, MessageId> = HashMap::new();
    let mut thread_through: HashMap<(ConversationId, ThreadId), MessageId> = HashMap::new();
    for item in feed.items.iter().filter(|item| item.is_unread) {
      let Some(read_through) = item.maybe_read_through else {
        continue;
      };
      match item.maybe_thread {
        Some(thread) if followed.contains(&(item.conversation, thread)) => {
          let entry = thread_through
            .entry((item.conversation, thread))
            .or_insert(read_through);
          *entry = (*entry).max(read_through);
        },
        _ => {
          let entry = through.entry(item.conversation).or_insert(read_through);
          *entry = (*entry).max(read_through);
        },
      }
    }
    for (conversation, message) in through {
      self.command(CoreCommand::MarkReadThrough {
        conversation,
        message,
      });
    }
    for ((conversation, thread), message) in thread_through {
      self.command(CoreCommand::MarkThreadRead {
        conversation,
        thread,
        message,
      });
    }
  }
}

/// The feed for `me` over the stored messages and read state of the given conversations.
pub fn build_feed(
  me: UserId,
  maybe_importer: Option<UserId>,
  is_bot: &dyn Fn(UserId) -> bool,
  conversations: impl IntoIterator<Item = (ConversationId, ConversationReads, Vec<StoredMessage>)>,
) -> ActivityFeed {
  let mut items = Vec::new();
  for (conversation, reads, messages) in conversations {
    items.extend(conversation_items(
      me,
      maybe_importer,
      is_bot,
      &reads,
      conversation,
      &messages,
    ));
  }
  feed_of(items)
}

/// The newest [`MAX_ITEMS`] of `items`, newest first.
fn feed_of(mut items: Vec<ActivityItem>) -> ActivityFeed {
  items.sort_by_key(|item| std::cmp::Reverse(item.at));
  items.truncate(MAX_ITEMS);
  ActivityFeed::from_items(items)
}

fn conversation_items(
  me: UserId,
  maybe_importer: Option<UserId>,
  is_bot: &dyn Fn(UserId) -> bool,
  reads: &ConversationReads,
  conversation: ConversationId,
  messages: &[StoredMessage],
) -> Vec<ActivityItem> {
  let all: Vec<&StoredMessage> = messages.iter().collect();
  let maybe_newest_top_level = newest_top_level_of_others(me, &all);
  items_of(
    me,
    maybe_importer,
    is_bot,
    reads,
    conversation,
    &all,
    maybe_newest_top_level,
  )
}

/// The entries of one conversation from `messages` (in display order), which hold at least the
/// [`is_relevant`] ones; `maybe_newest_top_level` is [`newest_top_level_of_others`] of all of them.
fn items_of(
  me: UserId,
  maybe_importer: Option<UserId>,
  is_bot: &dyn Fn(UserId) -> bool,
  reads: &ConversationReads,
  conversation: ConversationId,
  messages: &[&StoredMessage],
  maybe_newest_top_level: Option<MessageId>,
) -> Vec<ActivityItem> {
  let read = ReadCheck::new(me, reads, messages, maybe_newest_top_level);
  let by_id: HashMap<_, _> = messages
    .iter()
    .map(|stored| (stored.client_message_id, *stored))
    .collect();
  let my_threads = threads_involving(me, messages);
  let mut items = Vec::new();

  // Per person, message and emoji only the newest reaction message counts (they toggle).
  let mut latest_reactions: HashMap<_, &StoredMessage> = HashMap::new();
  for stored in messages
    .iter()
    .copied()
    .filter(|stored| stored.sender_user_id != me)
  {
    match &stored.content {
      MessageContent::Text { body, mentions, .. } if mentions.contains(&me) => {
        items.push(item(
          stored,
          maybe_importer,
          is_bot,
          conversation,
          &read,
          ActivityKind::Mention,
          body,
        ));
      },
      MessageContent::Text { body, .. }
        if stored
          .maybe_thread_id
          .is_some_and(|thread| my_threads.contains(&thread)) =>
      {
        items.push(item(
          stored,
          maybe_importer,
          is_bot,
          conversation,
          &read,
          ActivityKind::ThreadReply,
          body,
        ));
      },
      MessageContent::Reaction { target, emoji, .. } => {
        let key = (*target, stored.sender_user_id, emoji.clone());
        if latest_reactions
          .get(&key)
          .is_none_or(|known| known.sent_at_ms <= stored.sent_at_ms)
        {
          latest_reactions.insert(key, stored);
        }
      },
      _ => {},
    }
  }

  for stored in latest_reactions.into_values() {
    let MessageContent::Reaction {
      target,
      emoji,
      is_added: true,
      ..
    } = &stored.content
    else {
      continue;
    };
    let Some(reacted_to) = by_id.get(target).filter(|message| message.sender_user_id == me) else {
      continue;
    };
    let Some(excerpt) = text_of(reacted_to) else {
      continue;
    };
    items.push(ActivityItem {
      id: format!("reaction:{}", stored.client_message_id.to_hex()),
      kind: ActivityKind::Reaction { emoji: emoji.clone() },
      conversation,
      message: ui_message_id(reacted_to),
      maybe_thread: reacted_to.maybe_thread_id,
      actor: stored.sender_user_id,
      maybe_source_name: None,
      at: stored.sent_at_ms,
      excerpt: shorten(&excerpt),
      maybe_read_through: stored.maybe_server_id,
      is_unread: !read.is_read(stored, reacted_to.maybe_thread_id),
    });
  }
  items
}

fn item(
  stored: &StoredMessage,
  maybe_importer: Option<UserId>,
  is_bot: &dyn Fn(UserId) -> bool,
  conversation: ConversationId,
  read: &ReadCheck,
  kind: ActivityKind,
  body: &str,
) -> ActivityItem {
  let prefix = match kind {
    ActivityKind::Mention => "mention",
    ActivityKind::Reaction { .. } => "reaction",
    ActivityKind::ThreadReply => "reply",
  };
  // Who wrote it and when, as the list shows it: the original author of an imported message.
  let authorship = Authorship::of(stored, maybe_importer);
  ActivityItem {
    id: format!("{prefix}:{}", stored.client_message_id.to_hex()),
    kind,
    conversation,
    message: ui_message_id(stored),
    maybe_thread: stored.maybe_thread_id,
    actor: authorship.author,
    maybe_source_name: match &stored.content {
      MessageContent::Text { meta, .. } if authorship.maybe_origin.is_none() => meta
        .maybe_source
        .as_deref()
        .and_then(|identity| {
          crate::integration_sources::shown_source(identity, is_bot(stored.sender_user_id))
        })
        .map(|shown| shown.name),
      _ => None,
    },
    at: authorship.sent_at_ms,
    excerpt: shorten(body),
    maybe_read_through: stored.maybe_server_id,
    is_unread: !read.is_read(stored, stored.maybe_thread_id),
  }
}

/// One conversation's entries, kept until its messages or read state change.
#[derive(Debug)]
pub(crate) struct KeptActivity {
  revision: u64,
  reads: ConversationReads,
  items: Vec<ActivityItem>,
  /// What the entries were built from, followed from here when the conversation changes.
  relevant: RelevantMessages,
}

/// The messages of one conversation an entry can come from or depend on ([`is_relevant`]), kept
/// with where the store's journal of writes stood when they were last brought up to date. A
/// message written later then costs one lookup instead of a read of the whole conversation: a
/// channel that an integration posts to every second was read whole every second, and a large one
/// doesn't fit the store's message cache.
#[derive(Debug, Default)]
pub(crate) struct RelevantMessages {
  /// `None`: the store keeps no journal (or the read failed); they are read whole next time.
  maybe_cursor: Option<u64>,
  messages: HashMap<ClientMessageId, StoredMessage>,
  /// [`newest_top_level_of_others`] of all of the conversation's messages, not only these.
  maybe_newest_top_level: Option<MessageId>,
}

impl RelevantMessages {
  /// The relevant ones of all of a conversation's `messages`, read after the journal stood at
  /// `maybe_cursor`.
  fn of(me: UserId, maybe_cursor: Option<u64>, messages: &[StoredMessage]) -> Self {
    let mut relevant = Self {
      maybe_cursor,
      ..Self::default()
    };
    for message in messages {
      relevant.note(me, message);
    }
    relevant
  }

  /// Brings them up to date with what the journal says was written: each message as the store
  /// holds it now, or gone. `false`: the journal can't tell which (all of the conversation's
  /// messages went), so they are read whole again.
  fn follow(
    &mut self,
    me: UserId,
    store: &impl LocalStore,
    conversation: ConversationId,
    written: &[Written],
  ) -> Result<bool, StoreError> {
    for write in written {
      let Written::Message(client_message_id) = write else {
        return Ok(false);
      };
      self.messages.remove(client_message_id);
      if let Some(message) = store.message(conversation, *client_message_id)? {
        self.note(me, &message);
      }
    }
    Ok(true)
  }

  /// Kept in display order, as the entries are built from them.
  fn in_order(&self) -> Vec<&StoredMessage> {
    let mut messages: Vec<&StoredMessage> = self.messages.values().collect();
    messages.sort_by_key(|message| message.order());
    messages
  }

  /// A deleted or edited message doesn't move [`Self::maybe_newest_top_level`] back: it only
  /// decides that the conversation is read up to there while the server counts nothing unread.
  fn note(&mut self, me: UserId, message: &StoredMessage) {
    if let Some(newest) = newest_top_level_of_others(me, &[message]) {
      self.maybe_newest_top_level = self.maybe_newest_top_level.max(Some(newest));
    }
    if is_relevant(me, message) {
      self.messages.insert(message.client_message_id, message.clone());
    }
  }
}

/// Decides from the read positions whether an entry was seen. The one rule: an entry is read when
/// the read position of its target (the thread of a thread message, else the conversation) is at
/// or after it.
struct ReadCheck {
  /// The conversation's read position, with the top-level messages the server counts as read.
  conversation_through: Option<MessageId>,
  /// Per followed thread: the oldest reply of others that is still unread (none: all read).
  thread_first_unread: HashMap<ThreadId, Option<MessageId>>,
}

impl ReadCheck {
  fn new(
    me: UserId,
    reads: &ConversationReads,
    messages: &[&StoredMessage],
    maybe_newest_top_level: Option<MessageId>,
  ) -> Self {
    let newest_top_level = reads.is_caught_up.then_some(maybe_newest_top_level).flatten();
    let thread_first_unread = reads
      .thread_unread
      .iter()
      .map(|(thread, unread)| {
        let mut others: Vec<MessageId> = messages
          .iter()
          .filter(|stored| stored.maybe_thread_id == Some(*thread) && stored.sender_user_id != me)
          .filter_map(|stored| stored.maybe_server_id)
          .collect();
        others.sort_unstable_by_key(|id| std::cmp::Reverse(*id));
        let unread = usize::try_from(*unread).unwrap_or(usize::MAX);
        // The newest `unread` replies are after the thread's read position. Fewer stored than
        // counted (history not loaded): the oldest stored one is the best known bound.
        let first_unread = if unread == 0 {
          None
        } else {
          others.get(unread.saturating_sub(1)).or(others.last()).copied()
        };
        (*thread, first_unread)
      })
      .collect();
    Self {
      conversation_through: reads.maybe_last_read.max(newest_top_level),
      thread_first_unread,
    }
  }

  /// `thread` is the thread the entry's target lives in.
  fn is_read(&self, entry: &StoredMessage, thread: Option<ThreadId>) -> bool {
    let Some(id) = entry.maybe_server_id else {
      return false;
    };
    match thread.and_then(|thread| self.thread_first_unread.get(&thread)) {
      Some(first_unread) => first_unread.is_none_or(|first| id < first),
      None => self.conversation_through.is_some_and(|through| id <= through),
    }
  }
}

/// Whether an entry can come from `message` or depend on it: the user's own messages (what others
/// react to, the threads they are in), mentions of the user, every thread message (replies, and
/// the replies a thread's read position counts) and reactions. Everything else (a channel's
/// ordinary top-level posts) matters only through [`newest_top_level_of_others`].
fn is_relevant(me: UserId, message: &StoredMessage) -> bool {
  message.sender_user_id == me
    || message.maybe_thread_id.is_some()
    || match &message.content {
      MessageContent::Text { mentions, .. } => mentions.contains(&me),
      MessageContent::Reaction { .. } => true,
      _ => false,
    }
}

/// The newest top-level message of others with a server ID: read once the server counts nothing
/// unread in the conversation.
fn newest_top_level_of_others(me: UserId, messages: &[&StoredMessage]) -> Option<MessageId> {
  messages
    .iter()
    .filter(|stored| stored.maybe_thread_id.is_none() && stored.sender_user_id != me)
    .filter_map(|stored| stored.maybe_server_id)
    .max()
}

/// Threads the user started or replied in: replies there concern them.
fn threads_involving(me: UserId, messages: &[&StoredMessage]) -> HashSet<ThreadId> {
  let mut threads: HashSet<ThreadId> = messages
    .iter()
    .filter(|stored| stored.sender_user_id == me && !stored.is_hidden())
    .filter_map(|stored| stored.maybe_thread_id)
    .collect();
  threads.extend(
    messages
      .iter()
      .filter(|stored| stored.sender_user_id == me && !stored.is_hidden())
      .filter_map(|stored| stored.maybe_server_id)
      .map(|server_id| ThreadId::from_snowflake(server_id.snowflake())),
  );
  threads
}

/// The text of a message that can be reacted to; `None` for deleted ones.
fn text_of(stored: &StoredMessage) -> Option<String> {
  match &stored.content {
    MessageContent::Text { body, meta, .. } if body.is_empty() && !meta.attachments.is_empty() => Some(
      meta
        .attachments
        .iter()
        .map(|attachment| attachment.file_name.clone())
        .collect::<Vec<_>>()
        .join(" "),
    ),
    MessageContent::Text { body, .. } => Some(body.clone()),
    MessageContent::File { attachment } => Some(attachment.file_name.clone()),
    _ => None,
  }
}

pub(crate) fn shorten(text: &str) -> String {
  let single_line = readable_text(text)
    .split_whitespace()
    .collect::<Vec<_>>()
    .join(" ");
  if single_line.chars().count() <= EXCERPT_CHARS {
    return single_line;
  }
  let mut shortened: String = single_line.chars().take(EXCERPT_CHARS).collect();
  shortened.push('…');
  shortened
}

#[cfg(test)]
mod tests {
  use teamcraft_client_api::model::id_from_raw;
  use teamcraft_proto::ClientMessageId;

  use super::*;
  use crate::store::MessageMeta;

  const ME: u64 = 1;
  const ANNA: u64 = 2;
  const BEN: u64 = 3;

  mod mentions {
    use super::*;

    #[test]
    fn a_mention_by_somebody_else_is_listed_but_not_my_own_or_other_mentions() {
      let feed = feed(
        0,
        vec![
          text(1, ANNA, 100, "hi @me", &[ME], None),
          text(2, ANNA, 110, "hi @ben", &[BEN], None),
          text(3, ME, 120, "note to @me", &[ME], None),
        ],
      );
      assert_eq!(kinds(&feed), [ActivityKind::Mention]);
      assert_eq!(feed.items[0].actor, user(ANNA));
      assert_eq!(feed.items[0].excerpt, "hi @me");
    }

    #[test]
    fn a_mention_in_a_reply_stays_a_mention_and_keeps_its_thread() {
      let feed = feed(
        0,
        vec![
          text(1, ME, 100, "root", &[], None),
          reply(2, ANNA, 110, "@me look", &[ME], 1),
        ],
      );
      assert_eq!(kinds(&feed), [ActivityKind::Mention]);
      assert_eq!(feed.items[0].maybe_thread, Some(thread(1)));
    }
  }

  mod reactions {
    use super::*;

    #[test]
    fn a_reaction_to_my_message_is_listed_with_the_message_to_jump_to() {
      let feed = feed(
        0,
        vec![
          text(1, ME, 100, "ship it", &[], None),
          reaction(2, ANNA, 110, 1, "🎉", true),
        ],
      );
      assert_eq!(kinds(&feed), [ActivityKind::Reaction { emoji: "🎉".into() }]);
      assert_eq!(feed.items[0].message, message_id(1));
      assert_eq!(feed.items[0].excerpt, "ship it");
    }

    #[test]
    fn reactions_to_other_peoples_messages_and_my_own_are_ignored() {
      let feed = feed(
        0,
        vec![
          text(1, ANNA, 100, "hello", &[], None),
          text(2, ME, 105, "mine", &[], None),
          reaction(3, BEN, 110, 1, "👍", true),
          reaction(4, ME, 120, 2, "👍", true),
        ],
      );
      assert!(feed.items.is_empty());
    }

    #[test]
    fn a_removed_reaction_leaves_nothing_and_a_re_added_one_counts_once() {
      let removed = feed(
        0,
        vec![
          text(1, ME, 100, "x", &[], None),
          reaction(2, ANNA, 110, 1, "👍", true),
          reaction(3, ANNA, 120, 1, "👍", false),
        ],
      );
      assert!(removed.items.is_empty());

      let again = feed(
        0,
        vec![
          text(1, ME, 100, "x", &[], None),
          reaction(2, ANNA, 110, 1, "👍", true),
          reaction(3, ANNA, 120, 1, "👍", false),
          reaction(4, ANNA, 130, 1, "👍", true),
        ],
      );
      assert_eq!(again.items.len(), 1);
      assert_eq!(again.items[0].at, 130);
    }

    #[test]
    fn a_reaction_to_a_reply_of_mine_points_into_the_thread() {
      let feed = feed(
        0,
        vec![
          text(1, ANNA, 100, "root", &[], None),
          reply(2, ME, 110, "my reply", &[], 1),
          reaction(3, BEN, 120, 2, "❤", true),
        ],
      );
      assert_eq!(feed.items[0].maybe_thread, Some(thread(1)));
      assert_eq!(feed.items[0].message, message_id(2));
    }
  }

  mod thread_replies {
    use super::*;

    #[test]
    fn replies_in_my_own_thread_or_one_i_replied_in_are_listed() {
      let feed = feed(
        0,
        vec![
          text(1, ME, 100, "my root", &[], None),
          reply(2, ANNA, 110, "a reply", &[], 1),
          text(3, ANNA, 120, "their root", &[], None),
          reply(4, ME, 130, "i join", &[], 3),
          reply(5, BEN, 140, "ben too", &[], 3),
          text(6, BEN, 150, "other root", &[], None),
          reply(7, ANNA, 160, "elsewhere", &[], 6),
        ],
      );
      let ids: Vec<_> = feed.items.iter().map(|item| item.message).collect();
      assert_eq!(ids, [message_id(5), message_id(2)]);
      assert!(
        feed
          .items
          .iter()
          .all(|item| item.kind == ActivityKind::ThreadReply)
      );
    }
  }

  mod unread {
    use super::*;

    #[test]
    fn entries_after_the_conversations_read_position_are_unread_and_the_feed_is_newest_first() {
      let feed = feed(
        1,
        vec![
          text(1, ANNA, 100, "old @me", &[ME], None),
          text(2, ANNA, 110, "new @me", &[ME], None),
        ],
      );
      let state: Vec<_> = feed.items.iter().map(|item| (item.at, item.is_unread)).collect();
      assert_eq!(state, [(110, true), (100, false)]);
      assert_eq!(feed.unread_count, 1);
    }

    #[test]
    fn a_reaction_is_read_once_the_conversation_is_read_past_it() {
      let messages = vec![
        text(1, ME, 100, "ship it", &[], None),
        reaction(2, ANNA, 110, 1, "🎉", true),
      ];
      assert_eq!(
        feed(1, messages.clone()).unread_count,
        1,
        "read only to the reacted message"
      );
      assert_eq!(feed(2, messages).unread_count, 0);
    }

    #[test]
    fn counts_of_zero_from_the_server_read_the_top_level_entries_before_the_position_arrives() {
      let messages = vec![
        text(1, ANNA, 100, "@me one", &[ME], None),
        text(2, ANNA, 110, "@me two", &[ME], None),
      ];
      let reads = |is_caught_up| ConversationReads {
        is_caught_up,
        ..ConversationReads::default()
      };
      assert_eq!(feed_with(reads(false), messages.clone()).unread_count, 2);
      assert_eq!(feed_with(reads(true), messages).unread_count, 0);
    }

    #[test]
    fn a_thread_reply_follows_the_threads_read_position_not_the_conversations() {
      let messages = vec![
        text(1, ME, 100, "root", &[], None),
        reply(2, ANNA, 110, "first", &[], 1),
        reply(3, ANNA, 120, "second", &[], 1),
        text(9, ANNA, 130, "later top level", &[], None),
      ];
      let reads = |unread, last_read| ConversationReads {
        maybe_last_read: Some(message_id(last_read)),
        thread_unread: HashMap::from([(thread(1), unread)]),
        ..ConversationReads::default()
      };
      let unread_of =
        |feed: &ActivityFeed| -> Vec<bool> { feed.items.iter().map(|item| item.is_unread).collect() };
      // Two unread replies although the conversation was read past both: the thread decides.
      assert_eq!(unread_of(&feed_with(reads(2, 9), messages.clone())), [true, true]);
      // One unread: only the newest.
      assert_eq!(
        unread_of(&feed_with(reads(1, 0), messages.clone())),
        [true, false]
      );
      assert_eq!(unread_of(&feed_with(reads(0, 0), messages)), [false, false]);
    }

    #[test]
    fn an_entry_says_how_far_to_read_to_clear_it() {
      let feed = feed(0, vec![text(4, ANNA, 100, "@me", &[ME], None)]);
      assert_eq!(feed.items[0].maybe_read_through, Some(message_id(4)));
    }

    #[test]
    fn long_texts_are_shortened_to_one_line() {
      let feed = feed(
        0,
        vec![text(
          1,
          ANNA,
          100,
          &format!("@me {}\nmore", "x".repeat(300)),
          &[ME],
          None,
        )],
      );
      assert!(feed.items[0].excerpt.ends_with('…'));
      assert!(!feed.items[0].excerpt.contains('\n'));
      assert_eq!(feed.items[0].excerpt.chars().count(), EXCERPT_CHARS + 1);
    }
  }

  #[test]
  fn an_imported_mention_names_its_original_author_and_time() {
    const IMPORTER: u64 = 9;
    let mut stored = text(1, IMPORTER, 100, "hi @me", &[ME], None);
    if let MessageContent::Text { meta, .. } = &mut stored.content {
      meta.maybe_imported = Some(Box::new(crate::imported::ImportedOrigin {
        source: crate::imported::ImportSource::Synology,
        source_id: "1".into(),
        author: crate::imported::ImportedAuthor::User { user_id: user(ANNA) },
        sent_at_ms: 1_600_000_000_000,
        maybe_edited_at_ms: None,
        reactions: Vec::new(),
        missing_files: Vec::new(),
      }));
    }
    let feed = build_feed(
      user(ME),
      Some(user(IMPORTER)),
      &|_| false,
      [(id_from_raw(50), ConversationReads::default(), vec![stored])],
    );
    assert_eq!(feed.items[0].actor, user(ANNA));
    assert_eq!(feed.items[0].at, 1_600_000_000_000);
  }

  #[test]
  fn a_source_name_reaches_the_feed_from_a_bot_only() {
    let mut stored = text(1, ANNA, 100, "hi @me", &[ME], None);
    if let MessageContent::Text { meta, .. } = &mut stored.content {
      meta.maybe_source = Some(Box::new(teamcraft_proto::SourceIdentity {
        maybe_id: None,
        name: "Hotline".into(),
        maybe_picture_sha256: None,
        maybe_picture: None,
      }));
    }
    let of = |is_bot: &dyn Fn(UserId) -> bool, stored: StoredMessage| {
      build_feed(
        user(ME),
        None,
        is_bot,
        [(id_from_raw(50), ConversationReads::default(), vec![stored])],
      )
      .items[0]
        .maybe_source_name
        .clone()
    };
    assert_eq!(of(&|_| false, stored.clone()), None);
    assert_eq!(of(&|_| true, stored).as_deref(), Some("Hotline"));
  }

  mod followed {
    use super::*;
    use crate::store::MemoryStore;

    #[test]
    fn following_the_writes_gives_the_entries_a_whole_read_gives() {
      let store = MemoryStore::new();
      let before = [
        text(1, ME, 100, "ship it", &[], None),
        text(2, ANNA, 110, "@me look", &[ME], None),
        text(3, BEN, 120, "a bot post", &[], None),
      ];
      write(&store, &before);
      let mut relevant = RelevantMessages::of(user(ME), Some(0), &before);

      let after = [
        reaction(4, ANNA, 130, 1, "🎉", true),
        reply(5, BEN, 140, "agreed", &[], 1),
        text(6, BEN, 150, "another bot post", &[], None),
        // An edit takes the mention out: the entry goes.
        text(2, ANNA, 160, "look", &[], None),
      ];
      write(&store, &after);
      let written: Vec<Written> = after
        .iter()
        .map(|message| Written::Message(message.client_message_id))
        .collect();
      assert!(
        relevant
          .follow(user(ME), &store, id_from_raw(50), &written)
          .unwrap()
      );

      let all = store
        .messages(id_from_raw(50), crate::store::MessageFilter::All)
        .unwrap();
      let reads = ConversationReads {
        is_caught_up: true,
        ..ConversationReads::default()
      };
      assert_eq!(
        sorted(followed_items(&reads, &relevant)),
        sorted(conversation_items(
          user(ME),
          None,
          &|_| false,
          &reads,
          id_from_raw(50),
          &all
        )),
      );
      assert_eq!(
        kinds(&ActivityFeed::from_items(followed_items(&reads, &relevant))).len(),
        2
      );
    }

    #[test]
    fn ordinary_posts_of_others_are_not_kept_but_count_for_the_read_position() {
      let posts: Vec<StoredMessage> = (1..=50)
        .map(|raw| text(raw, BEN, i64::from(raw), "a bot post", &[], None))
        .collect();
      let relevant = RelevantMessages::of(user(ME), Some(0), &posts);
      assert!(relevant.messages.is_empty());
      assert_eq!(relevant.maybe_newest_top_level, Some(message_id(50)));
    }

    #[test]
    fn a_conversation_whose_messages_all_went_is_read_whole_again() {
      let store = MemoryStore::new();
      let mut relevant = RelevantMessages::of(user(ME), Some(0), &[]);
      assert!(
        !relevant
          .follow(user(ME), &store, id_from_raw(50), &[Written::Conversation])
          .unwrap()
      );
    }

    fn write(store: &MemoryStore, messages: &[StoredMessage]) {
      for message in messages {
        store.upsert_message(message).unwrap();
      }
    }

    fn followed_items(reads: &ConversationReads, relevant: &RelevantMessages) -> Vec<ActivityItem> {
      items_of(
        user(ME),
        None,
        &|_| false,
        reads,
        id_from_raw(50),
        &relevant.in_order(),
        relevant.maybe_newest_top_level,
      )
    }

    fn sorted(mut items: Vec<ActivityItem>) -> Vec<ActivityItem> {
      items.sort_by(|a, b| a.id.cmp(&b.id));
      items
    }
  }

  /// The feed with the conversation read through message `read_through` (0: nothing read).
  fn feed(read_through: u64, messages: Vec<StoredMessage>) -> ActivityFeed {
    let reads = ConversationReads {
      maybe_last_read: (read_through > 0).then(|| message_id(read_through)),
      ..ConversationReads::default()
    };
    feed_with(reads, messages)
  }

  fn feed_with(reads: ConversationReads, messages: Vec<StoredMessage>) -> ActivityFeed {
    build_feed(user(ME), None, &|_| false, [(id_from_raw(50), reads, messages)])
  }

  fn kinds(feed: &ActivityFeed) -> Vec<ActivityKind> {
    feed.items.iter().map(|item| item.kind.clone()).collect()
  }

  fn user(raw: u64) -> UserId {
    id_from_raw(raw)
  }

  fn message_id(raw: u64) -> teamcraft_client_api::model::MessageId {
    id_from_raw(raw * 1000)
  }

  fn thread(raw: u64) -> ThreadId {
    id_from_raw(raw * 1000)
  }

  fn client_id(raw: u8) -> ClientMessageId {
    ClientMessageId::from_bytes([raw; 16])
  }

  fn stored(
    raw: u8,
    sender: u64,
    at: i64,
    content: MessageContent,
    maybe_thread: Option<u64>,
  ) -> StoredMessage {
    StoredMessage {
      conversation_id: id_from_raw(50),
      client_message_id: client_id(raw),
      maybe_server_id: Some(message_id(u64::from(raw))),
      sender_user_id: user(sender),
      maybe_thread_id: maybe_thread.map(thread),
      sent_at_ms: at,
      content,
      is_edited: false,
      edits: Vec::new(),
      maybe_edit_id: None,
      is_pinned: false,
      maybe_verified_commit: None,
      maybe_server_change: None,
      maybe_unreadable: None,
    }
  }

  fn text(raw: u8, sender: u64, at: i64, body: &str, mentions: &[u64], thread: Option<u64>) -> StoredMessage {
    stored(
      raw,
      sender,
      at,
      MessageContent::Text {
        body: body.into(),
        mentions: mentions.iter().map(|raw| user(*raw)).collect(),
        meta: MessageMeta::default(),
      },
      thread,
    )
  }

  fn reply(raw: u8, sender: u64, at: i64, body: &str, mentions: &[u64], root: u64) -> StoredMessage {
    text(raw, sender, at, body, mentions, Some(root))
  }

  fn reaction(raw: u8, sender: u64, at: i64, target: u8, emoji: &str, is_added: bool) -> StoredMessage {
    stored(
      raw,
      sender,
      at,
      MessageContent::Reaction {
        target: client_id(target),
        emoji: emoji.into(),
        is_added,
        maybe_custom: None,
      },
      None,
    )
  }
}
