//! The archive's layout and records, version [`FORMAT_VERSION`] (docs/data-export.md is the
//! reference for people and tools reading it). Records are written once and never read back by the
//! app, so they only serialize. A change that alters a field's meaning raises the version; new
//! fields don't.

use serde::Serialize;
use teamcraft_client_api::data_export::DataExportScope;
use teamcraft_proto::{AttachmentId, ClientMessageId, ConversationId, MessageId, ThreadId, UserId};

use crate::account_data::AccountData;
use crate::store::{MessageContent, Priority, StoredMessage, Unreadable};

/// What a reader checks first in `manifest.json`.
pub const FORMAT: &str = "teamcraft-data-export";
pub const FORMAT_VERSION: u32 = 1;
/// Every path of the archive is below this folder, so unpacking it makes one folder.
pub const ROOT: &str = "teamcraft-export";

/// The paths of the archive's fixed files.
pub fn personal_data_path() -> String {
  format!("{ROOT}/account/personal-data.json")
}

pub fn account_data_path() -> String {
  format!("{ROOT}/account/account-data.json")
}

pub fn security_log_path() -> String {
  format!("{ROOT}/account/security-log.ndjson")
}

pub fn conversations_path() -> String {
  format!("{ROOT}/conversations.json")
}

pub fn index_path() -> String {
  format!("{ROOT}/index.html")
}

pub fn manifest_path() -> String {
  format!("{ROOT}/manifest.json")
}

/// One part of a conversation's messages, numbered from 1.
pub fn messages_path(conversation: ConversationId, part: u32) -> String {
  format!("{ROOT}/conversations/{conversation}/messages-{part:04}.ndjson")
}

/// A file the user sent, under its conversation and ID, named as it was sent (made safe).
pub fn file_path(conversation: ConversationId, attachment: AttachmentId, file_name: &str) -> String {
  format!(
    "{ROOT}/files/{conversation}/{attachment}/{}",
    safe_file_name(file_name)
  )
}

/// `name` as one path segment every file system takes: no separators, reserved or control
/// characters, no leading or trailing dots and spaces, at most 120 bytes.
pub fn safe_file_name(name: &str) -> String {
  const MAX_BYTES: usize = 120;
  let replaced: String = name
    .chars()
    .map(|character| match character {
      '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
      character if character.is_control() => '_',
      character => character,
    })
    .collect();
  let trimmed = replaced.trim_matches(|character: char| character == '.' || character == ' ');
  let mut end = trimmed.len().min(MAX_BYTES);
  while !trimmed.is_char_boundary(end) {
    end -= 1;
  }
  let cut = &trimmed[..end];
  if cut.is_empty() {
    "file".to_owned()
  } else {
    cut.to_owned()
  }
}

/// `manifest.json`: written last, it lists every other file with its size and checksum.
#[derive(Serialize)]
pub struct Manifest<'a> {
  pub format: &'static str,
  pub version: u32,
  pub created_at_ms: i64,
  pub user_id: UserId,
  pub scope: DataExportScope,
  pub includes_files: bool,
  /// The largest file that goes into the archive; bigger ones are listed in `missing_files`.
  pub max_file_bytes: u64,
  pub counts: Counts,
  pub files: Vec<ManifestFile<'a>>,
  /// Files a message record names a path for that couldn't be written, and files left out.
  pub missing_files: &'a [MissingFile],
  /// What the archive can't hold and why, for the reader (see docs/data-export.md).
  pub not_included: &'static [&'static str],
}

/// The totals in the manifest.
#[derive(Clone, Copy, Debug, Default, Serialize, serde::Deserialize)]
pub struct Counts {
  pub conversations: u32,
  pub messages: u64,
  pub unreadable_messages: u64,
  pub files: u32,
  pub files_missing: u32,
}

#[derive(Serialize)]
pub struct ManifestFile<'a> {
  pub path: &'a str,
  pub size: u64,
  pub sha256: &'a str,
}

/// A file that a message names but the archive doesn't hold.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct MissingFile {
  pub conversation_id: ConversationId,
  pub attachment_id: AttachmentId,
  pub file_name: String,
  pub reason: MissingReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissingReason {
  /// Larger than [`Manifest::max_file_bytes`]; save it from the app.
  TooLarge,
  /// The server no longer has it (deleted, or past the workspace's retention).
  Gone,
  /// It didn't match the hash its message announced, or didn't decrypt.
  Damaged,
}

/// Reasons the archive can hold less than the user may expect; listed in every manifest.
pub const NOT_INCLUDED: &[&str] = &[
  "messages_deleted_or_expired: deleted messages and those past the workspace's retention are not on the server any more and are not restored",
  "unreadable_messages: messages this device has no key for are listed with their metadata only (kind \"unreadable\")",
  "other_peoples_files: only files the user sent are included; other people's files are listed by name and size",
  "encryption_keys: no key of the user's devices, conversations or files is ever written",
  "audit_actions_by_the_user: what an admin did to others is in the instance's audit log, not in this export",
];

/// `account/account-data.json`: what the user's devices keep in the encrypted key bag (e2ee.md
/// §6.4), without the keys and trust records in it.
#[derive(Serialize)]
pub struct AccountDataRecord<'a> {
  /// Synced settings by name, with their values as the apps wrote them.
  pub settings: &'a teamcraft_proto::synced_settings::StoredSettings,
  pub saved_messages: Vec<SavedRecord>,
  pub drafts: Vec<DraftRecord<'a>>,
  pub saved_searches: Vec<SavedSearchRecord<'a>>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub recovery_passphrase_set_at_ms: Option<i64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub recovery_kit_made_at_ms: Option<i64>,
}

#[derive(Serialize)]
pub struct SavedRecord {
  pub conversation_id: ConversationId,
  pub client_message_id: ClientMessageId,
  pub saved_at_ms: i64,
}

#[derive(Serialize)]
pub struct DraftRecord<'a> {
  pub conversation_id: ConversationId,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub thread_id: Option<ThreadId>,
  pub text: &'a str,
  pub edited_at_ms: i64,
}

#[derive(Serialize)]
pub struct SavedSearchRecord<'a> {
  pub name: &'a str,
  pub query: &'a str,
}

impl<'a> AccountDataRecord<'a> {
  pub fn of(data: &'a AccountData) -> Self {
    let saved_messages = data
      .saved
      .values()
      .filter(|entry| !entry.is_removed)
      .map(|entry| SavedRecord {
        conversation_id: entry.value.conversation_id,
        client_message_id: entry.value.client_message_id,
        saved_at_ms: entry.updated_at_ms,
      })
      .collect();
    let drafts = data
      .drafts
      .values()
      .filter(|entry| !entry.is_removed && !entry.value.body.trim().is_empty())
      .map(|entry| DraftRecord {
        conversation_id: entry.value.conversation_id,
        thread_id: entry.value.maybe_thread_id,
        text: &entry.value.body,
        edited_at_ms: entry.updated_at_ms,
      })
      .collect();
    let saved_searches = data
      .saved_searches
      .values()
      .filter(|entry| !entry.is_removed)
      .map(|entry| SavedSearchRecord {
        name: &entry.value.name,
        query: &entry.value.query,
      })
      .collect();
    Self {
      settings: &data.settings,
      saved_messages,
      drafts,
      saved_searches,
      recovery_passphrase_set_at_ms: data.maybe_recovery_passphrase_set_at,
      recovery_kit_made_at_ms: data.maybe_recovery_key_set_at,
    }
  }
}

/// One entry of `conversations.json`, and of the index page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct ConversationSummary {
  pub id: ConversationId,
  /// `channel`, `dm`, `group_dm` and so on, as the server names the kind.
  pub kind: String,
  pub name: String,
  /// The paths of its message parts, in order.
  pub parts: Vec<String>,
  pub messages: u64,
  pub files: u32,
  /// The server let this device read only part of the history (joined later with history from
  /// joining on, or the history was refused).
  pub is_history_limited: bool,
}

/// One line of a `messages-NNNN.ndjson` part: a message, a thread reply or an event, in the
/// conversation's order (replies follow the message they answer).
#[derive(Serialize)]
pub struct MessageRecord<'a> {
  /// The server's ID; absent on lines this device derived.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub id: Option<MessageId>,
  pub client_message_id: ClientMessageId,
  /// The thread a reply belongs to (the ID of the message it answers).
  #[serde(skip_serializing_if = "Option::is_none")]
  pub thread_id: Option<ThreadId>,
  pub sender_id: UserId,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub sender_name: Option<&'a str>,
  pub is_own: bool,
  pub sent_at_ms: i64,
  pub kind: RecordKind,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub text: Option<&'a str>,
  #[serde(skip_serializing_if = "std::ops::Not::not")]
  pub is_edited: bool,
  /// Earlier versions of an edited text, oldest first.
  #[serde(skip_serializing_if = "Vec::is_empty")]
  pub earlier_versions: Vec<EarlierVersion<'a>>,
  #[serde(skip_serializing_if = "<[UserId]>::is_empty")]
  pub mentions: &'a [UserId],
  #[serde(skip_serializing_if = "Option::is_none")]
  pub priority: Option<Priority>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub reply_to: Option<ReplyRecord<'a>>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub forwarded_from: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub link: Option<&'a str>,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  pub files: Vec<FileRecord<'a>>,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  pub reactions: Vec<ReactionRecord>,
  #[serde(skip_serializing_if = "std::ops::Not::not")]
  pub is_pinned: bool,
  /// What an event says (who joined, what was renamed), as the app keeps it.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub event: Option<serde_json::Value>,
  /// Why the text is missing on an `unreadable` record.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub unreadable: Option<Unreadable>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
  Text,
  File,
  Event,
  /// This device has no key for it: only the server's metadata is known.
  Unreadable,
  /// A kind of a newer app, or one that has no text (a poll, a canvas edit).
  Other,
}

#[derive(Serialize)]
pub struct EarlierVersion<'a> {
  pub text: &'a str,
  pub replaced_at_ms: i64,
}

#[derive(Serialize)]
pub struct ReplyRecord<'a> {
  pub client_message_id: ClientMessageId,
  pub author_id: UserId,
  pub excerpt: &'a str,
}

#[derive(Serialize)]
pub struct FileRecord<'a> {
  pub attachment_id: AttachmentId,
  pub name: &'a str,
  pub media_type: &'a str,
  pub size_bytes: u64,
  /// Where the file is in the archive; absent when it isn't (another person's file, too large,
  /// files not chosen). A path listed in the manifest's `missing_files` didn't make it.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub path: Option<String>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct ReactionRecord {
  pub emoji: String,
  pub user_ids: Vec<UserId>,
}

/// The kind and text of a stored message.
pub fn kind_and_text(message: &StoredMessage) -> (RecordKind, Option<&str>) {
  if message.maybe_unreadable.is_some() {
    return (RecordKind::Unreadable, None);
  }
  match &message.content {
    MessageContent::Text { body, .. } => (RecordKind::Text, Some(body.as_str())),
    MessageContent::File { .. } => (RecordKind::File, None),
    MessageContent::Event { .. } => (RecordKind::Event, None),
    _ if message.is_local_line() => (RecordKind::Event, None),
    _ => (RecordKind::Other, None),
  }
}

/// Whether the scope takes `message` of `sender`.
pub fn is_in_scope(scope: DataExportScope, message: &StoredMessage, me: UserId) -> bool {
  match scope {
    DataExportScope::All => true,
    DataExportScope::Own => {
      message.sender_user_id == me && !matches!(kind_and_text(message).0, RecordKind::Event)
    },
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn file_names_become_one_safe_path_segment() {
    assert_eq!(safe_file_name("report.pdf"), "report.pdf");
    assert_eq!(safe_file_name("../../etc/passwd"), "_.._etc_passwd");
    assert_eq!(safe_file_name(".."), "file");
    assert_eq!(safe_file_name("a:b*c?.txt"), "a_b_c_.txt");
    assert_eq!(safe_file_name("line\nbreak"), "line_break");
    assert_eq!(safe_file_name(""), "file");
    let long = "ä".repeat(100);
    let cut = safe_file_name(&long);
    assert!(cut.len() <= 120 && cut.chars().all(|character| character == 'ä'));
  }

  #[test]
  fn every_path_is_below_the_root_folder() {
    let conversation: ConversationId =
      teamcraft_proto::Id::from_snowflake(teamcraft_proto::Snowflake::new(7).unwrap());
    for path in [
      personal_data_path(),
      account_data_path(),
      security_log_path(),
      conversations_path(),
      index_path(),
      manifest_path(),
      messages_path(conversation, 3),
    ] {
      assert!(path.starts_with("teamcraft-export/"), "{path}");
    }
    assert!(messages_path(conversation, 3).ends_with("/messages-0003.ndjson"));
  }
}
