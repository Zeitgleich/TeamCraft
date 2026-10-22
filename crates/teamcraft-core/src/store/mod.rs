//! The local store (e2ee.md §9): everything the client knows, already decrypted.
//!
//! [`LocalStore`] is deliberately small – a typed key-value space plus an ordered message table –
//! so a native SQLCipher implementation, an encrypted IndexedDB one in the browser and an in-memory
//! one for tests stay interchangeable. Typed records on top live in [`crate::records`].

#[cfg(target_arch = "wasm32")]
mod browser;
mod key_source;
mod memory;
pub mod sealed;
#[cfg(all(feature = "sealed-sqlite", not(target_arch = "wasm32")))]
mod sealed_sqlite;
#[cfg(all(feature = "sqlcipher", not(target_arch = "wasm32")))]
mod sqlite;

#[cfg(target_arch = "wasm32")]
pub use browser::{IndexedDbRecords, IndexedDbStore, browser_database_key};
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
pub use key_source::KeyringKeySource;
pub use key_source::{DatabaseKeySource, KeySourceError, StaticKeySource};
pub use memory::MemoryStore;
#[cfg(all(feature = "sealed-sqlite", not(target_arch = "wasm32")))]
pub use sealed_sqlite::{SealedSqliteStore, SqliteRecords};
#[cfg(all(feature = "sqlcipher", not(target_arch = "wasm32")))]
pub use sqlite::SqlCipherStore;

use serde::{Deserialize, Serialize};
use teamcraft_proto::{ClientMessageId, ConversationId, MessageId, ThreadId, UserId};

/// Key-value tables of the store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Table {
  /// This device's account: user, device, keys (one record).
  Account,
  Spaces,
  Conversations,
  /// Member lists, keyed by conversation.
  Members,
  Users,
  /// Content key rings, keyed by conversation.
  ContentKeys,
  /// The openmls provider storage (MLS group state, key package private keys).
  MlsStorage,
  /// Applied membership change IDs per conversation (replay protection, crypto README).
  AppliedChanges,
  /// Messages written but not yet accepted by the server.
  Outbox,
  /// Gateway session, MLS backlog cursors, key bag version.
  SyncState,
  /// Identity keys pinned per user (trust on first use, e2ee.md §3).
  PinnedIdentities,
  /// Messages whose content key hasn't arrived yet; retried when keys arrive.
  Undecrypted,
  /// Last known presence per user (gateway `presence_update`).
  Presence,
  /// The verified admin roster and workspace policy.
  Workspace,
  /// Membership changes waiting for this device to commit them.
  PendingChanges,
  /// Decrypted shared search index segments, keyed `conversation/month`.
  SearchSegments,
  /// The user's saved searches.
  SavedSearches,
  /// Followed threads (`GET /threads`), keyed `conversation/thread`.
  Threads,
  /// Unread and mention counts (`GET /unread`), one record.
  Unread,
  /// How far other members of DMs have read, keyed `conversation/user`.
  ReadReceipts,
  /// Who acknowledged an urgent message, keyed `conversation/message`.
  MessageAcks,
  /// Files of outbox messages not uploaded yet, keyed like the outbox.
  PendingUploads,
  /// Download state of files on this device, keyed by attachment ID.
  FileTransfers,
  /// Share links this user created, keyed by attachment ID.
  ShareLinks,
  /// Outbox messages waiting for their link preview, keyed like the outbox.
  PendingPreviews,
  /// Categories per space (`GET /spaces/{id}/categories`), keyed by space.
  Categories,
  /// Public channels the user could join, per space (the channel browser).
  ChannelBrowser,
  /// Users' signed device lists, newest seen (security review A4).
  DeviceLists,
  /// Verified signing keys of devices, `{user}/{device}`, to check message signatures (A10).
  DeviceKeys,
  /// When a device left a conversation, `{conversation}/{user}/{device}` (A10).
  RemovedSenders,
}

/// A decrypted (or own, not yet sent) message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredMessage {
  pub conversation_id: ConversationId,
  pub client_message_id: ClientMessageId,
  /// Absent while the message waits in the outbox.
  #[serde(rename = "server_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_server_id: Option<MessageId>,
  pub sender_user_id: UserId,
  #[serde(rename = "thread_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_thread_id: Option<ThreadId>,
  pub sent_at_ms: i64,
  pub content: MessageContent,
  #[serde(default)]
  pub is_edited: bool,
  /// Earlier versions of an edited text, oldest first.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub edits: Vec<EditRecord>,
  #[serde(default)]
  pub is_pinned: bool,
}

/// A text that an edit replaced.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditRecord {
  pub body: String,
  pub replaced_at_ms: i64,
}

/// Plaintext inside a message ciphertext. Serialized as JSON before encryption, so new kinds can
/// be added; older clients keep them as [`MessageContent::Unknown`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageContent {
  Text {
    body: String,
    /// Users the message mentions; their IDs also go to the server in plaintext for notifications
    /// (e2ee.md §4.4).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    mentions: Vec<UserId>,
    /// Replies, priority, forwards, files, link previews; absent in a plain message.
    #[serde(default, skip_serializing_if = "MessageMeta::is_plain")]
    meta: MessageMeta,
  },
  /// An encrypted attachment in object storage (e2ee.md §4.5).
  File { attachment: AttachmentRef },
  /// Replaces the text of the sender's own earlier message.
  Edit { target: ClientMessageId, body: String },
  /// Withdraws the sender's own earlier message.
  Delete { target: ClientMessageId },
  /// What a deleted message turns into locally.
  Deleted,
  /// A published search index segment of this conversation and month (e2ee.md §10.1), encrypted
  /// and stored like an attachment.
  IndexSegment {
    month: String,
    segment: AttachmentRef,
    message_count: u32,
  },
  /// Pins or unpins a message of the conversation for everyone.
  Pin {
    target: ClientMessageId,
    is_pinned: bool,
  },
  /// Replaces the conversation's bookmarks bar for everyone (the newest message wins).
  Bookmarks { items: Vec<BookmarkItem> },
  /// A change to the conversation, announced by whoever made it (shown as a system message).
  Event { event: ConversationEvent },
  /// Adds or removes the sender's reaction to another message (e2ee.md §4.4: reactions are
  /// encrypted messages, so the server can't count them).
  Reaction {
    target: ClientMessageId,
    emoji: String,
    is_added: bool,
  },
  #[serde(other)]
  Unknown,
}

/// One entry of a channel's bookmarks bar: a link, or a message with a file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookmarkItem {
  pub emoji: String,
  pub title: String,
  /// The link; empty for a file bookmark.
  #[serde(default, skip_serializing_if = "String::is_empty")]
  pub url: String,
  /// The message whose file this bookmarks.
  #[serde(rename = "message", default, skip_serializing_if = "Option::is_none")]
  pub maybe_message: Option<ClientMessageId>,
}

/// What changed in a conversation; sent encrypted by the member who changed it, so every member's
/// history shows the same.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ConversationEvent {
  /// The sender joined (a public channel).
  Joined,
  /// The sender left.
  Left,
  MemberAdded {
    user: UserId,
    history: teamcraft_proto::HistoryPolicy,
  },
  MemberRemoved {
    user: UserId,
  },
  Renamed {
    name: String,
  },
  TopicChanged {
    topic: String,
  },
  Archived,
  Unarchived,
  MadePrivate,
  MadePublic,
  /// From a newer client.
  #[serde(other)]
  Unknown,
}

/// The optional parts of a text message. Everything is inside the ciphertext; only the urgency bit
/// is also told to the server (push routing).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageMeta {
  #[serde(rename = "reply_to", default, skip_serializing_if = "Option::is_none")]
  pub maybe_reply_to: Option<ReplyRef>,
  #[serde(default, skip_serializing_if = "Priority::is_normal")]
  pub priority: Priority,
  #[serde(default, skip_serializing_if = "is_false")]
  pub wants_acknowledgement: bool,
  /// Recipients' devices don't alert.
  #[serde(default, skip_serializing_if = "is_false")]
  pub is_silent: bool,
  /// A thread reply that also shows in the conversation.
  #[serde(default, skip_serializing_if = "is_false")]
  pub is_also_in_channel: bool,
  #[serde(rename = "forwarded", default, skip_serializing_if = "Option::is_none")]
  pub maybe_forwarded: Option<Forwarded>,
  /// Files sent with the text (a `File` message carries a single file without text).
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub attachments: Vec<AttachmentRef>,
  #[serde(rename = "link_preview", default, skip_serializing_if = "Option::is_none")]
  pub maybe_link_preview: Option<LinkPreviewData>,
  /// The sender's verified workspace head, added when the message is sent (security review A6).
  #[serde(rename = "workspace_head", default, skip_serializing_if = "Option::is_none")]
  pub maybe_workspace_head: Option<Box<crate::workspace::WorkspaceHead>>,
}

/// The quoted message: the reference, plus enough to show it where it isn't loaded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyRef {
  pub target: ClientMessageId,
  pub author: UserId,
  pub excerpt: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
  #[default]
  Normal,
  Important,
  Urgent,
  /// From a newer client; shown as normal.
  #[serde(other)]
  Unknown,
}

/// Where a forwarded message came from, and its text (the comment is the message body).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Forwarded {
  pub author_name: String,
  #[serde(
    rename = "conversation_name",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_conversation_name: Option<String>,
  pub sent_at_ms: i64,
  pub body: String,
}

/// The sender's link preview (e2ee.md §12).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkPreviewData {
  pub url: String,
  #[serde(rename = "title", default, skip_serializing_if = "Option::is_none")]
  pub maybe_title: Option<String>,
  #[serde(rename = "description", default, skip_serializing_if = "Option::is_none")]
  pub maybe_description: Option<String>,
  #[serde(rename = "site_name", default, skip_serializing_if = "Option::is_none")]
  pub maybe_site_name: Option<String>,
  #[serde(rename = "image", default, skip_serializing_if = "Option::is_none")]
  pub maybe_image: Option<teamcraft_proto::Base64Bytes>,
}

/// Everything needed to fetch and decrypt an attachment; travels inside the encrypted message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentRef {
  pub attachment_id: teamcraft_proto::AttachmentId,
  /// The per-file key (e2ee.md §4.5); only inside encrypted messages and the encrypted store.
  pub key: teamcraft_proto::Base64Bytes,
  /// SHA-256 of the encrypted blob, checked before decrypting.
  pub ciphertext_sha256: teamcraft_proto::Base64Bytes,
  pub file_name: String,
  pub media_type: String,
  pub size_bytes: u64,
  /// A small PNG preview of an image, made by the sender.
  #[serde(rename = "thumbnail", default, skip_serializing_if = "Option::is_none")]
  pub maybe_thumbnail: Option<teamcraft_proto::Base64Bytes>,
}

/// Which messages of a conversation to list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageFilter {
  /// Top-level messages (no thread replies, except those also sent to the conversation),
  /// reactions excluded.
  TopLevel,
  /// Replies of one thread, reactions excluded.
  Thread(ThreadId),
  /// Everything, reactions included.
  All,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
  #[error("database error: {0}")]
  Database(String),
  #[error("the database key doesn't open this database")]
  WrongKey,
  #[error("stored record is unreadable: {0}")]
  Corrupt(String),
  #[error("database key: {0}")]
  KeySource(#[from] KeySourceError),
}

/// Persistent client state. Implementations are internally synchronized.
pub trait LocalStore {
  fn get(&self, table: Table, key: &str) -> Result<Option<Vec<u8>>, StoreError>;
  fn put(&self, table: Table, key: &str, value: &[u8]) -> Result<(), StoreError>;
  fn delete(&self, table: Table, key: &str) -> Result<(), StoreError>;
  /// All entries of a table, ordered by key.
  fn list(&self, table: Table) -> Result<Vec<(String, Vec<u8>)>, StoreError>;

  /// Inserts or replaces a message, identified by conversation and client message ID.
  fn upsert_message(&self, message: &StoredMessage) -> Result<(), StoreError>;
  fn message(
    &self,
    conversation: ConversationId,
    client_message_id: ClientMessageId,
  ) -> Result<Option<StoredMessage>, StoreError>;
  /// Messages in display order: by server ID, outbox messages last in the order they were written.
  fn messages(
    &self,
    conversation: ConversationId,
    filter: MessageFilter,
  ) -> Result<Vec<StoredMessage>, StoreError>;
  fn delete_messages(&self, conversation: ConversationId) -> Result<(), StoreError>;
}

/// A store shared between the engine and readers (UI adapters, tests).
macro_rules! forward_local_store {
  ($pointer:ty) => {
    impl<T: LocalStore + ?Sized> LocalStore for $pointer {
      fn get(&self, table: Table, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        (**self).get(table, key)
      }

      fn put(&self, table: Table, key: &str, value: &[u8]) -> Result<(), StoreError> {
        (**self).put(table, key, value)
      }

      fn delete(&self, table: Table, key: &str) -> Result<(), StoreError> {
        (**self).delete(table, key)
      }

      fn list(&self, table: Table) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
        (**self).list(table)
      }

      fn upsert_message(&self, message: &StoredMessage) -> Result<(), StoreError> {
        (**self).upsert_message(message)
      }

      fn message(
        &self,
        conversation: ConversationId,
        client_message_id: ClientMessageId,
      ) -> Result<Option<StoredMessage>, StoreError> {
        (**self).message(conversation, client_message_id)
      }

      fn messages(
        &self,
        conversation: ConversationId,
        filter: MessageFilter,
      ) -> Result<Vec<StoredMessage>, StoreError> {
        (**self).messages(conversation, filter)
      }

      fn delete_messages(&self, conversation: ConversationId) -> Result<(), StoreError> {
        (**self).delete_messages(conversation)
      }
    }
  };
}

forward_local_store!(std::sync::Arc<T>);
// Single-threaded sharing in the browser, where the IndexedDB store is neither `Send` nor `Sync`.
forward_local_store!(std::rc::Rc<T>);

impl MessageMeta {
  pub fn is_plain(&self) -> bool {
    self == &Self::default()
  }
}

impl Priority {
  pub fn is_normal(&self) -> bool {
    matches!(self, Self::Normal)
  }
}

impl MessageContent {
  /// A plain text message.
  pub fn text(body: impl Into<String>, mentions: Vec<UserId>) -> Self {
    Self::Text {
      body: body.into(),
      mentions,
      meta: MessageMeta::default(),
    }
  }
}

fn is_false(value: &bool) -> bool {
  !*value
}

impl StoredMessage {
  /// Display order: server messages by ID; pending ones after all of them, by time written.
  pub fn sort_key(&self) -> (u8, i64) {
    match self.maybe_server_id {
      Some(id) => (0, teamcraft_proto::Id::snowflake(id).get()),
      None => (1, self.sent_at_ms),
    }
  }

  pub fn is_reaction(&self) -> bool {
    matches!(self.content, MessageContent::Reaction { .. })
  }

  /// Reactions, edits, deletes and pins act on other messages and aren't listed themselves.
  pub fn is_hidden(&self) -> bool {
    matches!(
      self.content,
      MessageContent::Reaction { .. }
        | MessageContent::Edit { .. }
        | MessageContent::Delete { .. }
        | MessageContent::Pin { .. }
        | MessageContent::Bookmarks { .. }
        | MessageContent::IndexSegment { .. }
    )
  }

  /// A thread reply the sender also posted to the conversation.
  pub fn is_also_in_channel(&self) -> bool {
    matches!(&self.content, MessageContent::Text { meta, .. } if meta.is_also_in_channel)
  }

  fn matches(&self, filter: MessageFilter) -> bool {
    match filter {
      MessageFilter::All => true,
      MessageFilter::TopLevel => {
        !self.is_hidden() && (self.maybe_thread_id.is_none() || self.is_also_in_channel())
      },
      MessageFilter::Thread(thread) => !self.is_hidden() && self.maybe_thread_id == Some(thread),
    }
  }
}

impl Table {
  /// Every table; a new variant goes here too (stores that load by name skip unknown tables).
  pub const ALL: [Self; 30] = [
    Self::Account,
    Self::Spaces,
    Self::Conversations,
    Self::Members,
    Self::Users,
    Self::ContentKeys,
    Self::MlsStorage,
    Self::AppliedChanges,
    Self::Outbox,
    Self::SyncState,
    Self::PinnedIdentities,
    Self::Undecrypted,
    Self::Presence,
    Self::Workspace,
    Self::PendingChanges,
    Self::SearchSegments,
    Self::SavedSearches,
    Self::Threads,
    Self::Unread,
    Self::ReadReceipts,
    Self::MessageAcks,
    Self::PendingUploads,
    Self::FileTransfers,
    Self::ShareLinks,
    Self::PendingPreviews,
    Self::Categories,
    Self::ChannelBrowser,
    Self::DeviceLists,
    Self::DeviceKeys,
    Self::RemovedSenders,
  ];

  /// The table with this [`Self::name`], if this version knows it.
  pub fn from_name(name: &str) -> Option<Self> {
    Self::ALL.into_iter().find(|table| table.name() == name)
  }

  /// Stable name, used as the table key in persistent stores.
  pub fn name(self) -> &'static str {
    match self {
      Self::Account => "account",
      Self::Spaces => "spaces",
      Self::Conversations => "conversations",
      Self::Members => "members",
      Self::Users => "users",
      Self::ContentKeys => "content_keys",
      Self::MlsStorage => "mls_storage",
      Self::AppliedChanges => "applied_changes",
      Self::Outbox => "outbox",
      Self::SyncState => "sync_state",
      Self::PinnedIdentities => "pinned_identities",
      Self::Undecrypted => "undecrypted",
      Self::Presence => "presence",
      Self::Workspace => "workspace",
      Self::PendingChanges => "pending_changes",
      Self::SearchSegments => "search_segments",
      Self::SavedSearches => "saved_searches",
      Self::Threads => "threads",
      Self::Unread => "unread",
      Self::ReadReceipts => "read_receipts",
      Self::MessageAcks => "message_acks",
      Self::PendingUploads => "pending_uploads",
      Self::FileTransfers => "file_transfers",
      Self::ShareLinks => "share_links",
      Self::PendingPreviews => "pending_previews",
      Self::Categories => "categories",
      Self::ChannelBrowser => "channel_browser",
      Self::DeviceLists => "device_lists",
      Self::DeviceKeys => "device_keys",
      Self::RemovedSenders => "removed_senders",
    }
  }
}

#[cfg(test)]
pub(crate) mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  #[test]
  fn every_table_round_trips_through_its_name() {
    for table in Table::ALL {
      assert_eq!(Table::from_name(table.name()), Some(table));
    }
    assert_eq!(Table::from_name("not_a_table"), None);
  }

  /// Behaviour every [`LocalStore`] must show; run against each implementation.
  pub(crate) fn exercise_store(store: &impl LocalStore) {
    key_value_round_trip(store);
    messages_are_ordered_and_filtered(store);
    replies_also_sent_to_the_conversation_are_listed_in_both(store);
  }

  fn replies_also_sent_to_the_conversation_are_listed_in_both(store: &impl LocalStore) {
    let conversation = id(20);
    let root = message(conversation, 21, Some(600), None, 1_000);
    let mut reply = message(conversation, 22, Some(700), Some(600), 2_000);
    let quiet_reply = message(conversation, 23, Some(800), Some(600), 3_000);
    if let MessageContent::Text { meta, .. } = &mut reply.content {
      meta.is_also_in_channel = true;
    }
    for stored in [&root, &reply, &quiet_reply] {
      store.upsert_message(stored).unwrap();
    }
    let top: Vec<ClientMessageId> = store
      .messages(conversation, MessageFilter::TopLevel)
      .unwrap()
      .into_iter()
      .map(|m| m.client_message_id)
      .collect();
    assert_eq!(top, vec![root.client_message_id, reply.client_message_id]);
    assert_eq!(
      store
        .messages(conversation, MessageFilter::Thread(id(600)))
        .unwrap()
        .len(),
      2
    );
  }

  fn key_value_round_trip(store: &impl LocalStore) {
    assert_eq!(store.get(Table::Users, "1").unwrap(), None);
    store.put(Table::Users, "2", b"two").unwrap();
    store.put(Table::Users, "1", b"one").unwrap();
    store.put(Table::Spaces, "1", b"space").unwrap();
    store.put(Table::Users, "1", b"uno").unwrap();
    assert_eq!(store.get(Table::Users, "1").unwrap(), Some(b"uno".to_vec()));
    assert_eq!(
      store.list(Table::Users).unwrap(),
      vec![
        ("1".to_string(), b"uno".to_vec()),
        ("2".to_string(), b"two".to_vec())
      ]
    );
    store.delete(Table::Users, "1").unwrap();
    assert_eq!(store.list(Table::Users).unwrap().len(), 1);
    assert_eq!(store.get(Table::Spaces, "1").unwrap(), Some(b"space".to_vec()));
  }

  fn messages_are_ordered_and_filtered(store: &impl LocalStore) {
    let conversation = id(10);
    let other = id(11);
    let pending = message(conversation, 1, None, None, 5_000);
    let second = message(conversation, 2, Some(200), None, 2_000);
    let first = message(conversation, 3, Some(100), None, 3_000);
    let reply = message(conversation, 4, Some(300), Some(100), 4_000);
    let mut reaction = message(conversation, 5, Some(400), None, 4_500);
    reaction.content = MessageContent::Reaction {
      target: first.client_message_id,
      emoji: "+1".into(),
      is_added: true,
    };
    for stored in [
      &pending,
      &second,
      &first,
      &reply,
      &reaction,
      &message(other, 6, Some(50), None, 1),
    ] {
      store.upsert_message(stored).unwrap();
    }

    let top: Vec<_> = store
      .messages(conversation, MessageFilter::TopLevel)
      .unwrap()
      .into_iter()
      .map(|m| m.client_message_id)
      .collect();
    assert_eq!(
      top,
      vec![
        first.client_message_id,
        second.client_message_id,
        pending.client_message_id
      ]
    );
    let thread = store
      .messages(conversation, MessageFilter::Thread(id(100)))
      .unwrap();
    assert_eq!(thread, vec![reply.clone()]);
    assert_eq!(store.messages(conversation, MessageFilter::All).unwrap().len(), 5);

    // Sending assigns the server ID; the record is replaced, not duplicated.
    let mut sent = pending.clone();
    sent.maybe_server_id = Some(id(500));
    store.upsert_message(&sent).unwrap();
    assert_eq!(
      store.message(conversation, pending.client_message_id).unwrap(),
      Some(sent)
    );
    assert_eq!(store.messages(conversation, MessageFilter::All).unwrap().len(), 5);

    store.delete_messages(conversation).unwrap();
    assert!(
      store
        .messages(conversation, MessageFilter::All)
        .unwrap()
        .is_empty()
    );
    assert_eq!(store.messages(other, MessageFilter::All).unwrap().len(), 1);
  }

  fn message(
    conversation: ConversationId,
    client: u8,
    maybe_server: Option<i64>,
    maybe_thread: Option<i64>,
    sent_at_ms: i64,
  ) -> StoredMessage {
    StoredMessage {
      conversation_id: conversation,
      client_message_id: ClientMessageId::from_bytes([client; 16]),
      maybe_server_id: maybe_server.map(id),
      sender_user_id: id(1),
      maybe_thread_id: maybe_thread.map(id),
      sent_at_ms,
      content: MessageContent::Text {
        body: format!("message {client}"),
        mentions: Vec::new(),
        meta: Default::default(),
      },
      is_edited: false,
      edits: Vec::new(),
      is_pinned: false,
    }
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
