//! The activity feed of the UI adapter: mentions, reactions to the user's messages and replies in
//! their threads, built from the decrypted local messages (the server never sees content). What the
//! user has read is account data, so it follows them across devices.

use std::collections::{HashMap, HashSet};

use teamcraft_client_api::activity::{ActivityFeed, ActivityItem, ActivityKind};
use teamcraft_client_api::model::{ConversationId, Id, ThreadId, UserId};

use crate::client::stored_account_data;
use crate::store::{LocalStore, MessageContent, StoredMessage};
use crate::ui_backend::{CoreChatBackend, CoreCommand, ui_message_id};

/// Entries kept in the feed.
const MAX_ITEMS: usize = 200;
/// Characters of the message text shown in an entry.
const EXCERPT_CHARS: usize = 140;

impl<S: LocalStore> CoreChatBackend<S> {
  pub(crate) fn ui_activity(&self) -> ActivityFeed {
    let read_at_ms = self
      .read(stored_account_data(self.store_ref()))
      .activity_read_at_ms();
    let conversations = self
      .conversation_ids()
      .into_iter()
      .map(|conversation| (conversation, self.stored(conversation)));
    build_feed(self.me_id(), read_at_ms, conversations)
  }

  pub(crate) fn ui_mark_activity_read(&self) {
    // Up to the newest entry on screen, not "now": another device's clock may differ.
    if let Some(newest) = self.ui_activity().items.first() {
      self.command(CoreCommand::MarkActivityRead { up_to_ms: newest.at });
    }
  }
}

/// The feed for `me` over the stored messages of the given conversations.
pub fn build_feed(
  me: UserId,
  read_at_ms: i64,
  conversations: impl IntoIterator<Item = (ConversationId, Vec<StoredMessage>)>,
) -> ActivityFeed {
  let mut items = Vec::new();
  for (conversation, messages) in conversations {
    items.extend(conversation_items(me, read_at_ms, conversation, &messages));
  }
  items.sort_by_key(|item| std::cmp::Reverse(item.at));
  items.truncate(MAX_ITEMS);
  ActivityFeed::from_items(items)
}

fn conversation_items(
  me: UserId,
  read_at_ms: i64,
  conversation: ConversationId,
  messages: &[StoredMessage],
) -> Vec<ActivityItem> {
  let by_id: HashMap<_, _> = messages
    .iter()
    .map(|stored| (stored.client_message_id, stored))
    .collect();
  let my_threads = threads_involving(me, messages);
  let mut items = Vec::new();

  // Per person, message and emoji only the newest reaction message counts (they toggle).
  let mut latest_reactions: HashMap<_, &StoredMessage> = HashMap::new();
  for stored in messages.iter().filter(|stored| stored.sender_user_id != me) {
    match &stored.content {
      MessageContent::Text { body, mentions, .. } if mentions.contains(&me) => {
        items.push(item(
          stored,
          conversation,
          read_at_ms,
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
          conversation,
          read_at_ms,
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
      at: stored.sent_at_ms,
      excerpt: shorten(&excerpt),
      is_unread: stored.sent_at_ms > read_at_ms,
    });
  }
  items
}

fn item(
  stored: &StoredMessage,
  conversation: ConversationId,
  read_at_ms: i64,
  kind: ActivityKind,
  body: &str,
) -> ActivityItem {
  let prefix = match kind {
    ActivityKind::Mention => "mention",
    ActivityKind::Reaction { .. } => "reaction",
    ActivityKind::ThreadReply => "reply",
  };
  ActivityItem {
    id: format!("{prefix}:{}", stored.client_message_id.to_hex()),
    kind,
    conversation,
    message: ui_message_id(stored),
    maybe_thread: stored.maybe_thread_id,
    actor: stored.sender_user_id,
    at: stored.sent_at_ms,
    excerpt: shorten(body),
    is_unread: stored.sent_at_ms > read_at_ms,
  }
}

/// Threads the user started or replied in: replies there concern them.
fn threads_involving(me: UserId, messages: &[StoredMessage]) -> HashSet<ThreadId> {
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
        .map(|attachment| format!("📎 {}", attachment.file_name))
        .collect::<Vec<_>>()
        .join(" "),
    ),
    MessageContent::Text { body, .. } => Some(body.clone()),
    MessageContent::File { attachment } => Some(format!("📎 {}", attachment.file_name)),
    _ => None,
  }
}

fn shorten(text: &str) -> String {
  let single_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
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
    fn entries_newer_than_the_read_mark_are_unread_and_the_feed_is_newest_first() {
      let feed = feed(
        105,
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

  fn feed(read_at_ms: i64, messages: Vec<StoredMessage>) -> ActivityFeed {
    build_feed(user(ME), read_at_ms, [(id_from_raw(50), messages)])
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
      },
      None,
    )
  }
}
