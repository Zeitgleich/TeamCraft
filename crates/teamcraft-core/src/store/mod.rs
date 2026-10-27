//! The local store (e2ee.md §9): everything the client knows, already decrypted.
//!
//! [`LocalStore`] is deliberately small – a typed key-value space plus an ordered message table –
//! so a native SQLCipher implementation, an encrypted IndexedDB one in the browser and an in-memory
//! one for tests stay interchangeable. Typed records on top live in [`crate::records`].

#[cfg(target_arch = "wasm32")]
pub(crate) mod browser;
#[cfg(all(
  test,
  feature = "sqlcipher",
  feature = "sealed-sqlite",
  not(target_arch = "wasm32")
))]
mod fixtures;
mod key_source;
#[cfg(not(target_arch = "wasm32"))]
mod lock;
mod memory;
pub mod sealed;
#[cfg(all(feature = "sealed-sqlite", not(target_arch = "wasm32")))]
mod sealed_sqlite;
#[cfg(all(
  any(feature = "sqlcipher", feature = "sealed-sqlite"),
  not(target_arch = "wasm32")
))]
mod sql_group;
#[cfg(all(feature = "sqlcipher", not(target_arch = "wasm32")))]
mod sqlite;
#[cfg(all(
  any(feature = "sqlcipher", feature = "sealed-sqlite"),
  not(target_arch = "wasm32")
))]
mod sqlite_wipe;
mod tracked;
#[cfg(any(test, target_arch = "wasm32"))]
mod write_queue;

#[cfg(target_arch = "wasm32")]
pub use browser::{
  IndexedDbRecords, IndexedDbStore, browser_database_key, vault_delete, vault_get, vault_put,
};
#[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
pub use key_source::KeyringKeySource;
pub use key_source::{DatabaseKeySource, KeySourceError, StaticKeySource};
#[cfg(not(target_arch = "wasm32"))]
pub use key_source::{KeystoreThreadError, run_outside_runtime};
#[cfg(not(target_arch = "wasm32"))]
pub use lock::StoreLock;
pub use memory::MemoryStore;
#[cfg(all(feature = "sealed-sqlite", not(target_arch = "wasm32")))]
pub use sealed_sqlite::{SealedSqliteStore, SqliteRecords};
#[cfg(all(feature = "sqlcipher", not(target_arch = "wasm32")))]
pub use sqlite::SqlCipherStore;
pub use tracked::{RemovedRendition, StoreChanges, TrackedStore};

use serde::{Deserialize, Serialize};
use teamcraft_proto::{ClientMessageId, ConversationId, MessageId, MlsMessageId, ThreadId, UserId};

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
  /// The openmls provider storage (MLS group state, key package private keys), one record per
  /// storage entry (`mls_storage`).
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
  /// The members of a space with their roles, per space (space settings).
  SpaceRosters,
  /// Pending invitations of a space, per space (space settings, member managers only).
  SpaceInvites,
  /// Users' signed device lists, newest seen (security review A4).
  DeviceLists,
  /// Verified signing keys of devices, `{user}/{device}`, to check message signatures (A10).
  DeviceKeys,
  /// When a device left a conversation, `{conversation}/{user}/{device}` (A10).
  RemovedSenders,
  /// Small actions waiting for the server (read positions, acknowledgements, pins, presence), one
  /// record per target.
  PendingActions,
  /// Presence as `presence_update` events (and the reads after a connection) announced it to the
  /// UI, one record per user with its revision (issue #259).
  AnnouncedPresence,
}

/// A decrypted (or own, not yet sent) message, or a line this device derived from a verified MLS
/// commit ([`Self::maybe_verified_commit`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredMessage {
  pub conversation_id: ConversationId,
  pub client_message_id: ClientMessageId,
  /// Absent while the message waits in the outbox, and on a line derived from a commit.
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
  /// Server ID of the edit whose text is shown: an edit with a lower ID loses, so devices agree
  /// whatever order edits arrive in (issue #160). Absent before the first numbered edit.
  #[serde(rename = "edit_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_edit_id: Option<MessageId>,
  #[serde(default)]
  pub is_pinned: bool,
  /// The MLS commit, verified or made by this device, that this line was derived from (the
  /// "joined as a workspace admin" line, admin.md §2.4). Only this device sets it, never a message
  /// from the server or another member, so nobody can make such a line by sending one. The line
  /// sits at the commit's place among the messages, and messages can't replace or change it.
  #[serde(rename = "verified_commit", default, skip_serializing_if = "Option::is_none")]
  pub maybe_verified_commit: Option<MlsMessageId>,
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
  /// A change to the channel's integration-bridge sources (docs/bridge.md, "Sources"); applied
  /// to the conversation's [`SourceLedger`](crate::integration_sources::SourceLedger).
  IntegrationSource { change: teamcraft_proto::SourceChange },
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
  /// The channel moved to another space (named here as the mover saw it).
  Moved {
    #[serde(default)]
    space: String,
  },
  /// The sender added `added` to the conversation's call, which moved into the meeting chat
  /// `meeting` (docs/meetings.md, "A call becomes a meeting"). Not listed; devices in the call
  /// follow it only on this announcement from a group member, never on the server's word.
  CallMoved {
    meeting: ConversationId,
    #[serde(default)]
    added: Vec<UserId>,
    /// The DM call that moves: an announcement of another call moves nothing.
    #[serde(rename = "call", default, skip_serializing_if = "Option::is_none")]
    maybe_call: Option<teamcraft_proto::CallId>,
  },
  /// A workspace admin joined this private channel (admin.md §2.4), so the join is never silent;
  /// shown whatever the join/leave setting says, and only as a line each member's device derives
  /// from the verified commit that added the admin ([`StoredMessage::maybe_verified_commit`],
  /// issue #811). As a message it is still sent, for older versions, by the member whose client
  /// committed the admin's signed self-join (`visible`), or with `is_approved` by the channel
  /// owner who let the admin in (`owner_approval`); this version keeps such a message hidden.
  AdminJoined {
    user: UserId,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    is_approved: bool,
  },
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
  /// The sender's verified head of the channel's signed roles (e2ee.md §4.6.1), added when the
  /// message is sent, so members notice a server showing them different role logs.
  #[serde(
    rename = "channel_roles_head",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_channel_roles_head: Option<Box<crate::channel_roles::ChannelRolesHead>>,
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
  /// Width and height of a picture, so receivers lay it out before anything loads. Older clients
  /// ignore it; files from them have none.
  #[serde(rename = "dimensions", default, skip_serializing_if = "Option::is_none")]
  pub maybe_dimensions: Option<ImageDimensions>,
  /// How long a video plays, read by the sender's app; older clients ignore it.
  #[serde(rename = "duration_ms", default, skip_serializing_if = "Option::is_none")]
  pub maybe_duration_ms: Option<u64>,
}

/// The size of a picture in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageDimensions {
  pub width: u32,
  pub height: u32,
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

/// Which messages with a picture or video [`LocalStore::media_messages`] lists, from a message
/// outwards (the image viewer pages through them).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaRange {
  /// Older than the message (the newest ones without one), nearest first.
  Older(Option<MessageId>),
  /// Newer than the message, nearest first.
  Newer(MessageId),
}

/// Media types the image viewer shows: pictures and videos.
const MEDIA_TYPE_PREFIXES: [&str; 2] = ["image/", "video/"];

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
  /// The browser refused a write for lack of storage space.
  #[error("there is not enough storage space to save the local data")]
  QuotaExceeded,
  /// A newer version of the app took over the database; this one has to reload.
  #[error("the local database was closed by a newer version of the app")]
  Closed,
  /// Another process (a second app instance) holds the store.
  #[error("the local data is in use by another instance of the app")]
  InUse,
  /// A newer version of the app changed the database layout; this one can't read it (issue #160).
  #[error(
    "the local database is from a newer version of the app (layout {found}, this version reads up to {supported})"
  )]
  NewerSchema { found: i64, supported: i64 },
  /// A write of a group failed, so none of the group's writes was kept.
  #[error("the local database did not save a group of changes: {0}")]
  GroupFailed(String),
}

impl StoreError {
  /// The database can't be read with this version, now or after a restart: a damaged file, a key
  /// that doesn't open it, or a newer layout. The user decides what happens to it (issue #160).
  pub fn is_unreadable(&self) -> bool {
    matches!(self, Self::WrongKey | Self::Corrupt(_) | Self::NewerSchema { .. })
  }
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
  /// The message the server knows by `server_id`, without reading the whole conversation (the
  /// preview of a message link). The default scans; stores with an index answer directly.
  fn message_by_server_id(
    &self,
    conversation: ConversationId,
    server_id: MessageId,
  ) -> Result<Option<StoredMessage>, StoreError> {
    Ok(
      self
        .messages(conversation, MessageFilter::All)?
        .into_iter()
        .find(|message| message.maybe_server_id == Some(server_id)),
    )
  }
  /// Up to `limit` listed server messages with a picture or video attachment, in the `range`,
  /// nearest to its start first. The image viewer pages through a conversation's media with it,
  /// so a store with an index reads only what it returns. The default scans.
  fn media_messages(
    &self,
    conversation: ConversationId,
    range: MediaRange,
    limit: usize,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    Ok(pick_media(
      self.messages(conversation, MessageFilter::All)?,
      range,
      limit,
    ))
  }
  fn delete_messages(&self, conversation: ConversationId) -> Result<(), StoreError>;
  /// Removes one message, e.g. an own one that was never sent and the user dropped.
  fn delete_message(
    &self,
    conversation: ConversationId,
    client_message_id: ClientMessageId,
  ) -> Result<(), StoreError>;

  /// Overwrites what deleted or replaced records left behind in the store's files (free pages, a
  /// write-ahead log), so a secret that was deleted can't be read back from them, also not by
  /// somebody who later gets the database key (issue #391). The store may still hold it until this
  /// returns; data the file system or flash controller copied elsewhere is out of reach. Stores
  /// without such files do nothing.
  fn purge_freed(&self) -> Result<(), StoreError> {
    Ok(())
  }

  /// [`Self::purge_freed`], but for writes that come in bursts (the MLS storage is saved for
  /// every processed group message): the store may wait up to a configured interval and merge
  /// the purges, and still purges on [`Self::purge_freed`], when it is dropped and when the app
  /// stops it. Never fails the caller's write: a purge that did not run stays due and is
  /// retried.
  fn purge_freed_soon(&self) {}

  /// Starts a group of writes that reaches the disk together: after a crash, or a write of the
  /// group that failed, either all of them are kept or none (issue #160). Groups nest; the
  /// outermost [`Self::end_writes`] commits. Reads see the group's writes at once, also from other
  /// threads, and their writes join the group. Use [`write_group`] (or `Client::atomically`)
  /// instead of calling this directly. Stores without a disk keep the default.
  fn begin_writes(&self) -> Result<(), StoreError> {
    Ok(())
  }

  /// Ends the group started by the matching [`Self::begin_writes`].
  fn end_writes(&self) -> Result<(), StoreError> {
    Ok(())
  }

  /// Starts writing what is queued now instead of after a batching delay, without waiting for it:
  /// for a write another device relies on next (the secrets of a link, issue #168). Stores that
  /// write at once keep the default.
  fn write_soon(&self) {}

  /// What became of the groups of writes since the store was opened, for a caller that remembers
  /// what it wrote instead of reading it back (the MLS state, `mls_storage`): a committed group
  /// means every group open before it is on disk, a lost one that some writes it accepted are not.
  /// `None` for a store that keeps every write it accepted (in memory; the browser's, which writes
  /// a failed one again until it goes through).
  fn write_tally(&self) -> Option<WriteTally> {
    None
  }
}

/// Counts of [`LocalStore::write_tally`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WriteTally {
  /// Outermost groups of writes committed.
  pub committed: u64,
  /// Groups of writes rolled back (one of their writes failed, or the commit did).
  pub lost: u64,
}

/// Runs `writes` as one group of writes ([`LocalStore::begin_writes`]). An error of `writes` doesn't
/// undo what it wrote before; only a crash or a failed write of the group does.
pub fn write_group<S, T, E>(store: &S, writes: impl FnOnce() -> Result<T, E>) -> Result<T, E>
where
  S: LocalStore + ?Sized,
  E: From<StoreError>,
{
  store.begin_writes()?;
  // A panic must not leave the group open: nothing would ever be committed again.
  let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(writes));
  let ended = store.end_writes();
  match result {
    Ok(result) => {
      let value = result?;
      ended?;
      Ok(value)
    },
    Err(panic) => std::panic::resume_unwind(panic),
  }
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

      fn message_by_server_id(
        &self,
        conversation: ConversationId,
        server_id: MessageId,
      ) -> Result<Option<StoredMessage>, StoreError> {
        (**self).message_by_server_id(conversation, server_id)
      }

      fn messages(
        &self,
        conversation: ConversationId,
        filter: MessageFilter,
      ) -> Result<Vec<StoredMessage>, StoreError> {
        (**self).messages(conversation, filter)
      }

      fn media_messages(
        &self,
        conversation: ConversationId,
        range: MediaRange,
        limit: usize,
      ) -> Result<Vec<StoredMessage>, StoreError> {
        (**self).media_messages(conversation, range, limit)
      }

      fn delete_messages(&self, conversation: ConversationId) -> Result<(), StoreError> {
        (**self).delete_messages(conversation)
      }

      fn delete_message(
        &self,
        conversation: ConversationId,
        client_message_id: ClientMessageId,
      ) -> Result<(), StoreError> {
        (**self).delete_message(conversation, client_message_id)
      }

      fn purge_freed(&self) -> Result<(), StoreError> {
        (**self).purge_freed()
      }

      fn purge_freed_soon(&self) {
        (**self).purge_freed_soon()
      }

      fn begin_writes(&self) -> Result<(), StoreError> {
        (**self).begin_writes()
      }

      fn end_writes(&self) -> Result<(), StoreError> {
        (**self).end_writes()
      }

      fn write_soon(&self) {
        (**self).write_soon()
      }

      fn write_tally(&self) -> Option<WriteTally> {
        (**self).write_tally()
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
  /// Display order: server messages by ID, a line derived from a commit by the commit's ID (both
  /// the server's time-ordered IDs); pending ones after all of them, by time written.
  pub fn sort_key(&self) -> (u8, i64) {
    match (self.maybe_server_id, self.maybe_verified_commit) {
      (Some(id), _) => (0, teamcraft_proto::Id::snowflake(id).get()),
      (None, Some(commit)) => (0, teamcraft_proto::Id::snowflake(commit).get()),
      (None, None) => (1, self.sent_at_ms),
    }
  }

  pub fn is_reaction(&self) -> bool {
    matches!(self.content, MessageContent::Reaction { .. })
  }

  /// Reactions, edits, deletes and pins act on other messages and aren't listed themselves. Nor
  /// is a message saying that an admin joined: only the line this device derived from the commit
  /// shows that (issue #811), so a member can't make it up by sending one.
  pub fn is_hidden(&self) -> bool {
    match &self.content {
      MessageContent::Event {
        event: ConversationEvent::AdminJoined { .. },
      } => self.maybe_verified_commit.is_none(),
      content => matches!(
        content,
        MessageContent::Reaction { .. }
          | MessageContent::Edit { .. }
          | MessageContent::Delete { .. }
          | MessageContent::Pin { .. }
          | MessageContent::Bookmarks { .. }
          | MessageContent::IndexSegment { .. }
          | MessageContent::IntegrationSource { .. }
          | MessageContent::Event {
            event: ConversationEvent::CallMoved { .. }
          }
      ),
    }
  }

  /// A thread reply the sender also posted to the conversation.
  pub fn is_also_in_channel(&self) -> bool {
    matches!(&self.content, MessageContent::Text { meta, .. } if meta.is_also_in_channel)
  }

  /// A listed server message with a picture or video attachment: what the image viewer pages
  /// through.
  pub fn has_media(&self) -> bool {
    let attachments: &[AttachmentRef] = match &self.content {
      MessageContent::File { attachment } => std::slice::from_ref(attachment),
      MessageContent::Text { meta, .. } => &meta.attachments,
      _ => return false,
    };
    self.maybe_server_id.is_some()
      && !self.is_hidden()
      && attachments.iter().any(|attachment| {
        MEDIA_TYPE_PREFIXES
          .iter()
          .any(|prefix| attachment.media_type.starts_with(prefix))
      })
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

/// The `limit` messages with media in `range` among `messages` (in display order), nearest to the
/// range's start first.
pub fn pick_media(messages: Vec<StoredMessage>, range: MediaRange, limit: usize) -> Vec<StoredMessage> {
  let server_id = |message: &StoredMessage| {
    message
      .maybe_server_id
      .map(|id| teamcraft_proto::Id::snowflake(id).get())
  };
  let mut media: Vec<StoredMessage> = messages
    .into_iter()
    .filter(StoredMessage::has_media)
    .filter(|message| match range {
      MediaRange::Older(Some(from)) => server_id(message) < Some(teamcraft_proto::Id::snowflake(from).get()),
      MediaRange::Older(None) => true,
      MediaRange::Newer(from) => server_id(message) > Some(teamcraft_proto::Id::snowflake(from).get()),
    })
    .collect();
  if matches!(range, MediaRange::Older(_)) {
    media.reverse();
  }
  media.truncate(limit);
  media
}

impl Table {
  /// Every table; a new variant goes here too (stores that load by name skip unknown tables).
  pub const ALL: [Self; 34] = [
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
    Self::SpaceRosters,
    Self::SpaceInvites,
    Self::DeviceLists,
    Self::DeviceKeys,
    Self::RemovedSenders,
    Self::PendingActions,
    Self::AnnouncedPresence,
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
      Self::SpaceRosters => "space_rosters",
      Self::SpaceInvites => "space_invites",
      Self::DeviceLists => "device_lists",
      Self::DeviceKeys => "device_keys",
      Self::RemovedSenders => "removed_senders",
      Self::PendingActions => "pending_actions",
      Self::AnnouncedPresence => "announced_presence",
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
    groups_of_writes_nest_and_are_read_at_once(store);
    media_messages_are_paged_from_a_message_outwards(store);
    admin_joins_show_only_as_lines_from_commits(store);
  }

  /// Issue #811: the line derived from a commit sits at the commit's place and is listed; a message
  /// saying the same is kept but never listed.
  fn admin_joins_show_only_as_lines_from_commits(store: &impl LocalStore) {
    let conversation = id(30);
    let joined = MessageContent::Event {
      event: ConversationEvent::AdminJoined {
        user: id(7),
        is_approved: true,
      },
    };
    let before = message(conversation, 1, Some(100), None, 1_000);
    let after = message(conversation, 2, Some(300), None, 3_000);
    let mut line = message(conversation, 3, None, None, 2_000);
    line.content = joined.clone();
    line.maybe_verified_commit = Some(id(200));
    let mut announced = message(conversation, 4, Some(250), None, 2_500);
    announced.content = joined;
    for stored in [&after, &announced, &line, &before] {
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
        before.client_message_id,
        line.client_message_id,
        after.client_message_id
      ]
    );
    assert_eq!(store.messages(conversation, MessageFilter::All).unwrap().len(), 4);
    // The line is no message of the server's, also not under the commit's ID.
    assert_eq!(store.message_by_server_id(conversation, id(200)).unwrap(), None);
    store.delete_messages(conversation).unwrap();
  }

  fn media_messages_are_paged_from_a_message_outwards(store: &impl LocalStore) {
    let conversation = id(20);
    // Server IDs 10..=100: every other one has media; a text mentioning a media type has none.
    for (client, server) in (1..=10u8).zip((10..=100).step_by(10)) {
      let mut stored = message(conversation, client, Some(server), None, i64::from(client));
      match client % 2 {
        0 => stored.content = media_content("image/png"),
        _ if client == 3 => {
          stored.content = MessageContent::Text {
            body: r#""media_type":"image/png""#.into(),
            mentions: Vec::new(),
            meta: Default::default(),
          }
        },
        _ => {},
      }
      store.upsert_message(&stored).unwrap();
    }
    let mut video = message(conversation, 11, Some(110), None, 11);
    video.content = media_content("video/mp4");
    let mut document = message(conversation, 12, Some(120), None, 12);
    document.content = media_content("application/pdf");
    let mut pending = message(conversation, 13, None, None, 13);
    pending.content = media_content("image/png");
    for stored in [&video, &document, &pending] {
      store.upsert_message(stored).unwrap();
    }
    let servers = |range, limit| -> Vec<i64> {
      store
        .media_messages(conversation, range, limit)
        .unwrap()
        .iter()
        .map(|stored| Id::snowflake(stored.maybe_server_id.unwrap()).get())
        .collect()
    };
    assert_eq!(
      servers(MediaRange::Older(None), 3),
      [110, 100, 80],
      "newest first, no documents or texts"
    );
    assert_eq!(servers(MediaRange::Older(Some(id(80))), 10), [60, 40, 20]);
    assert_eq!(servers(MediaRange::Newer(id(60)), 2), [80, 100], "nearest first");
    assert_eq!(servers(MediaRange::Newer(id(100)), 10), [110]);
    assert!(servers(MediaRange::Newer(id(110)), 10).is_empty());
    assert!(servers(MediaRange::Older(None), 0).is_empty());
    assert!(
      store
        .media_messages(id(21), MediaRange::Older(None), 10)
        .unwrap()
        .is_empty(),
      "other conversations have their own"
    );
    store.delete_messages(conversation).unwrap();
  }

  fn media_content(media_type: &str) -> MessageContent {
    // From JSON: the fields every version of the record has.
    let attachment = serde_json::json!({
      "attachment_id": id::<teamcraft_proto::AttachmentId>(1),
      "key": "",
      "ciphertext_sha256": "",
      "file_name": "f",
      "media_type": media_type,
      "size_bytes": 1,
    });
    MessageContent::File {
      attachment: serde_json::from_value(attachment).unwrap(),
    }
  }

  fn groups_of_writes_nest_and_are_read_at_once(store: &impl LocalStore) {
    let written = write_group(store, || -> Result<_, StoreError> {
      store.put(Table::Spaces, "outer", b"1")?;
      write_group(store, || store.put(Table::Spaces, "inner", b"2"))?;
      // Inside the group, its writes are already visible.
      store.get(Table::Spaces, "inner")
    })
    .unwrap();
    assert_eq!(written, Some(b"2".to_vec()));
    assert_eq!(store.get(Table::Spaces, "outer").unwrap(), Some(b"1".to_vec()));
    // An error of the group's code keeps what it wrote before (only a failed write undoes it).
    let failed: Result<(), StoreError> = write_group(store, || {
      store.put(Table::Spaces, "before_error", b"3")?;
      Err(StoreError::Database("caller gave up".into()))
    });
    assert!(failed.is_err());
    assert_eq!(
      store.get(Table::Spaces, "before_error").unwrap(),
      Some(b"3".to_vec())
    );
    // The store takes new groups after that.
    write_group(store, || store.put(Table::Spaces, "after", b"4")).unwrap();
    assert_eq!(store.get(Table::Spaces, "after").unwrap(), Some(b"4".to_vec()));
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
      Some(sent.clone())
    );
    // Found by the server's ID too, only in its own conversation.
    assert_eq!(
      store.message_by_server_id(conversation, id(500)).unwrap(),
      Some(sent)
    );
    assert_eq!(
      store.message_by_server_id(conversation, id(100)).unwrap(),
      Some(first.clone())
    );
    assert_eq!(store.message_by_server_id(other, id(100)).unwrap(), None);
    assert_eq!(store.message_by_server_id(conversation, id(999)).unwrap(), None);
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
      maybe_edit_id: None,
      is_pinned: false,
      maybe_verified_commit: None,
    }
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
