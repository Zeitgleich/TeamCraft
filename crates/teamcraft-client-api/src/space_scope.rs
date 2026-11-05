//! What the Calls, Saved and Files views show inside a space (#954). The rule is shared by the web,
//! desktop and mobile clients. It runs on the device, over what the device already holds: the server
//! is asked nothing new and learns nothing about what is looked at.

use std::cmp::Reverse;
use std::collections::HashSet;

use crate::actions::{FileInfo, SavedItem};
use crate::backend::ChatBackend;
use crate::calls::CallHistoryItem;
use crate::model::{ConversationId, SpaceId};

/// Files taken from one conversation (its newest); a conversation with thousands of attachments
/// must not decide how much work the view does.
pub const MAX_FILES_PER_CONVERSATION: usize = 100;

/// The calls in the conversations of `space` the user is in, in the backend's order.
pub fn calls_of_space(backend: &(impl ChatBackend + ?Sized), space: SpaceId) -> Vec<CallHistoryItem> {
  let conversations = conversations_of_space(backend, space);
  let mut calls = backend.call_history();
  calls.retain(|call| conversations.contains(&call.conversation));
  calls
}

/// The saved messages from the conversations of `space` the user is in, in the backend's order.
pub fn saved_items_of_space(backend: &(impl ChatBackend + ?Sized), space: SpaceId) -> Vec<SavedItem> {
  let conversations = conversations_of_space(backend, space);
  let mut items = backend.saved_items();
  items.retain(|item| conversations.contains(&item.message.conversation));
  items
}

/// The newest files of the conversations of `space` the user is in, newest first.
pub fn files_of_space(backend: &(impl ChatBackend + ?Sized), space: SpaceId) -> Vec<FileInfo> {
  let conversations = backend
    .space_conversations(space)
    .into_iter()
    .map(|conversation| conversation.id);
  newest_files(backend, conversations)
}

/// The newest files of every conversation the user is in, newest first.
pub fn files_of_everything(backend: &(impl ChatBackend + ?Sized)) -> Vec<FileInfo> {
  let mut conversations: Vec<ConversationId> = backend
    .direct_conversations()
    .into_iter()
    .map(|conversation| conversation.id)
    .collect();
  for space in backend.spaces() {
    conversations.extend(
      backend
        .space_conversations(space.id)
        .into_iter()
        .map(|conversation| conversation.id),
    );
  }
  newest_files(backend, conversations)
}

fn conversations_of_space(backend: &(impl ChatBackend + ?Sized), space: SpaceId) -> HashSet<ConversationId> {
  backend
    .space_conversations(space)
    .into_iter()
    .map(|conversation| conversation.id)
    .collect()
}

fn newest_files(
  backend: &(impl ChatBackend + ?Sized),
  conversations: impl IntoIterator<Item = ConversationId>,
) -> Vec<FileInfo> {
  let mut files: Vec<FileInfo> = conversations
    .into_iter()
    .flat_map(|conversation| {
      backend
        .conversation_files(conversation)
        .into_iter()
        .take(MAX_FILES_PER_CONVERSATION)
    })
    .collect();
  files.sort_by_key(|file| Reverse(file.maybe_sent_at));
  files
}
