//! Reactions of others to the user's own messages that the user hasn't seen yet: the conversation
//! view's "jump to reaction" button (issue #1009, notifications.md "Reactions to my messages").
//!
//! An index in the store, kept as reaction messages arrive: the gateway and the catch-up both store
//! them through `receive_message`, which calls [`note_received`]. Reading the count therefore looks
//! at the index entries of one conversation, never at its messages. One entry per message, person
//! and emoji: reactions toggle, and the newest reaction message wins in any arrival order (history
//! pages arrive newest first). A reaction that arrives before the message it reacts to waits until
//! that message arrives.
//!
//! Whether an entry is seen follows from the member's synced [`ReactionsSeen`] (any device) and
//! from this device's own mark, set at once while the server is told through the queue of pending
//! actions.

use serde::{Deserialize, Serialize};
#[cfg(feature = "ui")]
use teamcraft_client_api::model::UnseenReactions;
use teamcraft_proto::{ClientMessageId, ConversationId, MessageId, UserId};
#[cfg(feature = "ui")]
use teamcraft_proto::{ConversationResponse, ReactionsSeen};

use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, MessageContent, StoredMessage, Table};

/// The index's keys in [`Table::SyncState`]: `my_reactions/{conversation}/{target}/{user}/{emoji}`.
/// A write is a change of the conversation's state shown to the UI.
pub(crate) const INDEX_PREFIX: &str = "my_reactions/";
/// Reactions waiting for their target: `early_reactions/{conversation}/{target}/{reaction}`.
const EARLY_PREFIX: &str = "early_reactions/";

/// The newest reaction of one person with one emoji on one of the user's messages.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct IndexEntry {
  /// The reaction message.
  reaction: MessageId,
  is_added: bool,
  /// The message reacted to, as the list names it.
  target: MessageId,
  /// Seen on this device; the server may not know yet.
  #[serde(default)]
  is_seen_here: bool,
}

/// A reaction whose target this device hasn't stored yet.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct EarlyReaction {
  sender: UserId,
  emoji: String,
  is_added: bool,
}

/// Indexes a message the device just stored from the server: a reaction of somebody else to one
/// of `me`'s messages, and the reactions that waited for this message.
pub(crate) fn note_received(
  store: &impl LocalStore,
  me: UserId,
  stored: &StoredMessage,
) -> Result<(), CoreError> {
  let conversation = stored.conversation_id;
  if let (
    MessageContent::Reaction {
      target,
      emoji,
      is_added,
    },
    Some(reaction),
  ) = (&stored.content, stored.maybe_server_id)
    && stored.sender_user_id != me
  {
    match store.message(conversation, *target)? {
      Some(reacted_to) => index(
        store,
        me,
        &reacted_to,
        stored.sender_user_id,
        emoji,
        *is_added,
        reaction,
      )?,
      None => records::put_json(
        store,
        Table::SyncState,
        &early_key(conversation, *target, reaction),
        &EarlyReaction {
          sender: stored.sender_user_id,
          emoji: emoji.clone(),
          is_added: *is_added,
        },
      )?,
    }
  }
  resolve_early(store, me, stored)
}

/// The unseen reactions of a conversation: entries of others' added reactions that neither this
/// device nor (by the synced state) any other device saw.
#[cfg(feature = "ui")]
pub(crate) fn unseen(
  store: &impl LocalStore,
  conversation: ConversationId,
) -> Result<UnseenReactions, CoreError> {
  let seen = synced_state(store, conversation)?;
  let mut messages = Vec::new();
  let mut count = 0;
  for (_, entry) in entries(store, conversation)? {
    if is_unseen(&entry, &seen) {
      count += 1;
      messages.push(entry.target);
    }
  }
  messages.sort_unstable();
  messages.dedup();
  Ok(UnseenReactions { count, messages })
}

/// Marks the unseen reactions to `targets` as seen on this device and returns their reaction
/// messages, for the server. Entries the synced state covers by its `through` are dropped: a
/// reaction message up to it counts as seen whatever arrives later.
#[cfg(feature = "ui")]
pub(crate) fn mark_seen(
  store: &impl LocalStore,
  conversation: ConversationId,
  targets: &[MessageId],
) -> Result<Vec<MessageId>, CoreError> {
  let seen = synced_state(store, conversation)?;
  let mut reactions = Vec::new();
  for (key, mut entry) in entries(store, conversation)? {
    if seen
      .maybe_through
      .is_some_and(|through| entry.reaction <= through)
    {
      store.delete(Table::SyncState, &key)?;
    } else if is_unseen(&entry, &seen) && targets.contains(&entry.target) {
      entry.is_seen_here = true;
      records::put_json(store, Table::SyncState, &key, &entry)?;
      reactions.push(entry.reaction);
    }
  }
  Ok(reactions)
}

/// Drops the index and the waiting reactions of a conversation that went away.
pub(crate) fn forget(store: &impl LocalStore, conversation: ConversationId) -> Result<(), CoreError> {
  for prefix in [INDEX_PREFIX, EARLY_PREFIX] {
    for (key, _) in store.list_prefix(Table::SyncState, &format!("{prefix}{conversation}/"))? {
      store.delete(Table::SyncState, &key)?;
    }
  }
  Ok(())
}

fn index(
  store: &impl LocalStore,
  me: UserId,
  reacted_to: &StoredMessage,
  reactor: UserId,
  emoji: &str,
  is_added: bool,
  reaction: MessageId,
) -> Result<(), CoreError> {
  // The user's own listed messages: a thread reply isn't a row of the conversation view.
  let is_listed = reacted_to.maybe_thread_id.is_none() || reacted_to.is_also_in_channel();
  let Some(reaction_target) = reacted_to
    .maybe_server_id
    .filter(|_| is_listed && reacted_to.sender_user_id == me)
  else {
    return Ok(());
  };
  let key = index_key(
    reacted_to.conversation_id,
    reacted_to.client_message_id,
    reactor,
    emoji,
  );
  let maybe_known = records::get_json::<IndexEntry>(store, Table::SyncState, &key)?;
  if maybe_known.is_some_and(|known| known.reaction >= reaction) {
    return Ok(());
  }
  let entry = IndexEntry {
    reaction,
    is_added,
    target: reaction_target,
    is_seen_here: false,
  };
  records::put_json(store, Table::SyncState, &key, &entry)
}

/// Indexes the reactions that arrived before `stored`, which they react to.
fn resolve_early(store: &impl LocalStore, me: UserId, stored: &StoredMessage) -> Result<(), CoreError> {
  let prefix = format!(
    "{EARLY_PREFIX}{}/{}/",
    stored.conversation_id, stored.client_message_id
  );
  for (key, value) in store.list_prefix(Table::SyncState, &prefix)? {
    store.delete(Table::SyncState, &key)?;
    let maybe_reaction = key
      .rsplit('/')
      .next()
      .and_then(|raw| raw.parse::<MessageId>().ok());
    let (Some(reaction), Ok(early)) = (maybe_reaction, serde_json::from_slice::<EarlyReaction>(&value))
    else {
      // Damaged: the reaction is shown, only its button entry is lost.
      tracing::debug!("dropped an unreadable waiting reaction");
      continue;
    };
    index(
      store,
      me,
      stored,
      early.sender,
      &early.emoji,
      early.is_added,
      reaction,
    )?;
  }
  Ok(())
}

#[cfg(feature = "ui")]
fn entries(
  store: &impl LocalStore,
  conversation: ConversationId,
) -> Result<Vec<(String, IndexEntry)>, CoreError> {
  Ok(
    store
      .list_prefix(Table::SyncState, &format!("{INDEX_PREFIX}{conversation}/"))?
      .into_iter()
      .filter_map(|(key, value)| Some((key, serde_json::from_slice(&value).ok()?)))
      .collect(),
  )
}

#[cfg(feature = "ui")]
fn is_unseen(entry: &IndexEntry, seen: &ReactionsSeen) -> bool {
  entry.is_added && !entry.is_seen_here && !seen.is_seen(entry.reaction)
}

/// What the server last said the user saw in the conversation (from any device).
#[cfg(feature = "ui")]
fn synced_state(store: &impl LocalStore, conversation: ConversationId) -> Result<ReactionsSeen, CoreError> {
  Ok(
    records::get_json::<ConversationResponse>(store, Table::Conversations, &conversation.to_string())?
      .and_then(|response| response.maybe_my_membership)
      .map(|membership| membership.reactions_seen)
      .unwrap_or_default(),
  )
}

fn index_key(conversation: ConversationId, target: ClientMessageId, reactor: UserId, emoji: &str) -> String {
  let emoji_hex: String = emoji.bytes().map(|byte| format!("{byte:02x}")).collect();
  format!("{INDEX_PREFIX}{conversation}/{target}/{reactor}/{emoji_hex}")
}

fn early_key(conversation: ConversationId, target: ClientMessageId, reaction: MessageId) -> String {
  format!("{EARLY_PREFIX}{conversation}/{target}/{reaction}")
}

#[cfg(test)]
#[cfg(feature = "ui")]
mod tests {
  use teamcraft_client_api::model::id_from_raw;

  use super::*;
  use crate::store::{MemoryStore, MessageMeta};

  const ME: u64 = 1;
  const ANNA: u64 = 2;

  mod arrival_order {
    use super::*;

    #[test]
    fn a_reaction_before_its_message_waits_for_it() {
      let store = MemoryStore::new();
      receive(&store, &reaction(20, ANNA, 10, "🎉", true));
      assert_eq!(count(&store), 0);
      receive(&store, &text(10, ME));
      assert_eq!(
        unseen(&store, conversation()).unwrap().messages,
        vec![message_id(10)]
      );
      assert!(
        store
          .list_prefix(Table::SyncState, EARLY_PREFIX)
          .unwrap()
          .is_empty()
      );
    }

    #[test]
    fn the_newest_reaction_message_wins_in_any_order() {
      let store = MemoryStore::new();
      receive(&store, &text(10, ME));
      // History pages arrive newest first: the removal before the reaction it removes.
      receive(&store, &reaction(30, ANNA, 10, "🎉", false));
      receive(&store, &reaction(20, ANNA, 10, "🎉", true));
      assert_eq!(count(&store), 0);
      receive(&store, &reaction(40, ANNA, 10, "🎉", true));
      assert_eq!(count(&store), 1);
    }

    #[test]
    fn reactions_to_others_messages_or_by_me_are_not_kept() {
      let store = MemoryStore::new();
      receive(&store, &text(10, ANNA));
      receive(&store, &text(11, ME));
      receive(&store, &reaction(20, ANNA, 10, "🎉", true));
      receive(&store, &reaction(21, ME, 11, "🎉", true));
      assert_eq!(count(&store), 0);
      assert!(
        store
          .list_prefix(Table::SyncState, INDEX_PREFIX)
          .unwrap()
          .is_empty()
      );
    }
  }

  mod seen {
    use super::*;

    #[test]
    fn marking_counts_down_once_and_only_the_named_messages() {
      let store = MemoryStore::new();
      receive(&store, &text(10, ME));
      receive(&store, &text(11, ME));
      receive(&store, &reaction(20, ANNA, 10, "🎉", true));
      receive(&store, &reaction(21, ANNA, 11, "👍", true));
      assert_eq!(
        mark_seen(&store, conversation(), &[message_id(10)]).unwrap(),
        vec![message_id(20)]
      );
      assert!(
        mark_seen(&store, conversation(), &[message_id(10)])
          .unwrap()
          .is_empty()
      );
      assert_eq!(
        unseen(&store, conversation()).unwrap().messages,
        vec![message_id(11)]
      );
      // A later arrival of the same reaction message changes nothing.
      receive(&store, &reaction(20, ANNA, 10, "🎉", true));
      assert_eq!(count(&store), 1);
    }
  }

  fn receive(store: &MemoryStore, message: &StoredMessage) {
    store.upsert_message(message).unwrap();
    note_received(store, user(ME), message).unwrap();
  }

  fn count(store: &MemoryStore) -> usize {
    unseen(store, conversation()).unwrap().count
  }

  fn text(raw: u8, sender: u64) -> StoredMessage {
    stored(
      raw,
      sender,
      MessageContent::Text {
        body: "hello".into(),
        mentions: Vec::new(),
        meta: MessageMeta::default(),
      },
    )
  }

  fn reaction(raw: u8, sender: u64, target: u8, emoji: &str, is_added: bool) -> StoredMessage {
    stored(
      raw,
      sender,
      MessageContent::Reaction {
        target: client_id(target),
        emoji: emoji.into(),
        is_added,
      },
    )
  }

  fn stored(raw: u8, sender: u64, content: MessageContent) -> StoredMessage {
    StoredMessage {
      conversation_id: conversation(),
      client_message_id: client_id(raw),
      maybe_server_id: Some(message_id(raw)),
      sender_user_id: user(sender),
      maybe_thread_id: None,
      sent_at_ms: i64::from(raw),
      content,
      is_edited: false,
      edits: Vec::new(),
      maybe_edit_id: None,
      is_pinned: false,
      maybe_verified_commit: None,
      maybe_server_change: None,
    }
  }

  fn conversation() -> ConversationId {
    id_from_raw(50)
  }

  fn client_id(raw: u8) -> ClientMessageId {
    ClientMessageId::from_bytes([raw; 16])
  }

  fn message_id(raw: u8) -> MessageId {
    id_from_raw(u64::from(raw))
  }

  fn user(raw: u64) -> UserId {
    id_from_raw(raw)
  }
}
