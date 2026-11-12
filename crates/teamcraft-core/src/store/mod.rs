//! The local store (e2ee.md §9): everything the client knows, already decrypted.
//!
//! [`LocalStore`] is deliberately small – a typed key-value space plus an ordered message table –
//! so a native SQLCipher implementation, an encrypted IndexedDB one in the browser and an in-memory
//! one for tests stay interchangeable. Typed records on top live in [`crate::records`].

#[cfg(target_arch = "wasm32")]
pub(crate) mod browser;
mod commit_journal;
#[cfg(all(
  test,
  feature = "sqlcipher",
  feature = "sealed-sqlite",
  not(target_arch = "wasm32")
))]
mod fixtures;
mod footprint;
mod key_source;
#[cfg(all(feature = "sealed-sqlite", not(target_arch = "wasm32")))]
mod lane;
#[cfg(not(target_arch = "wasm32"))]
mod lock;
#[cfg(all(feature = "sealed-sqlite", not(target_arch = "wasm32")))]
mod log_purger;
mod lru;
mod memory;
mod message_cache;
mod overlay;
pub mod sealed;
mod sealed_catalog;
mod sealed_index;
#[cfg(all(feature = "sealed-sqlite", not(target_arch = "wasm32")))]
mod sealed_sqlite;
#[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
mod secret_vault;
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
mod sqlite_log;
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
  IdbFailureHook, IndexedDbRecords, IndexedDbStore, browser_database_key,
  browser_database_key_without_upgrade, browser_dpop_prover, browser_dpop_prover_or_new, own_sound_delete,
  own_sound_get, own_sound_put, set_idb_failure_hook, vault_delete, vault_get, vault_put,
};
pub use commit_journal::{CommitJournal, CommitStamp};
pub use key_source::{DatabaseKeySource, KeySourceError, StaticKeySource};
#[cfg(not(target_arch = "wasm32"))]
pub use key_source::{KeystoreThreadError, run_outside_runtime};
#[cfg(all(feature = "sealed-sqlite", not(target_arch = "wasm32")))]
pub use lane::LaneStore;
#[cfg(not(target_arch = "wasm32"))]
pub use lock::StoreLock;
#[cfg(all(feature = "sealed-sqlite", not(target_arch = "wasm32")))]
pub use log_purger::{DEFAULT_MAX_PURGE_DELAY, LogPurgeLimits};
pub use memory::MemoryStore;
pub use overlay::OverlayStore;
pub use sealed::SealedStoreLimits;
#[cfg(all(feature = "sealed-sqlite", not(target_arch = "wasm32")))]
pub use sealed_sqlite::{SealedSqliteStore, SqliteRecords};
#[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
pub use secret_vault::{Keystore, OsKeystore, SecretVault, SessionNote, TokenKind};
#[cfg(all(feature = "sqlcipher", not(target_arch = "wasm32")))]
pub use sqlite::SqlCipherStore;
pub use tracked::{DEFAULT_MESSAGE_CACHE_BYTES, RemovedRendition, StoreChanges, TrackedStore};

use serde::{Deserialize, Serialize};
use teamcraft_proto::{ClientMessageId, ConversationId, MessageId, MlsMessageId, ThreadId, UserId};

use crate::push_preview::ShownMessage;

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
  /// The announced search segment per month, keyed `conversation/month` like
  /// [`Self::SearchSegments`]: written when an announcement is stored, so a prefetch reads these
  /// instead of every message (issue #1636).
  SegmentAnnouncements,
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
  /// Downloads to disk, keyed by attachment ID: how far each got and the recent ones the app lists
  /// (`client::disk_downloads`).
  Downloads,
  /// What each conversation's sidebar entry counted, keyed by conversation
  /// (`crate::entry_tally`). Derived: checked against [`LocalStore::feed_version`], so a lost or
  /// stale record only means counting again.
  EntryTallies,
  /// The local search index's files in chunks and its manifest (`crate::search_index`, issue #154):
  /// derived from the messages, and built again from them when it is lost.
  SearchIndex,
  /// Messages (`conversation/message`) and conversations (`conversation`) the local search index
  /// has to look at again, written with the message (`crate::search_index`). Native only.
  SearchQueue,
}

/// A decrypted (or own, not yet sent) message, or a line this device derived itself: from a
/// verified MLS commit ([`Self::maybe_verified_commit`]) or from the server's record of the
/// conversation ([`Self::maybe_server_change`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredMessage {
  pub conversation_id: ConversationId,
  pub client_message_id: ClientMessageId,
  /// Absent while the message waits in the outbox, and on a line this device derived.
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
  /// What makes this a line about a change the server reports of the conversation (archived,
  /// restored, made private or public, moved; issue #818), derived by this device from the
  /// server's record as it saw it change. Like [`Self::maybe_verified_commit`] only this device
  /// sets it, so no member's message makes or hides such a line; a member's announcement of the
  /// change can only name who made it ([`ServerChange::actor`]).
  #[serde(rename = "server_change", default, skip_serializing_if = "Option::is_none")]
  pub maybe_server_change: Option<ServerChange>,
  /// Set while this device can't read the message: the row is a placeholder at the message's place
  /// (issue #888) with only what the server's envelope says (sender, time, thread), and
  /// [`Self::content`] is [`MessageContent::Unknown`]. Only this device sets it, never a message:
  /// decrypting the message replaces the row.
  #[serde(rename = "unreadable", default, skip_serializing_if = "Option::is_none")]
  pub maybe_unreadable: Option<Unreadable>,
}

/// Why a stored message is a placeholder (issue #888, e2ee.md §5, "History on a new device").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Unreadable {
  /// Its content key (or its sender device's signing key) hasn't arrived yet and may still come.
  /// `since_ms`: when this device first parked it, on the local clock.
  Waiting { since_ms: i64 },
  /// No key came and none is expected: this device is in the group and waits for nothing, or the
  /// wait ran out. Still decrypted if a key turns up after all.
  NotAvailable,
  /// The key is there, but the message doesn't open with it or isn't validly signed: never retried.
  Failed,
  /// A state of a newer version.
  #[serde(other)]
  Unknown,
}

/// Where a line about a change the server reports stands, and whom it names (issue #818).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerChange {
  /// The line's place among the messages, on the server's clock: right after everything this
  /// device knew of the conversation when it saw the change, or, once a member's announcement
  /// names who made it, the announcement's.
  pub at: MessageId,
  /// The newest message this device held of the conversation when it saw the change: only an
  /// announcement sent after it can name who made the change.
  pub seen_after: MessageId,
  /// The server's newest message of the conversation as far as this device knew when it saw the
  /// change (the sync's head, else [`Self::seen_after`]): an announcement sent longer than the
  /// configured window after it names nobody.
  pub seen_until: MessageId,
  /// Whom the line names.
  #[serde(default)]
  pub actor: ChangeActor,
}

/// Who made a change the server reports, as far as this device can tell. The line's sender
/// ([`StoredMessage::sender_user_id`]) is that person when the actor [`Self::is_named`], and
/// [`nobody`] otherwise.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChangeActor {
  /// This device made the change itself, so the user did; announcements don't change that.
  ThisDevice,
  /// The sender announced the change in this message, which matched it.
  Announced { announcement: ClientMessageId },
  /// Different members announced the change: nobody is named, and no later announcement is.
  Contested,
  /// Nobody is named: no member's announcement matched the change (yet). Also what a record of a
  /// newer version reads as.
  #[default]
  #[serde(other)]
  Nobody,
}

impl ChangeActor {
  /// Whether the line names its sender as who made the change.
  pub fn is_named(&self) -> bool {
    matches!(self, Self::ThisDevice | Self::Announced { .. })
  }
}

/// The sender of a line that names nobody: no user has this ID (the smallest, older than anything
/// the server generates).
pub fn nobody() -> UserId {
  teamcraft_proto::Id::from_snowflake(teamcraft_proto::Snowflake::ZERO)
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
  Edit {
    target: ClientMessageId,
    body: String,
    /// The custom emoji the new text names (issue #207), added to the message's own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    custom_emoji: Vec<teamcraft_proto::CustomEmojiRef>,
  },
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
  /// An edit of the channel's shared page (e2ee.md §4.11): a Yjs update the core keeps
  /// ([`crate::canvas`]) but never reads; `is_snapshot` when it holds the whole page.
  Canvas {
    update: teamcraft_proto::Base64Bytes,
    #[serde(default, skip_serializing_if = "is_false")]
    is_snapshot: bool,
  },
  /// A change to the conversation, announced by whoever made it (shown as a system message).
  Event { event: ConversationEvent },
  /// Adds or removes the sender's reaction to another message (e2ee.md §4.4: reactions are
  /// encrypted messages, so the server can't count them).
  Reaction {
    target: ClientMessageId,
    emoji: String,
    is_added: bool,
    /// A custom emoji (issue #207): `emoji` is its `:shortcode:`, which older clients show, and
    /// this names its picture.
    #[serde(rename = "custom", default, skip_serializing_if = "Option::is_none")]
    maybe_custom: Option<teamcraft_proto::CustomEmojiRef>,
  },
  /// The sender's vote in a poll (issue #205, e2ee.md §4.12): the options it chooses, by position,
  /// replace the sender's earlier vote; none withdraws it. The newest valid vote of each sender in
  /// the server's order counts ([`crate::polls`]).
  PollVote {
    target: ClientMessageId,
    #[serde(default)]
    choices: Vec<u32>,
  },
  /// Ends a poll: votes the server took after this aren't counted. Only the poll's sender closes it.
  PollClose { target: ClientMessageId },
  /// A change to the channel's integration-bridge sources (docs/bridge.md, "Sources"); applied
  /// to the conversation's [`SourceLedger`](crate::integration_sources::SourceLedger).
  IntegrationSource { change: teamcraft_proto::SourceChange },
  /// A request to the workspace's notetaker (e2ee.md §8.2): join the conversation's call, or leave it;
  /// applied to the conversation's [`NotetakerLedger`](crate::notetaker::NotetakerLedger).
  Notetaker {
    request: teamcraft_proto::NotetakerRequest,
  },
  /// A signed entry of the channel's role log, sent by its signer right after the append (e2ee.md
  /// §4.6.3, part 1): it applies even if the log leaves it out, and a log that does is proof the
  /// server holds it back. Checked like an entry of the log; the sender vouches for nothing.
  ChannelRolesEntry {
    version: u64,
    payload: teamcraft_proto::Base64Bytes,
    signature: teamcraft_proto::Base64Bytes,
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
  // The next five are the server's record of the channel (issue #818): shown only as a line each
  // member's device derives from that record as it sees it change
  // ([`StoredMessage::maybe_server_change`]). As messages they are still sent by whoever made the
  // change, for older versions; this version keeps them hidden, and a matching one names who made
  // the change.
  Archived,
  Unarchived,
  MadePrivate,
  MadePublic,
  /// The channel moved to another space (named here as the mover saw it; a line names it as this
  /// device knows the space).
  Moved {
    #[serde(default)]
    space: String,
  },
  /// The sender added `added` to the conversation's call, which moved into the meeting chat
  /// `meeting` (docs/meetings.md, "A call becomes a meeting"). Devices in the call follow it only
  /// on this announcement from a group member, never on the server's word. Listed as a line with
  /// a link to the room (issue #1290); an older version stored it hidden, and such a row stays so.
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
  /// Imported from another chat system: the original author and time (e2ee.md §4.10). Believed
  /// only from the import account of the verified policy ([`crate::imported::trusted_origin`]).
  #[serde(rename = "imported", default, skip_serializing_if = "Option::is_none")]
  pub maybe_imported: Option<Box<crate::imported::ImportedOrigin>>,
  /// The integration source that posted this, as the bridge states it (docs/bridge.md, "Source
  /// identity"). Believed only from a bot account ([`crate::integration_sources::shown_source`]).
  #[serde(rename = "source", default, skip_serializing_if = "Option::is_none")]
  pub maybe_source: Option<Box<teamcraft_proto::SourceIdentity>>,
  /// The user groups the sender's device expanded into `mentions`, by handle (`accounting`), so
  /// receivers show `@accounting` as a group mention. Inside the ciphertext only: the server only
  /// sees the expanded IDs (e2ee.md §4.4). An older client ignores it and shows the text.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub group_mentions: Vec<String>,
  /// The workspace's custom emoji the text names by `:shortcode:` (issue #207), with their
  /// pictures' hashes, inside the ciphertext only. Receivers show those words as the pictures
  /// they hold; an older client ignores the field and shows the text. A boxed slice: the text
  /// variant of [`MessageContent`] stays small enough.
  #[serde(default, skip_serializing_if = "<[_]>::is_empty")]
  pub custom_emoji: Box<[teamcraft_proto::CustomEmojiRef]>,
  /// A poll (issue #205, e2ee.md §4.12): the message's text is what an older version shows (the
  /// question and options as text), this is what a current one shows. Votes and the close are
  /// [`MessageContent::PollVote`] and [`MessageContent::PollClose`] messages naming the message.
  #[serde(rename = "poll", default, skip_serializing_if = "Option::is_none")]
  pub maybe_poll: Option<Box<teamcraft_proto::PollSpec>>,
}

/// The quoted message: the reference, plus enough to show it where it isn't loaded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyRef {
  pub target: ClientMessageId,
  pub author: UserId,
  /// The message's text, or the file's name where it has none: all that an older client shows of a
  /// reply to one picture (it quotes the whole message and ignores `attachment`).
  pub excerpt: String,
  /// The one picture of an album this reply answers (issue #241).
  #[serde(rename = "attachment", default, skip_serializing_if = "Option::is_none")]
  pub maybe_attachment: Option<Box<ReplyAttachment>>,
}

/// One picture of the quoted message. A copy of its thumbnail travels with the reply, so the quote
/// stays when the picture is deleted or expires; its key doesn't: a quote gives nobody the file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyAttachment {
  pub attachment_id: teamcraft_proto::AttachmentId,
  pub file_name: String,
  #[serde(rename = "thumbnail", default, skip_serializing_if = "Option::is_none")]
  pub maybe_thumbnail: Option<teamcraft_proto::Base64Bytes>,
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
  /// A voice message's waveform (#204): one loudness byte per bar, made by the sender while
  /// recording. Inside the encrypted message like everything here; older clients ignore it and
  /// show the plain audio file.
  #[serde(rename = "waveform", default, skip_serializing_if = "Option::is_none")]
  pub maybe_waveform: Option<teamcraft_proto::Base64Bytes>,
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

/// A place in a conversation's display order ([`LocalStore::messages_at`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageSlot {
  /// Server messages and derived lines at this ID.
  At(MessageId),
  /// The messages not sent yet, which sort after all of them.
  Pending,
}

impl MessageSlot {
  pub(crate) fn holds(self, message: &StoredMessage) -> bool {
    match (self, message.sort_key()) {
      (Self::At(position), (0, value)) => value == teamcraft_proto::Id::snowflake(position).get(),
      (Self::Pending, (1, _)) => true,
      _ => false,
    }
  }
}

/// Which end of a conversation's server messages [`LocalStore::edge_server_id`] answers for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageEdge {
  Oldest,
  Newest,
}

/// A message's place in its conversation's display order ([`StoredMessage::sort_key`]), ties
/// broken by client message ID: what the feed reads count positions in ([`LocalStore::feed_rows`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MessageOrder {
  pub group: u8,
  pub value: i64,
  pub client_message_id: [u8; 16],
}

/// A write [`LocalStore::written_since`] tells of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Written {
  /// This message was written or removed.
  Message(ClientMessageId),
  /// All of the conversation's messages went.
  Conversation,
}

/// How far a conversation's feed was rewritten since the store began ([`LocalStore::feed_version`]).
/// A new message changes neither count: a reader finds those by their position.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FeedVersion {
  /// Changes when the store's indexes are built again, which starts the counts over.
  pub generation: u64,
  /// Writes of [`FeedChange::Reshaped`].
  pub reshaped: u64,
  /// Writes of [`FeedChange::Edited`].
  pub edited: u64,
}

impl FeedVersion {
  /// This version after `change`.
  pub(crate) fn after(self, change: FeedChange) -> Self {
    match change {
      FeedChange::Reshaped => Self {
        reshaped: self.reshaped.wrapping_add(1),
        ..self
      },
      FeedChange::Edited => Self {
        edited: self.edited.wrapping_add(1),
        ..self
      },
    }
  }
}

/// What rewriting or removing a stored message does to its conversation's feed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedChange {
  /// A row left, came back, moved, or changed what is counted of it (its time, sender, whether it
  /// is readable), or an event changed, which can change what the import's additions hide.
  Reshaped,
  /// A row stays where it is and only what it says changed (an edit, which can add a mention).
  Edited,
}

impl FeedChange {
  /// What replacing `earlier` with `maybe_new` (`None`: removing it) changes. A pending message
  /// changes nothing: it has no place yet that anything could have been counted up to. The same
  /// row written again counts as an edit: comparing whole messages costs more download than the
  /// rare second look at the unread rows.
  pub(crate) fn of(earlier: &StoredMessage, maybe_new: Option<&StoredMessage>) -> Option<Self> {
    if earlier.order().group != 0 {
      return None;
    }
    let is_row_after = maybe_new.is_some_and(StoredMessage::is_feed_row);
    if !earlier.is_feed_row() && !is_row_after {
      return None;
    }
    let is_same_place = maybe_new.is_some_and(|new| {
      is_row_after
        && earlier.is_feed_row()
        && !earlier.is_event()
        && !new.is_event()
        && new.order() == earlier.order()
        && new.sent_at_ms == earlier.sent_at_ms
        && new.sender_user_id == earlier.sender_user_id
        && new.maybe_unreadable.is_none() == earlier.maybe_unreadable.is_none()
    });
    Some(if is_same_place {
      Self::Edited
    } else {
      Self::Reshaped
    })
  }
}

/// Which messages with attachments [`LocalStore::attached_messages`] lists, from a message
/// outwards (the image viewer and the Files view page through them).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachedRange {
  /// Older than the message (the newest ones without one), nearest first.
  Older(Option<MessageId>),
  /// Newer than the message (the oldest ones without one), nearest first.
  Newer(Option<MessageId>),
}

/// Which attachments make a message count for [`LocalStore::attached_messages`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attached {
  /// A picture or video: what the image viewer pages through.
  Media,
  /// A file that is no picture or video.
  Document,
  /// Any file: what the Files view lists (issue #1509).
  Any,
  /// No file: a web link in the text ([`teamcraft_proto::web_links`]) of a message the channel
  /// shows (not a reply only in its thread), what the Files & links tab lists (issue #609).
  Link,
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
  /// A process that leaves the layout to the app (the iOS notification extension) found steps only
  /// the app runs (issue #1198). Nothing was changed, and the app migrates the store when it opens.
  #[error("the app hasn't brought the local database up to date yet (layout {found} of {supported})")]
  LayoutBehind { found: i64, supported: i64 },
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
  /// The entries of a table whose key starts with `prefix` (one conversation's records, keyed
  /// `<conversation>/…`), ordered by key. The default reads the whole table; a store with an index
  /// on the key reads only the range.
  fn list_prefix(&self, table: Table, prefix: &str) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
    let mut entries = self.list(table)?;
    entries.retain(|(key, _)| key.starts_with(prefix));
    Ok(entries)
  }

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
  /// [`Self::message_by_server_id`] as far as a notification reads it ([`ShownMessage`]): a store
  /// that keeps messages as records of JSON reads only that part of one, so the browser's service
  /// worker carries no reader of every kind of message (#1452). The default reads the whole message.
  fn shown_message_by_server_id(
    &self,
    conversation: ConversationId,
    server_id: MessageId,
  ) -> Result<Option<ShownMessage>, StoreError> {
    Ok(
      self
        .message_by_server_id(conversation, server_id)?
        .as_ref()
        .map(ShownMessage::from),
    )
  }
  /// Hands each message of [`Self::messages`] to `visit` by reference, for a reader that sums them
  /// up (counts, the newest, the unread ones) and keeps few: a store that holds them in memory
  /// copies none. The default reads them as [`Self::messages`] does.
  fn visit_messages(
    &self,
    conversation: ConversationId,
    filter: MessageFilter,
    visit: &mut dyn FnMut(&StoredMessage),
  ) -> Result<(), StoreError> {
    for message in self.messages(conversation, filter)? {
      visit(&message);
    }
    Ok(())
  }
  /// Hands all of `conversation`'s messages, in display order, to `read` as one slice: for a reader
  /// that looks at all of them to keep a few (a page of the feed with its reactions and replies).
  /// A store that holds them in memory copies none. The default reads them as [`Self::messages`].
  fn with_all_messages(
    &self,
    conversation: ConversationId,
    read: &mut dyn FnMut(&[StoredMessage]),
  ) -> Result<(), StoreError> {
    read(&self.messages(conversation, MessageFilter::All)?);
    Ok(())
  }
  /// The messages at one place of the display order: the server message and the lines this device
  /// derived at `position` (lines sit at the ID of what they tell of), or every pending message.
  /// A row of the UI is looked up by its ID with it. The default scans; stores with an index on the
  /// order read only that place.
  fn messages_at(
    &self,
    conversation: ConversationId,
    slot: MessageSlot,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    let mut messages = self.messages(conversation, MessageFilter::All)?;
    messages.retain(|message| slot.holds(message));
    Ok(messages)
  }
  /// A number that changes with every write to `conversation`'s messages, for a reader that keeps
  /// what it derived from them (the activity feed) until they change. `None`: the store doesn't
  /// count, read them again.
  fn message_revision(&self, conversation: ConversationId) -> Option<u64> {
    let _ = conversation;
    None
  }
  /// How far `conversation`'s feed was rewritten, kept with the messages: written in the same group
  /// as what it counts and still there after a restart, so what a reader derived from the feed and
  /// kept is checked against it. `None`: the store doesn't keep it (or its indexes aren't complete).
  fn feed_version(&self, conversation: ConversationId) -> Result<Option<FeedVersion>, StoreError> {
    let _ = conversation;
    Ok(None)
  }
  /// Where this store's journal of written messages stands now: a reader that keeps what it
  /// derived per message (a chat list's unread counts, issue #1466) asks
  /// [`Self::written_since`] from here later. `None`: the store keeps no journal.
  fn written_cursor(&self) -> Option<u64> {
    None
  }
  /// Whether another thread waits for this thread's open group of writes to end (the UI's reads,
  /// issue #1466): a long group of independent steps (a page of received messages) ends early
  /// then, and begins again after them. `false` for stores where nobody waits.
  fn is_awaited(&self) -> bool {
    false
  }

  /// The commit a reading connection's snapshot holds (issue #1476, [`CommitJournal`]). `None`: the
  /// store reads what its writes left as soon as they are made.
  fn snapshot_commit(&self) -> Option<u64> {
    None
  }

  /// The number of the last commit this store's writing connection made. `None`: commits aren't
  /// numbered.
  fn last_commit(&self) -> Option<u64> {
    None
  }

  /// A reading connection takes a new snapshot and follows the commits since its last look, so it
  /// sees every commit made before the call ([`Self::snapshot_commit`] moves up to them).
  fn renew_snapshot(&self) {}
  /// Begins a read group ([`read_together`]): the reads up to [`Self::end_reads`] see one snapshot
  /// of the disk, and other threads of this process wait for it like for a group of writes, while
  /// other processes (the iOS notification extension) still write. Groups of writes may nest in it.
  fn begin_reads(&self) -> Result<(), StoreError> {
    Ok(())
  }
  /// Ends the read group of [`Self::begin_reads`].
  fn end_reads(&self) {}
  /// What of `conversation`'s messages was written or removed since `cursor`
  /// ([`Self::written_cursor`]), maybe more. `None` when the journal can't tell: it no longer holds
  /// what came after `cursor`, or another connection wrote meanwhile.
  fn written_since(&self, conversation: ConversationId, cursor: u64) -> Option<Vec<Written>> {
    let _ = (conversation, cursor);
    None
  }
  /// The oldest or newest server message ID stored in `conversation`: the cursor of a catch-up, of
  /// older history and of the read position. Placeholders don't count (issue #888): their IDs are
  /// the server's unverified word, and one dated far ahead would move the cursor past messages
  /// never fetched. Asked on every catch-up of every conversation, so a
  /// store with an index reads only the message at that end; the default scans.
  fn edge_server_id(
    &self,
    conversation: ConversationId,
    edge: MessageEdge,
  ) -> Result<Option<MessageId>, StoreError> {
    let server_ids = self
      .messages(conversation, MessageFilter::All)?
      .into_iter()
      .filter(|message| message.maybe_unreadable.is_none())
      .filter_map(|message| message.maybe_server_id);
    Ok(match edge {
      MessageEdge::Oldest => server_ids.min(),
      MessageEdge::Newest => server_ids.max(),
    })
  }
  /// Up to `limit` listed server messages with an attachment of `kind`, in the `range`, nearest to
  /// its start first. The image viewer pages through a conversation's media with it, the Files
  /// view through its files, so a store with an index reads only what it returns. The default
  /// scans.
  fn attached_messages(
    &self,
    conversation: ConversationId,
    kind: Attached,
    range: AttachedRange,
    limit: usize,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    Ok(pick_attached(
      self.messages(conversation, MessageFilter::All)?,
      kind,
      range,
      limit,
    ))
  }
  /// A number that changes whenever a message with attachments may have been written or removed
  /// (also by another process): a reader that keeps a list of files reads it again only then.
  /// `None` when the store can't tell; the reader then reads every time.
  fn attachments_cursor(&self) -> Option<u64> {
    None
  }
  /// Whether the feed reads below ([`Self::feed_len`] to [`Self::reply_counts`]) read only what
  /// they return (issue #155): a reader then pages a conversation's feed with them instead of
  /// reading the conversation whole. Their defaults read it whole every time.
  fn indexes_feed(&self) -> bool {
    false
  }
  /// How many of `conversation`'s messages are rows of its feed ([`StoredMessage::is_feed_row`]).
  fn feed_len(&self, conversation: ConversationId) -> Result<usize, StoreError> {
    let mut len = 0;
    self.visit_messages(conversation, MessageFilter::TopLevel, &mut |message| {
      len += usize::from(message.is_feed_row());
    })?;
    Ok(len)
  }
  /// The rows of the feed at positions `start..start + len`, in display order ([`MessageOrder`]).
  fn feed_rows(
    &self,
    conversation: ConversationId,
    start: usize,
    len: usize,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    let mut rows = self.messages(conversation, MessageFilter::TopLevel)?;
    rows.retain(StoredMessage::is_feed_row);
    rows.sort_by_key(StoredMessage::order);
    Ok(rows.into_iter().skip(start).take(len).collect())
  }
  /// For each of `orders`, how many rows of the feed sort before it: where a row is, or would be.
  fn feed_ranks(
    &self,
    conversation: ConversationId,
    orders: &[MessageOrder],
  ) -> Result<Vec<usize>, StoreError> {
    let mut rows = Vec::new();
    self.visit_messages(conversation, MessageFilter::TopLevel, &mut |message| {
      if message.is_feed_row() {
        rows.push(message.order());
      }
    })?;
    rows.sort_unstable();
    Ok(
      orders
        .iter()
        .map(|order| rows.partition_point(|row| row < order))
        .collect(),
    )
  }
  /// `conversation`'s events and the lines this device derived ([`StoredMessage::is_event`]), in
  /// display order: what decides which of the import account's additions the feed leaves out
  /// (`crate::imported::ImportAdditions`). A conversation has few of them.
  fn event_messages(&self, conversation: ConversationId) -> Result<Vec<StoredMessage>, StoreError> {
    let mut events = self.messages(conversation, MessageFilter::All)?;
    events.retain(StoredMessage::is_event);
    events.sort_by_key(StoredMessage::order);
    Ok(events)
  }
  /// `conversation`'s pinned messages ([`StoredMessage::is_shown_pin`]), in display order: what the
  /// pinned panel lists (issue #1757). A store with an index of them reads only those, kept up to
  /// date by every write of a message; the default reads the conversation whole.
  fn pinned_messages(&self, conversation: ConversationId) -> Result<Vec<StoredMessage>, StoreError> {
    let mut pinned = Vec::new();
    self.visit_messages(conversation, MessageFilter::All, &mut |message| {
      if message.is_shown_pin() {
        pinned.push(message.clone());
      }
    })?;
    pinned.sort_by_key(StoredMessage::order);
    Ok(pinned)
  }
  /// The reactions to any of `targets`, in display order (the latest of a person's counts).
  fn reactions_to(
    &self,
    conversation: ConversationId,
    targets: &[ClientMessageId],
  ) -> Result<Vec<StoredMessage>, StoreError> {
    let mut reactions = self.messages(conversation, MessageFilter::All)?;
    reactions.retain(|message| {
      message
        .relation_target()
        .is_some_and(|target| targets.contains(&target))
    });
    reactions.sort_by_key(StoredMessage::order);
    Ok(reactions)
  }
  /// How many shown replies ([`StoredMessage::counts_as_reply`]) each of `threads` has, in the
  /// same order.
  fn reply_counts(
    &self,
    conversation: ConversationId,
    threads: &[ThreadId],
  ) -> Result<Vec<usize>, StoreError> {
    let mut counts = vec![0; threads.len()];
    self.visit_messages(conversation, MessageFilter::All, &mut |message| {
      if message.counts_as_reply()
        && let Some(at) = threads
          .iter()
          .position(|thread| Some(*thread) == message.maybe_thread_id)
      {
        counts[at] += 1;
      }
    })?;
    Ok(counts)
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
  /// somebody who later gets the database key (issue #391). A store may do it in the background,
  /// within the bound docs/e2ee.md §4.3.1 states (the sealed SQLite store: issue #1684);
  /// [`Self::wait_for_purge`] waits for it. Data the file system or flash controller copied
  /// elsewhere is out of reach. Stores without such files do nothing.
  fn purge_freed(&self) -> Result<(), StoreError> {
    Ok(())
  }

  /// Waits until the purges asked for so far ran, at most `max_wait`; whether they did. Stores
  /// that purge before [`Self::purge_freed`] returns keep the default.
  fn wait_for_purge(&self, max_wait: std::time::Duration) -> bool {
    let _ = max_wait;
    true
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

  /// [`Self::begin_writes`] if it wouldn't wait: `false`, and no group begun, while another thread
  /// holds the store's group (issue #1756). For writes that can wait for a later chance, on a thread
  /// whose reads must not wait. Stores that never wait keep the default.
  fn try_begin_writes(&self) -> Result<bool, StoreError> {
    self.begin_writes().map(|()| true)
  }

  /// Ends the group started by the matching [`Self::begin_writes`].
  fn end_writes(&self) -> Result<(), StoreError> {
    Ok(())
  }

  /// Makes the open group fail when it ends, as a failed write of the group would: a step of it
  /// panicked, and what it wrote so far must not reach the disk. Stores that can't undo a group keep
  /// the default. Only inside a group.
  fn fail_writes(&self) {}

  /// Starts writing what is queued now instead of after a batching delay, without waiting for it:
  /// for a write another device relies on next (the secrets of a link, issue #168). Stores that
  /// write at once keep the default.
  fn write_soon(&self) {}

  /// What the disk may hold other than what this store accepted, counted since it was opened, for
  /// a caller that remembers what it wrote instead of reading it back (the MLS state,
  /// `mls_storage`): writes the disk lost, and commits of another connection to the same database.
  /// Once `lost` moved, that caller writes everything again; `foreign` makes it look whether its
  /// records moved (and a sealed store drop its caches). `None` for a store that keeps every
  /// write it accepted and has no other writer (in memory; the browser's, which writes a failed
  /// one again until it goes through and is owned by one tab).
  fn write_tally(&self) -> Option<WriteTally> {
    None
  }

  /// Keys of `table` deleted in groups of writes the disk rolled back, which the store no longer
  /// lists although the disk may still hold them (a sealed store's cache keeps a rolled-back group);
  /// writing or deleting a key again in a group that reaches the disk forgets it. Stores that read
  /// the disk itself, or never lose a write, keep the default.
  fn lost_deletes(&self, table: Table) -> Vec<String> {
    let _ = table;
    Vec::new()
  }
}

/// Counts of [`LocalStore::write_tally`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WriteTally {
  /// Writes the disk lost: groups rolled back (one of their writes failed, or the commit did) and
  /// single writes that failed.
  pub lost: u64,
  /// Times another connection had committed to the database since the store last looked.
  pub foreign: u64,
}

/// Runs `reads` as one look at the store ([`LocalStore::begin_reads`]): many reads take the store's
/// lock and check the disk once, and no other thread's group of writes slips in between them, each
/// a wait of its own (a chat list pass, issue #1466). Other threads wait while it runs: keep it
/// short. Groups of writes inside it are their own groups.
pub fn read_together<S, T, E>(store: &S, reads: impl FnOnce() -> Result<T, E>) -> Result<T, E>
where
  S: LocalStore + ?Sized,
  E: From<StoreError>,
{
  store.begin_reads()?;
  let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(reads));
  store.end_reads();
  match result {
    Ok(result) => result,
    Err(panic) => std::panic::resume_unwind(panic),
  }
}

/// Runs `writes` as one group of writes ([`LocalStore::begin_writes`]). An error of `writes` doesn't
/// undo what it wrote before; only a crash, a failed write of the group or a panic does.
#[track_caller]
pub fn write_group<S, T, E>(store: &S, writes: impl FnOnce() -> Result<T, E>) -> Result<T, E>
where
  S: LocalStore + ?Sized,
  E: From<StoreError>,
{
  let _site = GroupSite::enter(std::panic::Location::caller());
  store.begin_writes()?;
  let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(writes));
  end_group(store, result)
}

/// [`write_group`] if the group can begin without waiting ([`LocalStore::try_begin_writes`]):
/// `None`, and `writes` not run, while another thread holds the store (issue #1756).
#[track_caller]
pub fn try_write_group<S, T, E>(store: &S, writes: impl FnOnce() -> Result<T, E>) -> Result<Option<T>, E>
where
  S: LocalStore + ?Sized,
  E: From<StoreError>,
{
  let _site = GroupSite::enter(std::panic::Location::caller());
  if !store.try_begin_writes()? {
    return Ok(None);
  }
  let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(writes));
  end_group(store, result).map(Some)
}

thread_local! {
  /// Where this thread's outermost group of writes was begun ([`GroupSite`]).
  static GROUP_SITE: std::cell::Cell<Option<&'static std::panic::Location<'static>>> =
    const { std::cell::Cell::new(None) };
}

/// Names the code that holds a group of writes open (issue #1681): a store that finds a group open
/// for long logs where it was begun, a place in the code, never data. Set by the outermost
/// [`write_group`] (or `Client::atomically`) of a thread until it returns; nested groups keep it.
pub(crate) struct GroupSite {
  is_outermost: bool,
}

impl GroupSite {
  pub(crate) fn enter(site: &'static std::panic::Location<'static>) -> Self {
    let is_outermost = GROUP_SITE.with(|current| {
      if current.get().is_some() {
        return false;
      }
      current.set(Some(site));
      true
    });
    Self { is_outermost }
  }

  /// The site of the thread's open group, as `file:line`; `None` outside [`write_group`]. Read by
  /// the SQLite stores only.
  #[cfg_attr(
    not(all(
      any(feature = "sqlcipher", feature = "sealed-sqlite"),
      not(target_arch = "wasm32")
    )),
    allow(dead_code)
  )]
  pub(crate) fn current() -> Option<&'static std::panic::Location<'static>> {
    GROUP_SITE.with(std::cell::Cell::get)
  }
}

impl Drop for GroupSite {
  fn drop(&mut self) {
    if self.is_outermost {
      GROUP_SITE.with(|current| current.set(None));
    }
  }
}

/// Ends a group of writes after its steps ran under `catch_unwind`. A panic must not leave the group
/// open, where nothing would ever be committed again, and must not commit what it wrote so far: half
/// a save of the MLS state, or one without its cursor, which the reopened client would then read.
pub(crate) fn end_group<S, T, E>(store: &S, result: std::thread::Result<Result<T, E>>) -> Result<T, E>
where
  S: LocalStore + ?Sized,
  E: From<StoreError>,
{
  if result.is_err() {
    store.fail_writes();
  }
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

      fn list_prefix(&self, table: Table, prefix: &str) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
        (**self).list_prefix(table, prefix)
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

      fn shown_message_by_server_id(
        &self,
        conversation: ConversationId,
        server_id: MessageId,
      ) -> Result<Option<ShownMessage>, StoreError> {
        (**self).shown_message_by_server_id(conversation, server_id)
      }

      fn message_revision(&self, conversation: ConversationId) -> Option<u64> {
        (**self).message_revision(conversation)
      }

      fn messages_at(
        &self,
        conversation: ConversationId,
        slot: MessageSlot,
      ) -> Result<Vec<StoredMessage>, StoreError> {
        (**self).messages_at(conversation, slot)
      }

      fn visit_messages(
        &self,
        conversation: ConversationId,
        filter: MessageFilter,
        visit: &mut dyn FnMut(&StoredMessage),
      ) -> Result<(), StoreError> {
        (**self).visit_messages(conversation, filter, visit)
      }

      fn with_all_messages(
        &self,
        conversation: ConversationId,
        read: &mut dyn FnMut(&[StoredMessage]),
      ) -> Result<(), StoreError> {
        (**self).with_all_messages(conversation, read)
      }

      fn edge_server_id(
        &self,
        conversation: ConversationId,
        edge: MessageEdge,
      ) -> Result<Option<MessageId>, StoreError> {
        (**self).edge_server_id(conversation, edge)
      }

      fn messages(
        &self,
        conversation: ConversationId,
        filter: MessageFilter,
      ) -> Result<Vec<StoredMessage>, StoreError> {
        (**self).messages(conversation, filter)
      }

      fn attached_messages(
        &self,
        conversation: ConversationId,
        kind: Attached,
        range: AttachedRange,
        limit: usize,
      ) -> Result<Vec<StoredMessage>, StoreError> {
        (**self).attached_messages(conversation, kind, range, limit)
      }

      fn attachments_cursor(&self) -> Option<u64> {
        (**self).attachments_cursor()
      }

      fn indexes_feed(&self) -> bool {
        (**self).indexes_feed()
      }

      fn feed_version(&self, conversation: ConversationId) -> Result<Option<FeedVersion>, StoreError> {
        (**self).feed_version(conversation)
      }

      fn written_cursor(&self) -> Option<u64> {
        (**self).written_cursor()
      }

      fn is_awaited(&self) -> bool {
        (**self).is_awaited()
      }

      fn snapshot_commit(&self) -> Option<u64> {
        (**self).snapshot_commit()
      }

      fn last_commit(&self) -> Option<u64> {
        (**self).last_commit()
      }

      fn renew_snapshot(&self) {
        (**self).renew_snapshot();
      }

      fn begin_reads(&self) -> Result<(), StoreError> {
        (**self).begin_reads()
      }

      fn end_reads(&self) {
        (**self).end_reads()
      }

      fn written_since(&self, conversation: ConversationId, cursor: u64) -> Option<Vec<Written>> {
        (**self).written_since(conversation, cursor)
      }

      fn feed_len(&self, conversation: ConversationId) -> Result<usize, StoreError> {
        (**self).feed_len(conversation)
      }

      fn feed_rows(
        &self,
        conversation: ConversationId,
        start: usize,
        len: usize,
      ) -> Result<Vec<StoredMessage>, StoreError> {
        (**self).feed_rows(conversation, start, len)
      }

      fn feed_ranks(
        &self,
        conversation: ConversationId,
        orders: &[MessageOrder],
      ) -> Result<Vec<usize>, StoreError> {
        (**self).feed_ranks(conversation, orders)
      }

      fn event_messages(&self, conversation: ConversationId) -> Result<Vec<StoredMessage>, StoreError> {
        (**self).event_messages(conversation)
      }

      fn pinned_messages(&self, conversation: ConversationId) -> Result<Vec<StoredMessage>, StoreError> {
        (**self).pinned_messages(conversation)
      }

      fn reactions_to(
        &self,
        conversation: ConversationId,
        targets: &[ClientMessageId],
      ) -> Result<Vec<StoredMessage>, StoreError> {
        (**self).reactions_to(conversation, targets)
      }

      fn reply_counts(
        &self,
        conversation: ConversationId,
        threads: &[ThreadId],
      ) -> Result<Vec<usize>, StoreError> {
        (**self).reply_counts(conversation, threads)
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

      fn wait_for_purge(&self, max_wait: std::time::Duration) -> bool {
        (**self).wait_for_purge(max_wait)
      }

      fn begin_writes(&self) -> Result<(), StoreError> {
        (**self).begin_writes()
      }

      fn try_begin_writes(&self) -> Result<bool, StoreError> {
        (**self).try_begin_writes()
      }

      fn end_writes(&self) -> Result<(), StoreError> {
        (**self).end_writes()
      }

      fn fail_writes(&self) {
        (**self).fail_writes()
      }

      fn write_soon(&self) {
        (**self).write_soon()
      }

      fn write_tally(&self) -> Option<WriteTally> {
        (**self).write_tally()
      }

      fn lost_deletes(&self, table: Table) -> Vec<String> {
        (**self).lost_deletes(table)
      }
    }
  };
}

forward_local_store!(&T);
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

impl ConversationEvent {
  /// A change the server records of the channel itself (issue #818): archived, restored, made
  /// private or public, moved to another space.
  pub fn is_server_change(&self) -> bool {
    matches!(
      self,
      Self::Archived | Self::Unarchived | Self::MadePrivate | Self::MadePublic | Self::Moved { .. }
    )
  }
}

fn is_false(value: &bool) -> bool {
  !*value
}

impl StoredMessage {
  /// Display order: server messages by ID, a line this device derived at its place
  /// ([`Self::line_position`], the same time-ordered IDs); pending ones after all of them, by time
  /// written.
  pub fn sort_key(&self) -> (u8, i64) {
    match (self.maybe_server_id, self.line_position()) {
      (Some(id), _) => (0, teamcraft_proto::Id::snowflake(id).get()),
      (None, Some(at)) => (0, teamcraft_proto::Id::snowflake(at).get()),
      (None, None) => (1, self.sent_at_ms),
    }
  }

  /// A line this device derived itself, from a verified commit (issue #811) or from the server's
  /// record of the conversation (issue #818): no message of the server's, and no message replaces
  /// or changes it.
  pub fn is_local_line(&self) -> bool {
    self.maybe_verified_commit.is_some() || self.maybe_server_change.is_some()
  }

  /// Where a line this device derived sits among the messages (on the server's clock, like their
  /// IDs): at the commit it comes from, or where [`ServerChange::at`] puts it. `None` for messages.
  pub fn line_position(&self) -> Option<MessageId> {
    match (&self.maybe_verified_commit, &self.maybe_server_change) {
      (Some(commit), _) => Some(teamcraft_proto::Id::from_snowflake(
        teamcraft_proto::Id::snowflake(*commit),
      )),
      (None, Some(change)) => Some(change.at),
      (None, None) => None,
    }
  }

  /// Who made the change an event tells of: for a line about a change the server reports, the
  /// person its actor names, if any ([`ChangeActor`]); for anything else its sender.
  pub fn maybe_actor(&self) -> Option<UserId> {
    match &self.maybe_server_change {
      Some(change) if !change.actor.is_named() => None,
      _ => Some(self.sender_user_id),
    }
  }

  pub fn is_reaction(&self) -> bool {
    matches!(self.content, MessageContent::Reaction { .. })
  }

  /// [`Self::sort_key`] with the tie broken.
  pub fn order(&self) -> MessageOrder {
    let (group, value) = self.sort_key();
    MessageOrder {
      group,
      value,
      client_message_id: *self.client_message_id.as_bytes(),
    }
  }

  /// The message a reaction reacts to, or a vote or close of a poll is about: what the stores
  /// index, so a page of the feed reads what is shown on its rows with it.
  pub fn relation_target(&self) -> Option<ClientMessageId> {
    match &self.content {
      MessageContent::Reaction { target, .. }
      | MessageContent::PollVote { target, .. }
      | MessageContent::PollClose { target } => Some(*target),
      _ => None,
    }
  }

  /// A row of the conversation's feed: top level (or a reply also sent to the conversation), not
  /// hidden, not deleted. The UI also leaves out the import account's additions
  /// (`crate::imported::ImportAdditions`), which only events and lines are ([`Self::is_event`]).
  pub fn is_feed_row(&self) -> bool {
    (self.maybe_thread_id.is_none() || self.is_also_in_channel())
      && !self.is_hidden()
      && !matches!(self.content, MessageContent::Deleted)
  }

  /// A shown reply of a thread. A placeholder (issue #888) may turn out to be no reply at all:
  /// counted once readable.
  pub fn counts_as_reply(&self) -> bool {
    self.maybe_thread_id.is_some()
      && !self.is_hidden()
      && !matches!(self.content, MessageContent::Deleted)
      && self.maybe_unreadable.is_none()
  }

  /// An event of the conversation or a line this device derived: the messages that can tell of
  /// the import account adding people, or be such an addition (`crate::imported`).
  pub fn is_event(&self) -> bool {
    self.line_position().is_some() || matches!(self.content, MessageContent::Event { .. })
  }

  /// Reactions, poll votes, edits, deletes and pins act on other messages and aren't listed themselves. Nor
  /// is a message saying that an admin joined: only the line this device derived from the commit
  /// shows that (issue #811), so a member can't make it up by sending one. The same goes for a
  /// message saying that the channel was archived, restored, made private or public, or moved:
  /// only the line this device derived from the server's record shows that (issue #818).
  pub fn is_hidden(&self) -> bool {
    match &self.content {
      MessageContent::Event {
        event: ConversationEvent::AdminJoined { .. },
      } => self.maybe_verified_commit.is_none(),
      MessageContent::Event { event } if event.is_server_change() => self.maybe_server_change.is_none(),
      content => matches!(
        content,
        MessageContent::Reaction { .. }
          | MessageContent::PollVote { .. }
          | MessageContent::PollClose { .. }
          | MessageContent::Edit { .. }
          | MessageContent::Delete { .. }
          | MessageContent::Pin { .. }
          | MessageContent::Bookmarks { .. }
          | MessageContent::Canvas { .. }
          | MessageContent::IndexSegment { .. }
          | MessageContent::IntegrationSource { .. }
          | MessageContent::Notetaker { .. }
          | MessageContent::ChannelRolesEntry { .. }
      ),
    }
  }

  /// A pinned message the conversation shows: what the pinned panel lists
  /// ([`LocalStore::pinned_messages`]).
  pub fn is_shown_pin(&self) -> bool {
    self.is_pinned && !self.is_hidden()
  }

  /// A thread reply the sender also posted to the conversation.
  pub fn is_also_in_channel(&self) -> bool {
    matches!(&self.content, MessageContent::Text { meta, .. } if meta.is_also_in_channel)
  }

  /// A listed server message with a picture or video attachment: what the image viewer pages
  /// through.
  pub fn has_media(&self) -> bool {
    self.has_attached(Attached::Media)
  }

  /// A listed server message with an attachment of `kind`.
  pub fn has_attached(&self, kind: Attached) -> bool {
    if kind == Attached::Link {
      // Only what the tab lists: a reply only in its thread isn't, as the channel doesn't show it.
      return self.maybe_server_id.is_some()
        && self.is_feed_row()
        && matches!(&self.content, MessageContent::Text { body, .. } if teamcraft_proto::web_links::has_web_link(body));
    }
    let attachments: &[AttachmentRef] = match &self.content {
      MessageContent::File { attachment } => std::slice::from_ref(attachment),
      MessageContent::Text { meta, .. } => &meta.attachments,
      _ => return false,
    };
    self.maybe_server_id.is_some()
      && !self.is_hidden()
      && attachments.iter().any(|attachment| match kind {
        Attached::Media => is_media_type(&attachment.media_type),
        Attached::Document => !is_media_type(&attachment.media_type),
        Attached::Any => true,
        Attached::Link => false,
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

/// Whether a media type is a picture or video, as the image viewer counts it.
fn is_media_type(media_type: &str) -> bool {
  MEDIA_TYPE_PREFIXES
    .iter()
    .any(|prefix| media_type.starts_with(prefix))
}

/// The `limit` messages with attachments of `kind` in `range` among `messages` (in display order),
/// nearest to the range's start first.
pub fn pick_attached(
  messages: Vec<StoredMessage>,
  kind: Attached,
  range: AttachedRange,
  limit: usize,
) -> Vec<StoredMessage> {
  let server_id = |message: &StoredMessage| {
    message
      .maybe_server_id
      .map(|id| teamcraft_proto::Id::snowflake(id).get())
  };
  let mut media: Vec<StoredMessage> = messages
    .into_iter()
    .filter(|message| message.has_attached(kind))
    .filter(|message| match range {
      AttachedRange::Older(Some(from)) => {
        server_id(message) < Some(teamcraft_proto::Id::snowflake(from).get())
      },
      AttachedRange::Newer(Some(from)) => {
        server_id(message) > Some(teamcraft_proto::Id::snowflake(from).get())
      },
      AttachedRange::Older(None) | AttachedRange::Newer(None) => true,
    })
    .collect();
  if matches!(range, AttachedRange::Older(_)) {
    media.reverse();
  }
  media.truncate(limit);
  media
}

impl Table {
  /// Every table; a new variant goes here too (stores that load by name skip unknown tables).
  pub const ALL: [Self; 39] = [
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
    Self::SegmentAnnouncements,
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
    Self::Downloads,
    Self::EntryTallies,
    Self::SearchIndex,
    Self::SearchQueue,
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
      Self::SegmentAnnouncements => "segment_announcements",
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
      Self::Downloads => "downloads",
      Self::EntryTallies => "entry_tallies",
      Self::SearchIndex => "search_index",
      Self::SearchQueue => "search_queue",
    }
  }
}

#[cfg(test)]
pub(crate) mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  mod poll_wire {
    use super::*;

    /// A version before polls (issue #205): its content enum without the poll kinds, and a meta
    /// that, like [`MessageMeta`], skips fields it doesn't know.
    #[derive(Debug, PartialEq, Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum OlderContent {
      Text {
        body: String,
        #[serde(default)]
        meta: OlderMeta,
      },
      Reaction {
        target: ClientMessageId,
      },
      #[serde(other)]
      Unknown,
    }

    #[derive(Debug, Default, PartialEq, Deserialize)]
    struct OlderMeta {
      #[serde(default)]
      is_silent: bool,
    }

    #[test]
    fn votes_and_closes_keep_their_wire_names() {
      let target = ClientMessageId::from_bytes([7; 16]);
      let vote = MessageContent::PollVote {
        target,
        choices: vec![0, 2],
      };
      let json = serde_json::to_value(&vote).unwrap();
      assert_eq!(json["type"], "poll_vote");
      assert_eq!(json["choices"], serde_json::json!([0, 2]));
      assert_eq!(json["target"], serde_json::to_value(target).unwrap());
      assert_eq!(serde_json::from_value::<MessageContent>(json).unwrap(), vote);
      let close = serde_json::to_value(MessageContent::PollClose { target }).unwrap();
      assert_eq!(close["type"], "poll_close");
      // A vote without choices withdraws (a newer sender may leave the empty list out).
      let mut withdrawn = close.clone();
      withdrawn["type"] = "poll_vote".into();
      assert_eq!(
        serde_json::from_value::<MessageContent>(withdrawn).unwrap(),
        MessageContent::PollVote {
          target,
          choices: Vec::new()
        }
      );
    }

    #[test]
    fn a_poll_is_a_text_with_the_poll_in_its_meta() {
      let poll = poll_text();
      let json = serde_json::to_value(&poll).unwrap();
      assert_eq!(json["type"], "text");
      assert_eq!(json["meta"]["poll"]["question"], "Lunch?");
      assert_eq!(json["meta"]["poll"]["voters"], "hidden");
      assert_eq!(serde_json::from_value::<MessageContent>(json).unwrap(), poll);
    }

    #[test]
    fn a_version_before_polls_shows_the_text_and_keeps_votes_as_unknown() {
      let poll = serde_json::to_string(&poll_text()).unwrap();
      assert_eq!(
        serde_json::from_str::<OlderContent>(&poll).unwrap(),
        OlderContent::Text {
          body: "**Poll:** Lunch?\n- Pizza\n- Salad".into(),
          meta: OlderMeta::default(),
        }
      );
      let target = ClientMessageId::from_bytes([7; 16]);
      for hidden in [
        MessageContent::PollVote {
          target,
          choices: vec![1],
        },
        MessageContent::PollClose { target },
      ] {
        let json = serde_json::to_string(&hidden).unwrap();
        assert_eq!(
          serde_json::from_str::<OlderContent>(&json).unwrap(),
          OlderContent::Unknown
        );
      }
    }

    #[test]
    fn this_version_keeps_poll_kinds_of_a_newer_one_as_unknown() {
      let json = r#"{"type":"poll_reopen","target":"x"}"#;
      assert_eq!(
        serde_json::from_str::<MessageContent>(json).unwrap(),
        MessageContent::Unknown
      );
    }

    #[test]
    fn votes_and_closes_are_hidden_and_name_their_poll() {
      let target = ClientMessageId::from_bytes([7; 16]);
      let mut vote = message(id(1), 2, Some(20), None, 20);
      vote.content = MessageContent::PollVote {
        target,
        choices: vec![0],
      };
      assert!(vote.is_hidden() && !vote.is_feed_row());
      assert_eq!(vote.relation_target(), Some(target));
      vote.content = MessageContent::PollClose { target };
      assert!(vote.is_hidden());
      assert_eq!(vote.relation_target(), Some(target));
    }

    fn poll_text() -> MessageContent {
      MessageContent::Text {
        body: "**Poll:** Lunch?\n- Pizza\n- Salad".into(),
        mentions: Vec::new(),
        meta: MessageMeta {
          maybe_poll: Some(Box::new(teamcraft_proto::PollSpec {
            question: "Lunch?".into(),
            options: vec!["Pizza".into(), "Salad".into()],
            choice: teamcraft_proto::PollChoice::Single,
            voters: teamcraft_proto::PollVoters::Hidden,
          })),
          ..MessageMeta::default()
        },
      }
    }
  }

  #[test]
  fn a_group_is_named_after_its_outermost_beginning() {
    let store = MemoryStore::new();
    assert!(GroupSite::current().is_none());
    let outer_line = line!() + 1;
    write_group(&store, || -> Result<(), StoreError> {
      write_group(&store, || -> Result<(), StoreError> {
        let site = GroupSite::current().unwrap();
        assert_eq!((site.file(), site.line()), (file!(), outer_line));
        Ok(())
      })
    })
    .unwrap();
    assert!(GroupSite::current().is_none());
  }

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
    attached_messages_are_paged_from_a_message_outwards(store);
    messages_with_web_links_are_listed_newest_first(store);
    admin_joins_show_only_as_lines_from_commits(store);
    server_changes_show_only_as_lines(store);
    edge_server_ids_skip_lines_pending_messages_and_placeholders(store);
    prefixes_list_only_their_keys(store);
    a_place_holds_its_message_and_lines_and_pending_ones_their_own(store);
    feed_reads_follow_every_write(store);
    pinned_messages_follow_every_write(store);
  }

  /// Issue #1757: the pinned panel's read answers what a whole read of the conversation says,
  /// through pins and unpins (local or synced: both are writes of the message), a pending message
  /// that was sent, an edit, a delete and an expiry (the message rewritten or removed), and the
  /// conversation's removal.
  fn pinned_messages_follow_every_write(store: &impl LocalStore) {
    let conversation = id(70);
    let other = id(71);
    let pinned_ids = |conversation| -> Vec<ClientMessageId> {
      let pinned = store.pinned_messages(conversation).unwrap();
      let mut all = store.messages(conversation, MessageFilter::All).unwrap();
      all.sort_by_key(StoredMessage::order);
      let expected: Vec<StoredMessage> = all.into_iter().filter(StoredMessage::is_shown_pin).collect();
      assert_eq!(pinned, expected, "as a whole read finds them");
      pinned.iter().map(|message| message.client_message_id).collect()
    };
    let mut pending = message(conversation, 71, None, None, 9_000);
    let mut first = message(conversation, 72, Some(2_000), None, 2_000);
    let mut second = message(conversation, 73, Some(2_100), None, 2_100);
    let mut reply = message(conversation, 74, Some(2_200), Some(2_000), 2_200);
    let plain = message(conversation, 75, Some(2_300), None, 2_300);
    // A pin that is itself hidden (a control message) is no pin of the panel.
    let mut control = message(conversation, 76, Some(2_400), None, 2_400);
    control.content = MessageContent::Pin {
      target: first.client_message_id,
      is_pinned: true,
    };
    control.is_pinned = true;
    let mut elsewhere = message(other, 77, Some(2_500), None, 2_500);
    elsewhere.is_pinned = true;
    for stored in [&pending, &first, &second, &reply, &plain, &control, &elsewhere] {
      store.upsert_message(stored).unwrap();
    }
    assert!(pinned_ids(conversation).is_empty());
    assert_eq!(pinned_ids(other), [elsewhere.client_message_id]);

    // Pinned out of display order, a pending message and a thread reply among them.
    for stored in [&mut second, &mut first, &mut pending, &mut reply] {
      stored.is_pinned = true;
      store.upsert_message(stored).unwrap();
    }
    assert_eq!(
      pinned_ids(conversation),
      [
        first.client_message_id,
        second.client_message_id,
        reply.client_message_id,
        pending.client_message_id
      ]
    );

    // Sent: the pending one moves to its server ID and stays pinned.
    pending.maybe_server_id = Some(id(2_050));
    store.upsert_message(&pending).unwrap();
    // Edited: still pinned.
    first.content = MessageContent::Text {
      body: "edited".into(),
      mentions: Vec::new(),
      meta: Default::default(),
    };
    store.upsert_message(&first).unwrap();
    // Unpinned.
    second.is_pinned = false;
    store.upsert_message(&second).unwrap();
    assert_eq!(
      pinned_ids(conversation),
      [
        first.client_message_id,
        pending.client_message_id,
        reply.client_message_id
      ]
    );

    // Deleted for everyone: what the record says decides, as a whole read does.
    reply.content = MessageContent::Deleted;
    store.upsert_message(&reply).unwrap();
    pinned_ids(conversation);
    // Removed (expired, or dropped from the outbox).
    store
      .delete_message(conversation, first.client_message_id)
      .unwrap();
    assert!(
      !pinned_ids(conversation).contains(&first.client_message_id),
      "a removed message is no pin"
    );

    // The conversation goes: none of its pins stay, another conversation's do.
    store.delete_messages(conversation).unwrap();
    assert!(store.pinned_messages(conversation).unwrap().is_empty());
    assert_eq!(pinned_ids(other), [elsewhere.client_message_id]);
    store.upsert_message(&plain).unwrap();
    assert!(pinned_ids(conversation).is_empty(), "nothing comes back");
    store.delete_messages(other).unwrap();
  }

  /// Issue #155: the feed reads answer what a whole read of the conversation says, through sends,
  /// deletes, a pending message that was sent, and reactions and replies coming and going.
  fn feed_reads_follow_every_write(store: &impl LocalStore) {
    let conversation = id(60);
    let mut pending = message(conversation, 61, None, None, 9_000);
    let mut root = message(conversation, 62, Some(1_000), None, 1_000);
    let mut reply = message(conversation, 63, Some(1_100), Some(1_000), 1_100);
    let mut reaction = message(conversation, 64, Some(1_200), None, 1_200);
    reaction.content = MessageContent::Reaction {
      target: root.client_message_id,
      emoji: "+1".into(),
      is_added: true,
      maybe_custom: None,
    };
    let mut joined = message(conversation, 65, Some(1_300), None, 1_300);
    joined.content = MessageContent::Event {
      event: ConversationEvent::Joined,
    };
    let later = message(conversation, 66, Some(1_400), None, 1_400);
    // Votes and closes of a poll are read with the reactions (issue #205).
    let mut vote = message(conversation, 67, Some(1_250), None, 1_250);
    vote.content = MessageContent::PollVote {
      target: root.client_message_id,
      choices: vec![0],
    };
    let mut close = message(conversation, 68, Some(1_450), None, 1_450);
    close.content = MessageContent::PollClose {
      target: later.client_message_id,
    };
    for stored in [&pending, &root, &reply, &reaction, &joined, &later, &vote, &close] {
      store.upsert_message(stored).unwrap();
    }
    assert_eq!(
      store
        .reactions_to(conversation, &[root.client_message_id, later.client_message_id])
        .unwrap()
        .len(),
      3
    );
    check_feed_reads(
      store,
      conversation,
      &[root.client_message_id, later.client_message_id],
    );
    assert_eq!(store.feed_len(conversation).unwrap(), 4);
    assert_eq!(
      store.reply_counts(conversation, &[id(1_000), id(1_400)]).unwrap(),
      [1, 0]
    );
    assert_eq!(store.event_messages(conversation).unwrap(), vec![joined.clone()]);

    pending.maybe_server_id = Some(id(1_050));
    store.upsert_message(&pending).unwrap();
    reply.content = MessageContent::Deleted;
    store.upsert_message(&reply).unwrap();
    reaction.content = MessageContent::Text {
      body: "no reaction any more".into(),
      mentions: Vec::new(),
      meta: Default::default(),
    };
    store.upsert_message(&reaction).unwrap();
    vote.content = MessageContent::Deleted;
    store.upsert_message(&vote).unwrap();
    root.content = MessageContent::Deleted;
    store.upsert_message(&root).unwrap();
    store
      .delete_message(conversation, joined.client_message_id)
      .unwrap();
    check_feed_reads(
      store,
      conversation,
      &[root.client_message_id, later.client_message_id],
    );
    assert_eq!(store.reply_counts(conversation, &[id(1_000)]).unwrap(), [0]);
    assert!(store.event_messages(conversation).unwrap().is_empty());
    assert!(
      store
        .reactions_to(conversation, &[root.client_message_id])
        .unwrap()
        .is_empty()
    );

    store.delete_messages(conversation).unwrap();
    assert_eq!(store.feed_len(conversation).unwrap(), 0);
    assert!(store.feed_rows(conversation, 0, 10).unwrap().is_empty());
  }

  /// Compares every feed read of `conversation` with what its messages, read whole, say.
  fn check_feed_reads(store: &impl LocalStore, conversation: ConversationId, targets: &[ClientMessageId]) {
    let mut all = store.messages(conversation, MessageFilter::All).unwrap();
    all.sort_by_key(StoredMessage::order);
    let rows: Vec<StoredMessage> = all
      .iter()
      .filter(|message| message.is_feed_row())
      .cloned()
      .collect();
    assert_eq!(store.feed_len(conversation).unwrap(), rows.len());
    for start in 0..=rows.len() {
      for len in [0, 1, 2, rows.len()] {
        let expected: Vec<StoredMessage> = rows.iter().skip(start).take(len).cloned().collect();
        assert_eq!(store.feed_rows(conversation, start, len).unwrap(), expected);
      }
    }
    let orders: Vec<MessageOrder> = all.iter().map(StoredMessage::order).collect();
    let ranks: Vec<usize> = orders
      .iter()
      .map(|order| rows.iter().filter(|row| row.order() < *order).count())
      .collect();
    assert_eq!(store.feed_ranks(conversation, &orders).unwrap(), ranks);
    let events: Vec<StoredMessage> = all.iter().filter(|message| message.is_event()).cloned().collect();
    assert_eq!(store.event_messages(conversation).unwrap(), events);
    let reactions: Vec<StoredMessage> = all
      .iter()
      .filter(|message| {
        message
          .relation_target()
          .is_some_and(|target| targets.contains(&target))
      })
      .cloned()
      .collect();
    assert_eq!(store.reactions_to(conversation, targets).unwrap(), reactions);
  }

  /// Issue #818: a line about a change the server reports sits where it was put (or moved to) and
  /// is listed; a member's message saying the same is kept but never listed.
  fn a_place_holds_its_message_and_lines_and_pending_ones_their_own(store: &impl LocalStore) {
    let conversation = id(43);
    let mut line = message(conversation, 1, None, None, 0);
    line.content = MessageContent::Event {
      event: ConversationEvent::Archived,
    };
    line.maybe_server_change = Some(ServerChange {
      at: id(200),
      seen_after: id(200),
      seen_until: id(200),
      actor: ChangeActor::Nobody,
    });
    for stored in [
      &line,
      &message(conversation, 2, Some(200), None, 2_000),
      &message(conversation, 3, Some(300), None, 3_000),
      &message(conversation, 4, None, None, 9_000),
    ] {
      store.upsert_message(stored).unwrap();
    }
    let clients = |slot| -> Vec<u8> {
      let mut found: Vec<u8> = store
        .messages_at(conversation, slot)
        .unwrap()
        .iter()
        .map(|message| message.client_message_id.as_bytes()[0])
        .collect();
      found.sort_unstable();
      found
    };

    assert_eq!(clients(MessageSlot::At(id(200))), [1, 2]);
    assert_eq!(clients(MessageSlot::At(id(300))), [3]);
    assert_eq!(clients(MessageSlot::At(id(400))), Vec::<u8>::new());
    assert_eq!(clients(MessageSlot::Pending), [4]);
    store.delete_messages(conversation).unwrap();
  }

  fn prefixes_list_only_their_keys(store: &impl LocalStore) {
    for key in ["7/a", "7/b", "70/a", "7", "8/a"] {
      store.put(Table::Undecrypted, key, key.as_bytes()).unwrap();
    }
    store.put(Table::ReadReceipts, "7/c", b"other table").unwrap();

    let listed: Vec<String> = store
      .list_prefix(Table::Undecrypted, "7/")
      .unwrap()
      .into_iter()
      .map(|(key, _)| key)
      .collect();
    assert_eq!(listed, ["7/a", "7/b"]);
    assert!(store.list_prefix(Table::Undecrypted, "9/").unwrap().is_empty());
    for key in ["7/a", "7/b", "70/a", "7", "8/a"] {
      store.delete(Table::Undecrypted, key).unwrap();
    }
    store.delete(Table::ReadReceipts, "7/c").unwrap();
  }

  fn edge_server_ids_skip_lines_pending_messages_and_placeholders(store: &impl LocalStore) {
    let conversation = id(41);
    assert_eq!(
      store.edge_server_id(conversation, MessageEdge::Newest).unwrap(),
      None
    );
    let line_at = |client: u8, at: i64| {
      let mut line = message(conversation, client, None, None, 0);
      line.content = MessageContent::Event {
        event: ConversationEvent::Archived,
      };
      line.maybe_server_change = Some(ServerChange {
        at: id(at),
        seen_after: id(at),
        seen_until: id(at),
        actor: ChangeActor::Nobody,
      });
      line
    };
    let pending = message(conversation, 5, None, None, 9_000);
    // Placeholders (issue #888) at both ends: their IDs are the server's unverified word, and one
    // dated far ahead must not move the catch-up cursor past messages never fetched.
    let placeholder_at = |client: u8, at: i64| {
      let mut placeholder = message(conversation, client, Some(at), None, at * 10);
      placeholder.content = MessageContent::Unknown;
      placeholder.maybe_unreadable = Some(Unreadable::Waiting { since_ms: 0 });
      placeholder
    };
    for stored in [
      &line_at(1, 50),
      &placeholder_at(6, 10),
      &message(conversation, 2, Some(100), None, 1_000),
      &message(conversation, 3, Some(300), None, 3_000),
      &line_at(4, 500),
      &placeholder_at(7, 1_000_000),
      &pending,
    ] {
      store.upsert_message(stored).unwrap();
    }

    assert_eq!(
      store.edge_server_id(conversation, MessageEdge::Oldest).unwrap(),
      Some(id(100))
    );
    assert_eq!(
      store.edge_server_id(conversation, MessageEdge::Newest).unwrap(),
      Some(id(300))
    );
    assert_eq!(store.edge_server_id(id(42), MessageEdge::Oldest).unwrap(), None);
    store.delete_messages(conversation).unwrap();
  }

  fn server_changes_show_only_as_lines(store: &impl LocalStore) {
    let conversation = id(40);
    let archived = MessageContent::Event {
      event: ConversationEvent::Archived,
    };
    let before = message(conversation, 1, Some(100), None, 1_000);
    let after = message(conversation, 2, Some(300), None, 3_000);
    let mut line = message(conversation, 3, None, None, 2_000);
    line.content = archived.clone();
    line.maybe_server_change = Some(ServerChange {
      at: id(200),
      seen_after: id(100),
      seen_until: id(100),
      actor: ChangeActor::Nobody,
    });
    let mut announced = message(conversation, 4, Some(250), None, 2_500);
    announced.content = archived;
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
    assert_eq!(store.message_by_server_id(conversation, id(200)).unwrap(), None);
    store.delete_messages(conversation).unwrap();
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

  fn attached_messages_are_paged_from_a_message_outwards(store: &impl LocalStore) {
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
    let servers_of = |kind, range, limit| -> Vec<i64> {
      store
        .attached_messages(conversation, kind, range, limit)
        .unwrap()
        .iter()
        .map(|stored| Id::snowflake(stored.maybe_server_id.unwrap()).get())
        .collect()
    };
    let servers = |range, limit| servers_of(Attached::Media, range, limit);
    assert_eq!(
      servers(AttachedRange::Older(None), 3),
      [110, 100, 80],
      "newest first, no documents or texts"
    );
    assert_eq!(servers(AttachedRange::Older(Some(id(80))), 10), [60, 40, 20]);
    assert_eq!(
      servers(AttachedRange::Newer(Some(id(60))), 2),
      [80, 100],
      "nearest first"
    );
    assert_eq!(servers(AttachedRange::Newer(Some(id(100))), 10), [110]);
    assert!(servers(AttachedRange::Newer(Some(id(110))), 10).is_empty());
    assert!(servers(AttachedRange::Older(None), 0).is_empty());
    assert_eq!(
      servers(AttachedRange::Newer(None), 2),
      [20, 40],
      "the oldest first"
    );
    // The Files view (issue #1509): every file, or only documents.
    assert_eq!(
      servers_of(Attached::Any, AttachedRange::Older(None), 4),
      [120, 110, 100, 80],
      "documents too, still no texts or unsent messages"
    );
    assert_eq!(
      servers_of(Attached::Any, AttachedRange::Older(Some(id(110))), 2),
      [100, 80]
    );
    assert_eq!(
      servers_of(Attached::Document, AttachedRange::Older(None), 10),
      [120]
    );
    assert_eq!(
      servers_of(Attached::Document, AttachedRange::Newer(None), 10),
      [120]
    );
    assert!(
      store
        .attached_messages(id(21), Attached::Media, AttachedRange::Older(None), 10)
        .unwrap()
        .is_empty(),
      "other conversations have their own"
    );
    store.delete_messages(conversation).unwrap();
  }

  /// Issue #609: the Files & links tab pages through the messages whose text has a web link.
  fn messages_with_web_links_are_listed_newest_first(store: &impl LocalStore) {
    let conversation = id(22);
    let text = |body: &str| MessageContent::Text {
      body: body.into(),
      mentions: Vec::new(),
      meta: Default::default(),
    };
    let bodies = [
      "See https://example.com/a.",
      "no link",
      "HTTP://EXAMPLE.ORG/upper",
      // Has the marker a store may look for, but no web link.
      "ftp://example.com/file and app://open",
      "[docs](https://example.com/docs)",
      "edited away below: https://example.com/old",
    ];
    for (client, body) in (1..=6u8).zip(bodies) {
      let mut stored = message(
        conversation,
        client,
        Some(i64::from(client) * 10),
        None,
        i64::from(client),
      );
      stored.content = text(body);
      store.upsert_message(&stored).unwrap();
    }
    let mut edited = message(conversation, 6, Some(60), None, 6);
    edited.content = text("no link any more");
    store.upsert_message(&edited).unwrap();
    let mut deleted = message(conversation, 7, Some(70), None, 7);
    deleted.content = text("https://example.com/deleted");
    store.upsert_message(&deleted).unwrap();
    deleted.content = MessageContent::Deleted;
    store.upsert_message(&deleted).unwrap();
    let mut pending = message(conversation, 8, None, None, 8);
    pending.content = text("https://example.com/unsent");
    store.upsert_message(&pending).unwrap();
    let mut thread_only = message(conversation, 10, Some(100), Some(10), 10);
    thread_only.content = text("in the thread: https://example.com/thread");
    store.upsert_message(&thread_only).unwrap();
    let mut file = message(conversation, 9, Some(90), None, 9);
    file.content = media_content("application/pdf");
    store.upsert_message(&file).unwrap();

    let servers = |range, limit| -> Vec<i64> {
      store
        .attached_messages(conversation, Attached::Link, range, limit)
        .unwrap()
        .iter()
        .map(|stored| Id::snowflake(stored.maybe_server_id.unwrap()).get())
        .collect()
    };
    assert_eq!(
      servers(AttachedRange::Older(None), 10),
      [50, 30, 10],
      "no text without a web link, no edited-away, deleted, unsent, thread-only or file message"
    );
    assert_eq!(servers(AttachedRange::Older(None), 2), [50, 30]);
    assert_eq!(servers(AttachedRange::Older(Some(id(30))), 10), [10]);
    assert_eq!(servers(AttachedRange::Newer(None), 1), [10]);
    assert!(
      store
        .attached_messages(conversation, Attached::Any, AttachedRange::Older(None), 10)
        .unwrap()
        .iter()
        .all(|stored| stored.maybe_server_id == Some(id(90))),
      "a link is no file"
    );
    store.delete_messages(conversation).unwrap();
  }

  pub(crate) fn media_content(media_type: &str) -> MessageContent {
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

  /// Issue #840: what a group wrote before a panic stays out, the panic counts as lost writes, and
  /// the store goes on. A store that can undo a group calls this ([`exercise_store`] also runs on
  /// the one in memory, which can't).
  #[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "sqlcipher", feature = "sealed-sqlite")
  ))]
  pub(crate) fn a_panic_undoes_its_group(store: &impl LocalStore) {
    store.put(Table::Spaces, "before", b"1").unwrap();
    let lost_before = store.write_tally().unwrap().lost;

    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      write_group(store, || -> Result<(), StoreError> {
        store.put(Table::Spaces, "half", b"2")?;
        write_group(store, || -> Result<(), StoreError> {
          store.put(Table::Spaces, "inner", b"3")?;
          panic!("made to panic inside a group of writes");
        })
      })
    }));

    assert!(panicked.is_err(), "the panic goes on");
    assert_eq!(store.get(Table::Spaces, "half").unwrap(), None);
    assert_eq!(store.get(Table::Spaces, "inner").unwrap(), None);
    assert_eq!(store.get(Table::Spaces, "before").unwrap(), Some(b"1".to_vec()));
    assert_eq!(store.write_tally().unwrap().lost, lost_before + 1);
    write_group(store, || store.put(Table::Spaces, "after", b"4")).unwrap();
    assert_eq!(store.get(Table::Spaces, "after").unwrap(), Some(b"4".to_vec()));
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
      maybe_custom: None,
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

  pub(crate) fn message(
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
      maybe_server_change: None,
      maybe_unreadable: None,
    }
  }

  pub(crate) fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
