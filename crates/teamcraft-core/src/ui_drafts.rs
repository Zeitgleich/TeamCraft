//! The "Drafts & sent" view of the UI adapter (#1220): the drafts from the encrypted account data,
//! the user's own messages from the decrypted local store. The server sees nothing of either.

use std::cmp::Reverse;
use std::collections::{HashMap, VecDeque};

use teamcraft_client_api::actions::ReplyPreview;
use teamcraft_client_api::drafts::{DraftItem, DraftReply, SentItem};
use teamcraft_client_api::model::{ConversationId, Id, MessageId, SpaceId, ThreadId, UserId};
use teamcraft_proto::ConversationResponse;

use crate::client::{reply_ref, stored_account_data};
use crate::error::CoreError;
use crate::records::{self, draft_reply_key};
use crate::store::{LocalStore, MessageContent, MessageEdge, MessageFilter, StoredMessage, Table};
use crate::ui_actions::reply_preview;
use crate::ui_activity::shorten;
use crate::ui_backend::{CoreChatBackend, UnreadCounts};

/// Drafts listed; a draft beyond them stays in its conversation's composer.
const MAX_DRAFTS: usize = 200;
/// Sent messages listed: "recent", not the whole history.
const MAX_SENT: usize = 200;

/// One conversation's own messages (its newest [`MAX_SENT`]), kept until its messages change.
pub(crate) struct KeptSent {
  revision: u64,
  items: Vec<SentItem>,
}

impl<S: LocalStore> CoreChatBackend<S> {
  /// What the composer of `conversation` replies to (#1115), as a quote; `None` once the message
  /// is gone, and then the record goes too.
  pub(crate) fn ui_draft_reply(&self, conversation: ConversationId) -> Option<ReplyPreview> {
    let key = draft_reply_key(conversation);
    let reply: DraftReply = self.read(records::get_json(self.store_ref(), Table::Account, &key))?;
    let maybe_target = self
      .find_stored(conversation, reply.message)
      .filter(|target| !matches!(target.content, MessageContent::Deleted));
    let Some(target) = maybe_target else {
      self.forget_draft_reply(&key);
      return None;
    };
    let quoted = reply_ref(&target, self.importer(), reply.maybe_attachment);
    Some(reply_preview(&quoted, Some(&target), self.importer()))
  }

  /// Written here, on the read thread, like a draft's files (`ui_attach_draft_files`): one record,
  /// and the composer has it before the next read. A write that fails costs only the reply.
  pub(crate) fn ui_set_draft_reply(&self, conversation: ConversationId, maybe_reply: Option<DraftReply>) {
    let key = draft_reply_key(conversation);
    let Some(reply) = maybe_reply else {
      self.forget_draft_reply(&key);
      return;
    };
    if let Err(error) = records::put_json(self.store_ref(), Table::Account, &key, &reply) {
      tracing::debug!(%error, "could not keep what the composer replies to");
    }
  }

  fn forget_draft_reply(&self, key: &str) {
    if let Err(error) = self.store_ref().delete(Table::Account, key) {
      tracing::debug!(%error, "could not forget what the composer replied to");
    }
  }

  /// Reads only the conversations a draft is in, and nothing at all without a draft.
  pub(crate) fn ui_drafts(&self, maybe_space: Option<SpaceId>) -> Vec<DraftItem> {
    let data = self.read(stored_account_data(self.store_ref()));
    let drafts = data.drafts_by_edit();
    if drafts.is_empty() {
      return Vec::new();
    }
    let responses: HashMap<ConversationId, ConversationResponse> = self
      .conversations_raw()
      .into_iter()
      .map(|response| (response.id, response))
      .collect();
    let users = self.summaries();
    drafts
      .into_iter()
      // A draft of a conversation the user left stays in the account data: rejoining finds it.
      .filter_map(|(draft, edited_at)| {
        let response = responses.get(&draft.conversation_id)?;
        Some((draft, edited_at, response))
      })
      .filter(|(_, _, response)| maybe_space.is_none() || response.maybe_space_id == maybe_space)
      .take(MAX_DRAFTS)
      .map(|(draft, edited_at, response)| {
        let named = self.conversation_view(response, &users, UnreadCounts::default());
        DraftItem {
          conversation: draft.conversation_id,
          maybe_thread: draft.maybe_thread_id,
          conversation_name: named.name,
          is_notes_to_self: named.is_notes_to_self,
          maybe_thread_root: draft
            .maybe_thread_id
            .and_then(|thread| self.thread_root_excerpt(draft.conversation_id, thread)),
          file_count: u32::try_from(data.draft_files(draft.conversation_id, draft.maybe_thread_id).len())
            .unwrap_or(u32::MAX),
          body: draft.body,
          edited_at,
        }
      })
      .collect()
  }

  /// The newest own messages across the conversations, newest first. Goes through the conversations
  /// from the one with the newest message and stops where none can hold anything newer than what
  /// is listed, so somebody who writes a lot reads few conversations; a conversation that didn't
  /// change since the last time is not read again.
  pub(crate) fn ui_sent_messages(&self, maybe_space: Option<SpaceId>) -> Vec<SentItem> {
    let responses = self.conversations_raw();
    let mut by_recency: Vec<(MessageId, &ConversationResponse)> = responses
      .iter()
      .filter(|response| maybe_space.is_none() || response.maybe_space_id == maybe_space)
      .filter_map(|response| {
        let newest = self
          .store_ref()
          .edge_server_id(response.id, MessageEdge::Newest)
          .map_err(CoreError::from);
        Some((self.read(newest)?, response))
      })
      .collect();
    by_recency.sort_by_key(|(newest, _)| Reverse(*newest));
    self
      .sent
      .borrow_mut()
      .retain(|conversation, _| responses.iter().any(|response| response.id == *conversation));

    let mut items: Vec<SentItem> = Vec::new();
    for (newest, response) in by_recency {
      if items.len() == MAX_SENT && items.last().is_some_and(|oldest| oldest.message > newest) {
        break;
      }
      items.extend(self.own_messages(response.id));
      items.sort_by_key(|item| Reverse(item.message));
      items.truncate(MAX_SENT);
    }
    self.with_conversation_names(items, &responses)
  }

  fn own_messages(&self, conversation: ConversationId) -> Vec<SentItem> {
    let maybe_revision = self.store_ref().message_revision(conversation);
    if let (Some(revision), Some(kept)) = (maybe_revision, self.sent.borrow().get(&conversation))
      && kept.revision == revision
    {
      return kept.items.clone();
    }
    let me = self.me_id();
    let mut newest: VecDeque<SentItem> = VecDeque::new();
    let visited = self
      .store_ref()
      .visit_messages(conversation, MessageFilter::All, &mut |stored| {
        if let Some(item) = sent_item(me, conversation, stored) {
          if newest.len() == MAX_SENT {
            newest.pop_front();
          }
          newest.push_back(item);
        }
      });
    self.read(visited.map_err(CoreError::from));
    let items: Vec<SentItem> = newest.into();
    if let Some(revision) = maybe_revision {
      self.sent.borrow_mut().insert(
        conversation,
        KeptSent {
          revision,
          items: items.clone(),
        },
      );
    }
    items
  }

  fn with_conversation_names(
    &self,
    mut items: Vec<SentItem>,
    responses: &[ConversationResponse],
  ) -> Vec<SentItem> {
    let users = self.summaries();
    let mut names: HashMap<ConversationId, (String, bool)> = HashMap::new();
    for item in &mut items {
      if !names.contains_key(&item.conversation)
        && let Some(response) = responses.iter().find(|response| response.id == item.conversation)
      {
        let named = self.conversation_view(response, &users, UnreadCounts::default());
        names.insert(item.conversation, (named.name, named.is_notes_to_self));
      }
      if let Some((name, is_notes_to_self)) = names.get(&item.conversation) {
        item.conversation_name.clone_from(name);
        item.is_notes_to_self = *is_notes_to_self;
      }
    }
    items
  }

  fn thread_root_excerpt(&self, conversation: ConversationId, thread: ThreadId) -> Option<String> {
    let root = self
      .store_ref()
      .message_by_server_id(conversation, MessageId::from_snowflake(thread.snowflake()))
      .map_err(CoreError::from);
    match self.read(root)?.content {
      MessageContent::Text { body, .. } if !body.is_empty() => Some(shorten(&body)),
      _ => None,
    }
  }
}

/// The list's row for a message, when the user sent it and the server took it: what still waits in
/// the outbox shows as pending in its conversation, not as sent.
fn sent_item(me: UserId, conversation: ConversationId, stored: &StoredMessage) -> Option<SentItem> {
  if stored.sender_user_id != me || stored.is_hidden() {
    return None;
  }
  let message = stored.maybe_server_id?;
  let (excerpt, file_count) = match &stored.content {
    MessageContent::Text { body, meta, .. } => (shorten(body), meta.attachments.len()),
    MessageContent::File { .. } => (String::new(), 1),
    _ => return None,
  };
  Some(SentItem {
    conversation,
    conversation_name: String::new(),
    is_notes_to_self: false,
    message,
    maybe_thread: stored.maybe_thread_id,
    excerpt,
    file_count: u32::try_from(file_count).unwrap_or(u32::MAX),
    sent_at: stored.sent_at_ms,
  })
}

#[cfg(test)]
mod reply_tests {
  use std::sync::Arc;

  use teamcraft_client_api::backend::ChatBackend;
  use teamcraft_proto::{ClientMessageId, Snowflake};

  use super::*;
  use crate::clock::ManualClock;
  use crate::store::{MemoryStore, TrackedStore};
  use crate::ui_backend::CoreChatBackend;

  /// A store that outlives the backend: the core reopened after the app was killed (#1115).
  #[test]
  fn a_reply_target_comes_back_after_a_restart_as_a_quote() {
    let store = Arc::new(MemoryStore::new());
    store.upsert_message(&message(5, "lunch at noon?")).unwrap();
    let before = backend(store.clone());
    before.set_draft_reply(conversation(), Some(reply(5)));
    drop(before);

    let quote = backend(store).draft_reply(conversation()).unwrap();

    assert_eq!(quote.maybe_message, Some(message_id(5)));
    assert_eq!(quote.author, user(3));
    assert_eq!(quote.excerpt, "lunch at noon?");
    assert_eq!(quote.maybe_attachment, None);
  }

  #[test]
  fn forgetting_the_target_leaves_nothing_to_restore() {
    let store = Arc::new(MemoryStore::new());
    store.upsert_message(&message(5, "lunch?")).unwrap();
    let backend = backend(store.clone());
    backend.set_draft_reply(conversation(), Some(reply(5)));

    backend.set_draft_reply(conversation(), None);

    assert_eq!(backend.draft_reply(conversation()), None);
    assert_eq!(kept_record(&store), None);
  }

  #[test]
  fn a_target_that_is_gone_is_forgotten() {
    let store = Arc::new(MemoryStore::new());
    let backend = backend(store.clone());
    backend.set_draft_reply(conversation(), Some(reply(5)));

    assert_eq!(
      backend.draft_reply(conversation()),
      None,
      "the store doesn't have it"
    );
    assert_eq!(kept_record(&store), None);
  }

  #[test]
  fn a_target_deleted_for_everyone_is_forgotten() {
    let store = Arc::new(MemoryStore::new());
    let mut deleted = message(5, "");
    deleted.content = MessageContent::Deleted;
    store.upsert_message(&deleted).unwrap();
    let backend = backend(store.clone());
    backend.set_draft_reply(conversation(), Some(reply(5)));

    assert_eq!(backend.draft_reply(conversation()), None);
    assert_eq!(kept_record(&store), None);
  }

  /// A store built for tracking, as the mobile apps run it: the record goes with the conversation.
  #[test]
  fn the_target_goes_with_its_conversation() {
    let store = TrackedStore::new(Arc::new(MemoryStore::new()));
    store
      .put(Table::Account, &draft_reply_key(conversation()), b"{}")
      .unwrap();

    store
      .delete(Table::Conversations, &conversation().to_string())
      .unwrap();

    assert_eq!(
      store
        .get(Table::Account, &draft_reply_key(conversation()))
        .unwrap(),
      None
    );
  }

  fn backend(store: Arc<MemoryStore>) -> CoreChatBackend<Arc<MemoryStore>> {
    CoreChatBackend::new(store, user(1), Box::new(ManualClock::new(0)))
  }

  fn kept_record(store: &MemoryStore) -> Option<Vec<u8>> {
    store
      .get(Table::Account, &draft_reply_key(conversation()))
      .unwrap()
  }

  fn reply(id: i64) -> DraftReply {
    DraftReply {
      message: message_id(id),
      maybe_attachment: None,
    }
  }

  fn message(id: i64, body: &str) -> StoredMessage {
    let client = u8::try_from(id).unwrap();
    StoredMessage {
      conversation_id: conversation(),
      client_message_id: ClientMessageId::from_bytes([client; 16]),
      maybe_server_id: Some(message_id(id)),
      sender_user_id: user(3),
      maybe_thread_id: None,
      sent_at_ms: id,
      content: MessageContent::Text {
        body: body.into(),
        mentions: Vec::new(),
        meta: Default::default(),
      },
      is_edited: false,
      edits: Vec::new(),
      maybe_edit_id: None,
      is_pinned: false,
      maybe_verified_commit: None,
      maybe_server_change: None,
      maybe_unreadable: None,
    }
  }

  fn conversation() -> ConversationId {
    ConversationId::from_snowflake(Snowflake::new(10).unwrap())
  }

  fn message_id(id: i64) -> MessageId {
    MessageId::from_snowflake(Snowflake::new(id).unwrap())
  }

  fn user(id: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(id).unwrap())
  }
}
