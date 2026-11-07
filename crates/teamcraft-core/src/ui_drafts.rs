//! The "Drafts & sent" view of the UI adapter (#1220): the drafts from the encrypted account data,
//! the user's own messages from the decrypted local store. The server sees nothing of either.

use std::cmp::Reverse;
use std::collections::{HashMap, VecDeque};

use teamcraft_client_api::drafts::{DraftItem, SentItem};
use teamcraft_client_api::model::{ConversationId, Id, MessageId, SpaceId, ThreadId, UserId};
use teamcraft_proto::ConversationResponse;

use crate::client::stored_account_data;
use crate::error::CoreError;
use crate::store::{LocalStore, MessageContent, MessageEdge, MessageFilter, StoredMessage};
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
      .map(|(draft, edited_at, response)| DraftItem {
        conversation: draft.conversation_id,
        maybe_thread: draft.maybe_thread_id,
        conversation_name: self
          .conversation_view(response, &users, UnreadCounts::default())
          .name,
        maybe_thread_root: draft
          .maybe_thread_id
          .and_then(|thread| self.thread_root_excerpt(draft.conversation_id, thread)),
        file_count: u32::try_from(data.draft_files(draft.conversation_id, draft.maybe_thread_id).len())
          .unwrap_or(u32::MAX),
        body: draft.body,
        edited_at,
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
    let mut names: HashMap<ConversationId, String> = HashMap::new();
    for item in &mut items {
      if !names.contains_key(&item.conversation)
        && let Some(response) = responses.iter().find(|response| response.id == item.conversation)
      {
        let name = self
          .conversation_view(response, &users, UnreadCounts::default())
          .name;
        names.insert(item.conversation, name);
      }
      if let Some(name) = names.get(&item.conversation) {
        item.conversation_name.clone_from(name);
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
    message,
    maybe_thread: stored.maybe_thread_id,
    excerpt,
    file_count: u32::try_from(file_count).unwrap_or(u32::MAX),
    sent_at: stored.sent_at_ms,
  })
}
