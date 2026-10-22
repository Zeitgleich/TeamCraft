//! The data boundary of the UI. Components only talk to [`ChatBackend`]; the mock implementation in
//! [`crate::mock`] is replaced by an adapter over `teamcraft-core` once the client core exists.
//!
//! The trait is synchronous on purpose: it models the *local, already decrypted* store of the client
//! core. Network and decryption happen below it and arrive as store updates, which the UI picks up by
//! re-reading after a revision bump (see [`crate::state::AppState::revision`]).

use crate::model::{
  Conversation, ConversationId, CustomStatus, Message, MessageId, PresenceStatus, Space, SpaceId,
  ThreadSummary, User, UserId,
};

pub trait ChatBackend {
  /// The signed-in user, including the presence and custom status they chose.
  fn me(&self) -> User;
  fn user(&self, id: UserId) -> Option<User>;
  fn users(&self) -> Vec<User>;

  fn spaces(&self) -> Vec<Space>;
  /// Channels of a space the user is a member of.
  fn space_conversations(&self, space: SpaceId) -> Vec<Conversation>;
  /// DMs and group DMs; they live outside spaces.
  fn direct_conversations(&self) -> Vec<Conversation>;
  fn conversation(&self, id: ConversationId) -> Option<Conversation>;

  /// Number of top-level messages (thread replies excluded).
  fn message_count(&self, conversation: ConversationId) -> usize;
  /// Top-level messages `start..start + len`, oldest first. Out-of-range parts are clipped.
  fn messages(&self, conversation: ConversationId, start: usize, len: usize) -> Vec<Message>;
  /// The thread root followed by its replies, oldest first.
  fn thread(&self, conversation: ConversationId, root: MessageId) -> Vec<Message>;
  fn followed_threads(&self) -> Vec<ThreadSummary>;

  /// Posts a message, as a thread reply when `maybe_root` is set.
  fn post_message(&self, conversation: ConversationId, maybe_root: Option<MessageId>, body: &str);
  fn toggle_reaction(&self, conversation: ConversationId, message: MessageId, emoji: &str);
  fn mark_read(&self, conversation: ConversationId);
  fn set_favorite(&self, conversation: ConversationId, is_favorite: bool);

  fn set_presence(&self, presence: PresenceStatus);
  fn set_custom_status(&self, maybe_status: Option<CustomStatus>);
}
