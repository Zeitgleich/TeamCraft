//! What the UI can do beyond plain messages, and the extra data it shows for them: rich sending
//! (replies, priority, acknowledgements, silent, files), edits and deletes, forwarding, pins, saved
//! items, drafts, permalinks, files and share links, link previews, and managing spaces and
//! channels. Used by the [`crate::backend::ChatBackend`] methods of the same names.
//!
//! Kept apart from [`crate::model`] so existing view models stay as they are: per-message extras
//! come from [`crate::backend::ChatBackend::message_extras`] for the rows on screen.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{CategoryId, HistoryPolicy, IdempotencyKey};

use crate::model::{ConversationId, Message, MessageId, SpaceId, ThreadId, TimestampMs, UserId};

/// How strongly a message asks for attention (Teams-style "important" / "urgent").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessagePriority {
  #[default]
  Normal,
  Important,
  /// Notifies repeatedly until read; also bypasses do-not-disturb on the server's push routing.
  Urgent,
}

/// A message to send, with everything the composer can set.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutgoingMessage {
  pub conversation: ConversationId,
  /// Thread reply when set.
  #[serde(default)]
  pub maybe_thread: Option<ThreadId>,
  pub body: String,
  /// Quotes another message of the conversation (the reference travels encrypted).
  #[serde(default)]
  pub maybe_reply_to: Option<MessageId>,
  #[serde(default)]
  pub priority: MessagePriority,
  /// Asks every reader to confirm they have read it.
  #[serde(default)]
  pub wants_acknowledgement: bool,
  /// Recipients' devices don't notify (no sound, no banner).
  #[serde(default)]
  pub is_silent: bool,
  /// A thread reply that is also posted to the conversation.
  #[serde(default)]
  pub also_send_to_channel: bool,
  #[serde(default)]
  pub files: Vec<OutgoingFile>,
}

/// A file to attach. Large files: prefer [`FileSource::Path`] where the backend can read files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutgoingFile {
  pub file_name: String,
  pub media_type: String,
  pub source: FileSource,
  /// A thumbnail the app made itself (PNG or JPEG), for formats the client core can't decode but
  /// the platform can: the browser draws SVG, AVIF and HEIC. The core's own thumbnail wins.
  #[serde(rename = "thumbnail", default, skip_serializing_if = "Option::is_none")]
  pub maybe_thumbnail: Option<Vec<u8>>,
  /// How long a video plays, read by the app from the file (the core has no video decoder).
  #[serde(rename = "duration_ms", default, skip_serializing_if = "Option::is_none")]
  pub maybe_duration_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileSource {
  Bytes {
    bytes: Vec<u8>,
  },
  /// A path on the machine running the client core (desktop and mobile shells).
  Path {
    path: String,
  },
  /// A file picked in the browser, registered with the client core in the same page
  /// (`teamcraft_core::picked_files::register`): read in slices, never whole.
  Picked {
    handle: String,
    size_bytes: u64,
  },
}

/// Everything about a message that [`Message`] doesn't carry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageExtras {
  pub message: MessageId,
  #[serde(default)]
  pub maybe_reply: Option<ReplyPreview>,
  #[serde(default)]
  pub maybe_forwarded: Option<ForwardedFrom>,
  #[serde(default)]
  pub priority: MessagePriority,
  #[serde(default)]
  pub wants_acknowledgement: bool,
  /// Who acknowledged it (shown to the sender; readers see whether they did).
  #[serde(default)]
  pub acknowledged_by: Vec<UserId>,
  #[serde(default)]
  pub is_pinned: bool,
  #[serde(default)]
  pub is_saved: bool,
  #[serde(default)]
  pub is_silent: bool,
  /// Deleted for everyone: show a placeholder instead of the body.
  #[serde(default)]
  pub is_deleted: bool,
  /// A thread reply that was also posted to the conversation.
  #[serde(default)]
  pub is_also_in_channel: bool,
  /// Earlier versions, oldest first (empty if never edited).
  #[serde(default)]
  pub edits: Vec<MessageEdit>,
  #[serde(default)]
  pub files: Vec<FileInfo>,
  #[serde(default)]
  pub maybe_link_preview: Option<LinkPreview>,
  /// What a system message reports, for events [`crate::model::SystemEvent`] doesn't cover.
  #[serde(default)]
  pub maybe_workspace_event: Option<WorkspaceEvent>,
  /// An own message the server rejected: offer "retry" ([`crate::remote::ChatAction::RetrySend`])
  /// or "delete" ([`crate::remote::ChatAction::DiscardUnsent`]) instead of "sending…" forever.
  #[serde(default)]
  pub is_send_failed: bool,
  /// An own message every other member read, in a conversation whose receipts show as ticks
  /// ([`crate::receipts::ReadIndicator::Ticks`]): its two ticks (#568, #769). Never set without
  /// receipts (channels, group DMs, receipts off on either side).
  #[serde(default)]
  pub is_read: bool,
}

impl MessageExtras {
  /// A plain message: nothing beyond what [`Message`] has.
  pub fn new(message: MessageId) -> Self {
    Self {
      message,
      maybe_reply: None,
      maybe_forwarded: None,
      priority: MessagePriority::Normal,
      wants_acknowledgement: false,
      acknowledged_by: Vec::new(),
      is_pinned: false,
      is_saved: false,
      is_silent: false,
      is_deleted: false,
      is_also_in_channel: false,
      edits: Vec::new(),
      files: Vec::new(),
      maybe_link_preview: None,
      maybe_workspace_event: None,
      is_send_failed: false,
      is_read: false,
    }
  }
}

/// The quoted message, as much as the reader can see of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyPreview {
  /// The quoted message, if this device has it.
  #[serde(default)]
  pub maybe_message: Option<MessageId>,
  pub author: UserId,
  /// The start of the quoted text; empty if the quoted message is gone or not loaded.
  pub excerpt: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardedFrom {
  pub author_name: String,
  #[serde(default)]
  pub maybe_conversation_name: Option<String>,
  pub sent_at: TimestampMs,
}

/// One earlier version of an edited message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageEdit {
  pub body: String,
  pub replaced_at: TimestampMs,
}

/// Largest file the image and video viewer opens in memory, on every platform; bigger files are
/// only saved. The client core enforces the same limit on the bytes it receives
/// (`teamcraft_core::MAX_VIEWER_BYTES`).
pub const MAX_VIEWER_BYTES: u64 = 128 * 1024 * 1024;

/// An attached file and where its bytes are.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileInfo {
  pub conversation: ConversationId,
  pub message: MessageId,
  /// Identifies the file across devices (the server's attachment ID).
  pub file_id: String,
  pub file_name: String,
  pub media_type: String,
  pub size_bytes: u64,
  /// A small preview for images (JPEG, or PNG for transparent ones), made by the sender's client.
  #[serde(default)]
  pub maybe_thumbnail_png: Option<Vec<u8>>,
  pub transfer: FileTransfer,
  /// Who sent the message the file belongs to (shown in the image viewer).
  #[serde(default)]
  pub maybe_sender: Option<UserId>,
  /// When that message was sent.
  #[serde(default)]
  pub maybe_sent_at: Option<TimestampMs>,
  /// Width and height of a picture, from the sender; reserves its space before anything loads.
  #[serde(default)]
  pub maybe_dimensions: Option<PixelSize>,
  /// How long a video plays, from the sender.
  #[serde(default)]
  pub maybe_duration_ms: Option<u64>,
  /// SHA-256 of the file's ciphertext as the message names it (64 hex digits); keys the persistent
  /// cache of its rendition (`media::MediaKey::Rendition`). Absent for files still going up.
  #[serde(rename = "content_sha256", default, skip_serializing_if = "Option::is_none")]
  pub maybe_content_sha256: Option<String>,
}

impl FileInfo {
  /// A picture or a video: what the image viewer pages through.
  pub fn is_picture_or_video(&self) -> bool {
    self.media_type.starts_with("image/") || self.media_type.starts_with("video/")
  }
}

/// Where in a conversation's pictures and videos a page of the image viewer starts (#274).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "message", rename_all = "snake_case")]
pub enum MediaAnchor {
  /// The newest ones.
  Newest,
  /// Half a page each side of the message (the newest ones when it is gone).
  Around(MessageId),
  /// Older than the message.
  Older(MessageId),
  /// Newer than the message.
  Newer(MessageId),
}

/// A page of the files of messages with pictures or videos, newest first, and whether the
/// conversation has more beyond either end (`Older`/`Newer` pages only know the end they walk to).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaPage {
  pub files: Vec<FileInfo>,
  pub has_older: bool,
  pub has_newer: bool,
}

impl MediaPage {
  /// The page of `anchor` among `files` (newest first, all of the conversation): for backends that
  /// hold the whole list anyway. `max_messages` counts messages with pictures or videos.
  pub fn from_files(files: Vec<FileInfo>, anchor: MediaAnchor, max_messages: usize) -> Self {
    let mut messages: Vec<(MessageId, Vec<FileInfo>)> = Vec::new();
    for file in files {
      match messages.last_mut() {
        Some((message, group)) if *message == file.message => group.push(file),
        _ => messages.push((file.message, vec![file])),
      }
    }
    messages.retain(|(_, group)| group.iter().any(FileInfo::is_picture_or_video));
    let position = |id: MessageId| messages.iter().position(|(message, _)| *message == id);
    // The range of `messages` the page shows.
    let range = match anchor {
      MediaAnchor::Newest => 0..max_messages.min(messages.len()),
      MediaAnchor::Around(id) => {
        let start = position(id).unwrap_or(0).saturating_sub(max_messages / 2);
        start..(start + max_messages).min(messages.len())
      },
      MediaAnchor::Older(id) => {
        let start = position(id).map_or(messages.len(), |at| at + 1);
        start..(start + max_messages).min(messages.len())
      },
      MediaAnchor::Newer(id) => {
        let end = position(id).unwrap_or(0);
        end.saturating_sub(max_messages)..end
      },
    };
    let (has_newer, has_older) = (range.start > 0, range.end < messages.len());
    Self {
      files: messages[range]
        .iter()
        .flat_map(|(_, group)| group.clone())
        .collect(),
      has_older,
      has_newer,
    }
  }
}

/// A size in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PixelSize {
  pub width: u32,
  pub height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum FileTransfer {
  /// On the server only; [`crate::backend::ChatBackend::download_file`] fetches it.
  Remote,
  Uploading {
    done_bytes: u64,
    total_bytes: u64,
    /// Attempts that failed so far; retried by itself with growing pauses until
    /// [`Self::UploadStalled`]. The UI says "retrying" instead of a progress that doesn't move.
    #[serde(default, skip_serializing_if = "is_zero")]
    failed_attempts: u32,
  },
  Downloading {
    done_bytes: u64,
    total_bytes: u64,
  },
  /// Decrypted on this device.
  Local {
    path: String,
  },
  /// A download (or an older failed transfer) that failed; `download_file` tries again.
  Failed {
    reason: FileFailure,
  },
  /// An upload that stopped retrying by itself after repeated failures. The message waits for it;
  /// [`crate::backend::ChatBackend::retry_send`] on the message tries again.
  UploadStalled {
    reason: FileFailure,
  },
  /// An upload that failed for good (too large, unreadable); the message went out without it.
  UploadFailed {
    reason: FileFailure,
  },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileFailure {
  TooLarge,
  Network,
  /// The blob doesn't match what the sender encrypted (tampered or corrupted).
  Corrupted,
  /// Reading or writing the file on this device failed.
  LocalFile,
  /// The file storage (or the server) answered "unavailable": an outage a later retry gets
  /// through (#742).
  StorageUnavailable,
}

/// The sender's preview of a link, inside the encrypted message (e2ee.md §12).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkPreview {
  pub url: String,
  #[serde(default)]
  pub maybe_title: Option<String>,
  #[serde(default)]
  pub maybe_description: Option<String>,
  #[serde(default)]
  pub maybe_site_name: Option<String>,
  /// A small JPEG or PNG.
  #[serde(default)]
  pub maybe_image: Option<Vec<u8>>,
}

/// A saved message with where it lives.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedItem {
  pub message: Message,
  pub conversation_name: String,
  pub saved_at: TimestampMs,
}

/// Options of a public download link for one file (e2ee.md §4.5.1). `Debug` hides the password.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareLinkOptions {
  /// How long the link lasts, counted by the server from the request (never from this device's
  /// clock) and shortened to the instance maximum.
  #[serde(default)]
  pub maybe_expires_in_seconds: Option<u64>,
  #[serde(default)]
  pub maybe_max_downloads: Option<u32>,
  /// Wraps the file key once more; the downloader needs it in addition to the link.
  #[serde(default)]
  pub maybe_password: Option<String>,
}

/// Shortest password a share link may have: anyone with the link can try passwords offline, so a
/// short one protects little (`teamcraft_crypto::share_link::MIN_PASSWORD_CHARS` enforces it).
pub const MIN_SHARE_PASSWORD_CHARS: usize = 8;

impl std::fmt::Debug for ShareLinkOptions {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("ShareLinkOptions")
      .field("maybe_expires_in_seconds", &self.maybe_expires_in_seconds)
      .field("maybe_max_downloads", &self.maybe_max_downloads)
      .field("has_password", &self.maybe_password.is_some())
      .finish()
  }
}

/// `Debug` hides the URL, whose fragment is the file key.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareLink {
  pub file_id: String,
  /// The link's ID on the server, to revoke it.
  #[serde(default)]
  pub share_id: String,
  /// Downloads so far, as of the last refresh.
  #[serde(default)]
  pub download_count: u32,
  /// The full link including the `#` fragment with the key. Show it only to the creator.
  pub url: String,
  #[serde(default)]
  pub maybe_expires_at: Option<TimestampMs>,
  #[serde(default)]
  pub maybe_max_downloads: Option<u32>,
  pub has_password: bool,
}

impl std::fmt::Debug for ShareLink {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("ShareLink")
      .field("file_id", &self.file_id)
      .field("has_password", &self.has_password)
      .finish_non_exhaustive()
  }
}

/// A conversation reached by a permalink.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermalinkTarget {
  pub conversation: ConversationId,
  pub message: MessageId,
  #[serde(default)]
  pub maybe_thread: Option<ThreadId>,
}

/// The message a permalink points to, as the viewer's own client sees it: the embedded preview under
/// a message that links to another one. Each viewer resolves it from their own decrypted store, so
/// the sender never embeds the linked text and people outside the source conversation learn nothing
/// about it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LinkedMessage {
  /// The viewer can read the message.
  Visible {
    target: PermalinkTarget,
    message: Message,
    /// Its attachments, for the chips of the preview.
    #[serde(default)]
    files: Vec<FileInfo>,
  },
  /// The viewer is a member, but the message was deleted.
  Deleted { target: PermalinkTarget },
  /// The viewer is a member, but this device doesn't have the message (not synced yet, or purged).
  NotLoaded { target: PermalinkTarget },
  /// Not a member, a message this device doesn't have, or no message link at all. Says nothing
  /// about whether the message exists.
  #[serde(other)]
  Unavailable,
}

/// A new space.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewSpace {
  pub name: String,
  #[serde(default)]
  pub maybe_description: Option<String>,
  /// Anyone in the workspace may join (otherwise by invitation).
  pub is_open: bool,
  /// Generated once per user action; a retry of the queued action reuses it so the server creates
  /// the space only once.
  #[serde(default)]
  pub maybe_idempotency_key: Option<IdempotencyKey>,
}

/// Changes to a space; absent fields stay.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceChanges {
  #[serde(default)]
  pub maybe_name: Option<String>,
  #[serde(default)]
  pub maybe_description: Option<String>,
  /// `#rrggbb`; an empty string clears the color.
  #[serde(default)]
  pub maybe_color: Option<String>,
  #[serde(default)]
  pub maybe_is_open: Option<bool>,
  #[serde(default)]
  pub maybe_is_archived: Option<bool>,
  /// The id of a configured brand; an empty string clears the tag.
  #[serde(default)]
  pub maybe_brand_id: Option<String>,
}

/// A new channel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewChannel {
  pub space: SpaceId,
  pub name: String,
  pub is_private: bool,
  #[serde(default)]
  pub maybe_topic: Option<String>,
  #[serde(default)]
  pub maybe_category: Option<CategoryId>,
  /// What people added later see (default: the workspace policy's).
  #[serde(default)]
  pub maybe_history: Option<HistoryPolicy>,
  /// A log channel for bots and machine output (docs/bridge.md); such a channel is private.
  #[serde(default)]
  pub is_integration: bool,
  /// Generated once per user action; a retry of the queued action reuses it so the server creates
  /// the channel only once.
  #[serde(default)]
  pub maybe_idempotency_key: Option<IdempotencyKey>,
}

/// Changes to a channel; absent fields stay.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelChanges {
  #[serde(default)]
  pub maybe_name: Option<String>,
  #[serde(default)]
  pub maybe_topic: Option<String>,
  #[serde(default)]
  pub maybe_description: Option<String>,
  /// Converts public ↔ private.
  #[serde(default)]
  pub maybe_is_private: Option<bool>,
  #[serde(default)]
  pub maybe_is_archived: Option<bool>,
  /// Switches the channel between normal and log mode.
  #[serde(default)]
  pub maybe_is_integration: Option<bool>,
  #[serde(default)]
  pub maybe_category: Option<CategoryChoice>,
  /// Moves the channel to another space (admin of both spaces, and every channel member already in
  /// the new space); the channel leaves its category,
  /// unless `maybe_category` names one of the new space.
  #[serde(default)]
  pub maybe_space: Option<SpaceId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CategoryChoice {
  Category { category: CategoryId },
  Uncategorized,
}

/// A channel the user may see in the channel browser.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowsableChannel {
  pub conversation: ConversationId,
  pub space: SpaceId,
  pub name: String,
  pub topic: String,
  pub is_member: bool,
  pub is_archived: bool,
  /// Contains people from outside the organization (guests).
  pub has_external_people: bool,
  /// The category the channel is listed under in its space.
  #[serde(default)]
  pub maybe_category: Option<CategoryId>,
  /// How the user gets in: public channels are joined, private ones only by workspace admins as
  /// the signed policy allows (admin.md §2.4).
  #[serde(default)]
  pub join: BrowseJoin,
}

/// How the user can join a channel from the browser.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowseJoin {
  /// A private channel a workspace admin joins by themselves (`admin_self_join: visible`); the
  /// members see that an admin joined.
  AsAdmin,
  /// A private channel whose owners have to approve an admin (`admin_self_join: owner_approval`).
  NeedsApproval,
  /// Like [`Self::NeedsApproval`], and the admin asked already: the owners decide.
  Requested,
  /// A public channel: anyone in the space joins.
  #[default]
  #[serde(other)]
  Open,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryInfo {
  pub id: CategoryId,
  pub space: SpaceId,
  pub name: String,
  pub position: i32,
}

/// Invites a person by email to a space, as member or guest, optionally into some channels.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceInvite {
  pub space: SpaceId,
  pub email: String,
  pub is_guest: bool,
  #[serde(default)]
  pub channels: Vec<ConversationId>,
  #[serde(default)]
  pub maybe_expires_at: Option<TimestampMs>,
  /// A personal note for the invitation e-mail.
  #[serde(default)]
  pub maybe_message: Option<String>,
}

/// Workspace events shown as system messages.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum WorkspaceEvent {
  MemberAdded {
    user: UserId,
    by: UserId,
    history: HistoryPolicy,
  },
  MemberRemoved {
    user: UserId,
    by: UserId,
  },
  // The next five are changes the server records of the channel (issue #818): the server's word.
  // They name who made the change (`by`) only when that member's own encrypted announcement
  // matched it, or the user made it on this device; otherwise `by` is absent ("This channel was
  // archived").
  Archived {
    #[serde(rename = "by", default, skip_serializing_if = "Option::is_none")]
    maybe_by: Option<UserId>,
  },
  Unarchived {
    #[serde(rename = "by", default, skip_serializing_if = "Option::is_none")]
    maybe_by: Option<UserId>,
  },
  MadePrivate {
    #[serde(rename = "by", default, skip_serializing_if = "Option::is_none")]
    maybe_by: Option<UserId>,
  },
  MadePublic {
    #[serde(rename = "by", default, skip_serializing_if = "Option::is_none")]
    maybe_by: Option<UserId>,
  },
  /// The channel moved to the space with this name (empty if this device does not know it).
  Moved {
    #[serde(rename = "by", default, skip_serializing_if = "Option::is_none")]
    maybe_by: Option<UserId>,
    space: String,
  },
  Pinned {
    message: MessageId,
    by: UserId,
  },
  /// A workspace admin joined the private channel (admin.md §2.4): always shown. By themselves
  /// (`visible`, neither field), let in by a channel member under `owner_approval`
  /// (`maybe_approved_by`), or added by somebody without an approval to ask for
  /// (`maybe_added_by`); at most one of them is set.
  AdminJoined {
    user: UserId,
    #[serde(rename = "approved_by", default, skip_serializing_if = "Option::is_none")]
    maybe_approved_by: Option<UserId>,
    #[serde(rename = "added_by", default, skip_serializing_if = "Option::is_none")]
    maybe_added_by: Option<UserId>,
  },
  #[serde(other)]
  Unknown,
}

fn is_zero(value: &u32) -> bool {
  *value == 0
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::id_from_raw;

  fn media_file(message: i64, media_type: &str) -> FileInfo {
    // From JSON: only the fields every version of the record has.
    serde_json::from_value(serde_json::json!({
      "conversation": "1",
      "message": message.to_string(),
      "file_id": message.to_string(),
      "file_name": "f",
      "media_type": media_type,
      "size_bytes": 1,
      "transfer": { "state": "remote" },
    }))
    .unwrap()
  }

  /// Messages 9, 8, ..., 1 (newest first); 5 only has a document.
  fn nine_messages() -> Vec<FileInfo> {
    (1..=9)
      .rev()
      .map(|message| {
        media_file(
          message,
          if message == 5 {
            "application/pdf"
          } else {
            "image/png"
          },
        )
      })
      .collect()
  }

  fn messages_of(page: &MediaPage) -> Vec<i64> {
    page
      .files
      .iter()
      .map(|file| file.message.to_string().parse().unwrap())
      .collect()
  }

  #[test]
  fn media_pages_start_at_the_newest_or_around_a_message() {
    let newest = MediaPage::from_files(nine_messages(), MediaAnchor::Newest, 3);
    assert_eq!(messages_of(&newest), [9, 8, 7]);
    assert!(newest.has_older && !newest.has_newer);
    let around = MediaPage::from_files(nine_messages(), MediaAnchor::Around(id_from_raw(6)), 4);
    assert_eq!(messages_of(&around), [8, 7, 6, 4], "documents don't count");
    assert!(around.has_older && around.has_newer);
    let gone = MediaPage::from_files(nine_messages(), MediaAnchor::Around(id_from_raw(99)), 4);
    assert_eq!(
      messages_of(&gone),
      [9, 8, 7, 6],
      "an unknown message gets the newest page"
    );
  }

  #[test]
  fn media_pages_walk_outwards_in_both_directions() {
    let older = MediaPage::from_files(nine_messages(), MediaAnchor::Older(id_from_raw(7)), 3);
    assert_eq!(messages_of(&older), [6, 4, 3]);
    assert!(older.has_older);
    let last = MediaPage::from_files(nine_messages(), MediaAnchor::Older(id_from_raw(2)), 3);
    assert_eq!(messages_of(&last), [1]);
    assert!(!last.has_older);
    let newer = MediaPage::from_files(nine_messages(), MediaAnchor::Newer(id_from_raw(3)), 2);
    assert_eq!(
      messages_of(&newer),
      [6, 4],
      "the nearest ones, still newest first"
    );
    assert!(newer.has_newer);
    let end = MediaPage::from_files(nine_messages(), MediaAnchor::Newer(id_from_raw(9)), 5);
    assert_eq!(messages_of(&end), [] as [i64; 0]);
    assert!(
      MediaPage::from_files(nine_messages(), MediaAnchor::Older(id_from_raw(99)), 3)
        .files
        .is_empty()
    );
  }

  #[test]
  fn media_anchors_are_stable_on_the_wire() {
    assert_eq!(
      serde_json::to_string(&MediaAnchor::Newest).unwrap(),
      r#"{"kind":"newest"}"#
    );
    assert_eq!(
      serde_json::to_string(&MediaAnchor::Older(id_from_raw(7))).unwrap(),
      r#"{"kind":"older","message":"7"}"#
    );
  }

  #[test]
  fn queued_channel_actions_from_before_log_channels_still_parse() {
    let channel: NewChannel =
      serde_json::from_str(r#"{"space":"1","name":"general","is_private":false}"#).expect("parse");
    assert!(!channel.is_integration);
    let changes: ChannelChanges = serde_json::from_str("{}").expect("parse");
    assert_eq!(changes.maybe_is_integration, None);
  }

  #[test]
  fn outgoing_messages_default_to_plain_ones() {
    let parsed: OutgoingMessage = serde_json::from_str(r#"{"conversation":"7","body":"hi"}"#).expect("parse");
    assert_eq!(parsed.priority, MessagePriority::Normal);
    assert!(!parsed.is_silent && !parsed.wants_acknowledgement && parsed.files.is_empty());
    assert_eq!(parsed.conversation, id_from_raw(7));
  }

  #[test]
  fn read_state_and_workspace_events_keep_the_names_the_mobile_apps_decode() {
    let mut extras = MessageExtras::new(id_from_raw(9));
    extras.is_read = true;
    assert_eq!(serde_json::to_value(&extras).expect("serialize")["is_read"], true);
    assert_eq!(
      serde_json::to_value(WorkspaceEvent::MemberAdded {
        user: id_from_raw(2),
        by: id_from_raw(1),
        history: HistoryPolicy::Full,
      })
      .expect("serialize"),
      serde_json::json!({ "event": "member_added", "user": "2", "by": "1", "history": "full" })
    );
    assert_eq!(
      serde_json::to_value(WorkspaceEvent::Moved {
        maybe_by: Some(id_from_raw(1)),
        space: "Project A".into(),
      })
      .expect("serialize"),
      serde_json::json!({ "event": "moved", "by": "1", "space": "Project A" })
    );
    // A change nobody is named for (issue #818) leaves `by` out; the apps word it without a name.
    assert_eq!(
      serde_json::to_value(WorkspaceEvent::Archived { maybe_by: None }).expect("serialize"),
      serde_json::json!({ "event": "archived" })
    );
    assert_eq!(
      serde_json::from_value::<WorkspaceEvent>(serde_json::json!({ "event": "made_private", "by": "4" }))
        .expect("parse"),
      WorkspaceEvent::MadePrivate {
        maybe_by: Some(id_from_raw(4))
      }
    );
  }

  #[test]
  fn tagged_enums_are_stable_and_tolerate_new_events() {
    assert_eq!(
      serde_json::to_string(&FileTransfer::Downloading {
        done_bytes: 1,
        total_bytes: 2
      })
      .expect("serialize"),
      r#"{"state":"downloading","done_bytes":1,"total_bytes":2}"#
    );
    assert_eq!(
      serde_json::from_str::<WorkspaceEvent>(r#"{"event":"moved_to_space"}"#).expect("parse"),
      WorkspaceEvent::Unknown
    );
  }

  #[test]
  fn linked_messages_are_tagged_and_tolerate_new_states() {
    let target = PermalinkTarget {
      conversation: id_from_raw(7),
      message: id_from_raw(8),
      maybe_thread: None,
    };
    assert_eq!(
      serde_json::to_string(&LinkedMessage::Deleted { target }).expect("serialize"),
      r#"{"state":"deleted","target":{"conversation":"7","message":"8","maybe_thread":null}}"#
    );
    assert_eq!(
      serde_json::from_str::<LinkedMessage>(r#"{"state":"unavailable"}"#).expect("parse"),
      LinkedMessage::Unavailable
    );
    assert_eq!(
      serde_json::from_str::<LinkedMessage>(r#"{"state":"moved"}"#).expect("parse"),
      LinkedMessage::Unavailable
    );
  }

  #[test]
  fn an_upload_without_failed_attempts_keeps_its_wire_form() {
    let fresh = FileTransfer::Uploading {
      done_bytes: 1,
      total_bytes: 2,
      failed_attempts: 0,
    };
    let json = r#"{"state":"uploading","done_bytes":1,"total_bytes":2}"#;
    assert_eq!(serde_json::to_string(&fresh).expect("serialize"), json);
    assert_eq!(serde_json::from_str::<FileTransfer>(json).expect("parse"), fresh);
    let retrying = FileTransfer::Uploading {
      done_bytes: 0,
      total_bytes: 2,
      failed_attempts: 3,
    };
    assert_eq!(
      serde_json::to_string(&retrying).expect("serialize"),
      r#"{"state":"uploading","done_bytes":0,"total_bytes":2,"failed_attempts":3}"#
    );
  }
}
