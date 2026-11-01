//! History imported from another chat system (e2ee.md §4.10, issue #402): what an imported
//! message says about its origin, inside the ciphertext, and when a client believes it.
//!
//! An imported message is an ordinary message of the import account's device. Its author and time
//! are that account's statement, so they are shown only when the sender is the import account the
//! verified workspace policy names; from anybody else [`MessageMeta::imported`] is ignored, so a
//! member can't post in somebody else's name.
//!
//! [`MessageMeta::imported`]: crate::store::MessageMeta::imported

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use teamcraft_proto::{ClientMessageId, ConversationId, ImportPolicy, UserId};

use crate::convert;
use crate::records;
use crate::store::{ConversationEvent, LocalStore, MessageContent, StoredMessage, Table};
use crate::workspace::WorkspaceState;

/// Where an imported message came from and whom it is by.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedOrigin {
  pub source: ImportSource,
  /// The message's ID in the old system, opaque.
  pub source_id: String,
  pub author: ImportedAuthor,
  /// The original time, Unix milliseconds.
  pub sent_at_ms: i64,
  #[serde(rename = "edited_at_ms", default, skip_serializing_if = "Option::is_none")]
  pub maybe_edited_at_ms: Option<i64>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub reactions: Vec<ImportedReaction>,
  /// Names of files the export didn't have.
  #[serde(rename = "missing", default, skip_serializing_if = "Vec::is_empty")]
  pub missing_files: Vec<String>,
}

/// The system an import came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportSource {
  Synology,
  /// From a newer client; shown without a name.
  #[serde(other)]
  Unknown,
}

impl ImportSource {
  /// The stable name used in derived message IDs.
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Synology => "synology",
      Self::Unknown => "unknown",
    }
  }
}

/// Who wrote an imported message, or reacted to it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ImportedAuthor {
  /// A TeamCraft account the import's mapping names.
  User { user_id: UserId },
  /// Somebody the mapping doesn't name (left, deleted, never had an account): shown like a deleted
  /// account (admin.md §3.2.1).
  Deleted,
  /// A bot or webhook of the old system, by its name there.
  Named { name: String },
  /// From a newer client; shown like [`Self::Deleted`].
  #[serde(other)]
  Unknown,
}

/// One emoji and who reacted with it in the old system.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedReaction {
  pub emoji: String,
  pub authors: Vec<ImportedAuthor>,
}

/// The import account of the verified workspace policy, if it names one (also after the import
/// period: its messages keep their origin). `None` before a workspace was verified.
pub fn importer(store: &impl LocalStore) -> Option<UserId> {
  import_policy(store).and_then(|policy| policy.maybe_importer)
}

/// The import group of the verified workspace policy.
pub fn import_policy(store: &impl LocalStore) -> Option<ImportPolicy> {
  records::get_json::<WorkspaceState>(store, Table::Workspace, crate::client::WORKSPACE_KEY)
    .ok()
    .flatten()
    .map(|state| state.policy.import)
}

/// The origin `message` claims, if its sender is `maybe_importer` (see the module docs).
pub fn trusted_origin(message: &StoredMessage, maybe_importer: Option<UserId>) -> Option<&ImportedOrigin> {
  if maybe_importer != Some(message.sender_user_id) {
    return None;
  }
  match &message.content {
    MessageContent::Text { meta, .. } => meta.maybe_imported.as_deref(),
    _ => None,
  }
}

/// Who a message is shown as written by, and when: for an imported message of the import account
/// its original author and time (e2ee.md §4.10), otherwise its sender and the time it was sent.
/// Every view that names a message's author or time (the list, the lightbox, files, search,
/// activity) takes them from here, never from the sender or the message ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Authorship<'a> {
  /// The account shown as the author: the original author when the import maps them to one, the
  /// sender otherwise (also for imported authors without an account, see `maybe_origin`).
  pub author: UserId,
  pub sent_at_ms: i64,
  /// The trusted origin of an imported message.
  pub maybe_origin: Option<&'a ImportedOrigin>,
}

impl<'a> Authorship<'a> {
  pub fn of(message: &'a StoredMessage, maybe_importer: Option<UserId>) -> Self {
    let maybe_origin = trusted_origin(message, maybe_importer);
    let author = match maybe_origin.map(|origin| &origin.author) {
      Some(ImportedAuthor::User { user_id }) => *user_id,
      _ => message.sender_user_id,
    };
    Self {
      author,
      sent_at_ms: maybe_origin.map_or(message.sent_at_ms, |origin| origin.sent_at_ms),
      maybe_origin,
    }
  }

  /// The author as an account, `None` for an imported author without one (a deleted account, a
  /// bot of the old system): those are not the import account.
  pub fn maybe_account(&self) -> Option<UserId> {
    match self.maybe_origin.map(|origin| &origin.author) {
      None | Some(ImportedAuthor::User { .. }) => Some(self.author),
      Some(ImportedAuthor::Deleted | ImportedAuthor::Named { .. } | ImportedAuthor::Unknown) => None,
    }
  }
}

/// Whether `message` is the import account `maybe_importer` telling of a change it made, like
/// adding people to what it imported: an event message or a line derived from its commit.
pub fn is_import_line(message: &StoredMessage, maybe_importer: Option<UserId>) -> bool {
  maybe_importer == Some(message.sender_user_id)
    && (message.line_position().is_some() || matches!(message.content, MessageContent::Event { .. }))
}

/// Whether `message` is a line of the import account that no client shows: the people it added
/// (workspace admins too), the changes it announced (archiving a channel) and its leave. Only its
/// own joining shows, as a workspace admin above all: that is how the members of a channel merged
/// into learn that the import account can read it (e2ee.md §4.10, issue #1079).
pub fn is_quiet_import_line(message: &StoredMessage, maybe_importer: Option<UserId>) -> bool {
  is_import_line(message, maybe_importer) && !is_own_join(message)
}

/// Whether `message` is its sender telling that they joined.
fn is_own_join(message: &StoredMessage) -> bool {
  match message.content {
    MessageContent::Event {
      event: ConversationEvent::Joined,
    } => true,
    MessageContent::Event {
      event: ConversationEvent::AdminJoined { user, .. } | ConversationEvent::MemberAdded { user, .. },
    } => user == message.sender_user_id,
    _ => false,
  }
}

/// The import account's lines ([`is_quiet_import_line`]), the import account adding people to
/// what it imported, and other members letting those people in (or they saying they joined) once
/// they set up the app: not shown, the people of an imported conversation were in it in the old
/// system (e2ee.md §4.10).
pub struct ImportAdditions {
  maybe_importer: Option<UserId>,
  added: BTreeSet<UserId>,
}

impl ImportAdditions {
  /// The people `maybe_importer` added among a conversation's messages `all`.
  pub fn of(all: &[StoredMessage], maybe_importer: Option<UserId>) -> Self {
    let added = all
      .iter()
      .filter_map(|message| imported_addition(message, maybe_importer))
      .collect();
    Self {
      maybe_importer,
      added,
    }
  }

  /// None noted yet, for a pass over a conversation in order ([`Self::note`]).
  pub fn new(maybe_importer: Option<UserId>) -> Self {
    Self {
      maybe_importer,
      added: BTreeSet::new(),
    }
  }

  /// [`Self::hides`] for a pass in conversation order: the import account adds people before
  /// anybody lets them in again.
  pub fn note(&mut self, message: &StoredMessage) -> bool {
    if let Some(user) = imported_addition(message, self.maybe_importer) {
      self.added.insert(user);
    }
    self.hides(message)
  }

  /// Whether `message` is a quiet line of the import account, adds one of them, or is one of them
  /// saying they joined.
  pub fn hides(&self, message: &StoredMessage) -> bool {
    if is_quiet_import_line(message, self.maybe_importer) {
      return true;
    }
    let subject = match message.content {
      MessageContent::Event {
        event: ConversationEvent::MemberAdded { user, .. },
      } => user,
      MessageContent::Event {
        event: ConversationEvent::Joined,
      } => message.sender_user_id,
      _ => return false,
    };
    self.added.contains(&subject) && Some(subject) != self.maybe_importer
  }
}

/// The person `message` says the import account `maybe_importer` added, if it does.
fn imported_addition(message: &StoredMessage, maybe_importer: Option<UserId>) -> Option<UserId> {
  if maybe_importer != Some(message.sender_user_id) {
    return None;
  }
  match message.content {
    MessageContent::Event {
      event: ConversationEvent::MemberAdded { user, .. },
    } if Some(user) != maybe_importer => Some(user),
    _ => None,
  }
}

/// The client message ID of an imported message (e2ee.md §4.10): the same for every run of the
/// import, so the server keeps the first.
pub fn imported_message_id(
  conversation: ConversationId,
  source: ImportSource,
  source_id: &str,
) -> ClientMessageId {
  let derived = teamcraft_crypto::ids::ClientMessageId::imported(
    convert::conversation(conversation),
    source.as_str(),
    source_id,
  );
  ClientMessageId::from_bytes(*derived.as_bytes())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::store::MessageMeta;

  const IMPORTER: &str = "7";

  #[test]
  fn the_origin_counts_only_from_the_import_account() {
    let importer: UserId = IMPORTER.parse().unwrap();
    let message = imported_by(importer);
    assert!(trusted_origin(&message, Some(importer)).is_some());
    assert!(trusted_origin(&message, None).is_none());
    let forged = imported_by("8".parse().unwrap());
    assert!(trusted_origin(&forged, Some(importer)).is_none());
  }

  #[test]
  fn only_the_import_account_adding_somebody_else_is_left_out() {
    let importer: UserId = IMPORTER.parse().unwrap();
    let other: UserId = "8".parse().unwrap();
    let adding = |sender: UserId, user: UserId| StoredMessage {
      sender_user_id: sender,
      content: MessageContent::Event {
        event: ConversationEvent::MemberAdded {
          user,
          history: teamcraft_proto::HistoryPolicy::Full,
        },
      },
      ..imported_by(sender)
    };
    let by_import = adding(importer, other);
    let additions = ImportAdditions::of(std::slice::from_ref(&by_import), Some(importer));
    let mut in_order = ImportAdditions::new(Some(importer));
    assert!(
      !in_order.note(&adding("1".parse().unwrap(), other)),
      "before the import's"
    );
    assert!(in_order.note(&by_import));
    assert!(additions.hides(&by_import));
    assert!(!additions.hides(&adding(other, importer)));
    assert!(!additions.hides(&adding(importer, importer)), "its own joining");
    assert!(!additions.hides(&imported_by(importer)));
    assert!(!ImportAdditions::of(std::slice::from_ref(&by_import), None).hides(&by_import));
    // Another member letting that person's device in: left out too; adding anybody else not.
    let member: UserId = "1".parse().unwrap();
    assert!(additions.hides(&adding(member, other)));
    assert!(!additions.hides(&adding(member, "9".parse().unwrap())));
    let joined = |sender: UserId| StoredMessage {
      content: MessageContent::Event {
        event: ConversationEvent::Joined,
      },
      ..adding(sender, sender)
    };
    assert!(additions.hides(&joined(other)));
    assert!(!additions.hides(&joined(member)));
  }

  /// Issue #1079: a workspace admin the import account added, the channel it archived and its
  /// leave show no line; its own joining does, and so do other people's lines.
  #[test]
  fn the_import_account_s_lines_are_quiet_except_its_own_joining() {
    let importer: UserId = IMPORTER.parse().unwrap();
    let admin: UserId = "8".parse().unwrap();
    let member: UserId = "1".parse().unwrap();
    let event = |sender: UserId, event: ConversationEvent| StoredMessage {
      content: MessageContent::Event { event },
      ..imported_by(sender)
    };
    let admin_line = |sender: UserId, user: UserId| StoredMessage {
      maybe_verified_commit: Some("90".parse().unwrap()),
      ..event(
        sender,
        ConversationEvent::AdminJoined {
          user,
          is_approved: false,
        },
      )
    };
    let archived_line = StoredMessage {
      maybe_server_change: Some(crate::store::ServerChange {
        at: "91".parse().unwrap(),
        seen_after: "90".parse().unwrap(),
        seen_until: "90".parse().unwrap(),
        actor: crate::store::ChangeActor::Announced {
          announcement: ClientMessageId::from_bytes([2; 16]),
        },
      }),
      ..event(importer, ConversationEvent::Archived)
    };
    let additions = ImportAdditions::new(Some(importer));

    for quiet in [
      admin_line(importer, admin),
      archived_line.clone(),
      event(importer, ConversationEvent::Archived),
      event(importer, ConversationEvent::Left),
    ] {
      assert!(
        is_quiet_import_line(&quiet, Some(importer)),
        "{:?}",
        quiet.content
      );
      assert!(additions.hides(&quiet), "{:?}", quiet.content);
    }
    for shown in [
      admin_line(importer, importer),
      event(importer, ConversationEvent::Joined),
      admin_line(member, admin),
      StoredMessage {
        sender_user_id: member,
        ..archived_line
      },
      imported_by(importer),
    ] {
      assert!(!additions.hides(&shown), "{:?}", shown.content);
    }
    assert!(!ImportAdditions::new(None).hides(&admin_line(importer, admin)));
  }

  #[test]
  fn the_wire_shape_is_stable_and_forward_compatible() {
    let origin = ImportedOrigin {
      source: ImportSource::Synology,
      source_id: "42".into(),
      author: ImportedAuthor::Named {
        name: "Monitor".into(),
      },
      sent_at_ms: 1_600_000_000_000,
      maybe_edited_at_ms: None,
      reactions: vec![ImportedReaction {
        emoji: "\u{1f44d}".into(),
        authors: vec![
          ImportedAuthor::User {
            user_id: IMPORTER.parse().unwrap(),
          },
          ImportedAuthor::Deleted,
        ],
      }],
      missing_files: Vec::new(),
    };
    let json = serde_json::to_value(&origin).unwrap();
    assert_eq!(
      json,
      serde_json::json!({
        "source": "synology",
        "source_id": "42",
        "author": {"kind": "named", "name": "Monitor"},
        "sent_at_ms": 1_600_000_000_000_i64,
        "reactions": [{"emoji": "\u{1f44d}", "authors": [{"kind": "user", "user_id": "7"}, {"kind": "deleted"}]}],
      })
    );
    let newer: ImportedOrigin = serde_json::from_value(serde_json::json!({
      "source": "teams", "source_id": "1", "author": {"kind": "guest_of_teams"}, "sent_at_ms": 1,
    }))
    .unwrap();
    assert_eq!(newer.source, ImportSource::Unknown);
    assert_eq!(newer.author, ImportedAuthor::Unknown);
  }

  #[test]
  fn derived_ids_are_stable_per_message() {
    let conversation: ConversationId = "5".parse().unwrap();
    assert_eq!(
      imported_message_id(conversation, ImportSource::Synology, "1"),
      imported_message_id(conversation, ImportSource::Synology, "1")
    );
    assert_ne!(
      imported_message_id(conversation, ImportSource::Synology, "1"),
      imported_message_id(conversation, ImportSource::Synology, "2")
    );
  }

  fn imported_by(sender: UserId) -> StoredMessage {
    let meta = MessageMeta {
      maybe_imported: Some(Box::new(ImportedOrigin {
        source: ImportSource::Synology,
        source_id: "1".into(),
        author: ImportedAuthor::Deleted,
        sent_at_ms: 1,
        maybe_edited_at_ms: None,
        reactions: Vec::new(),
        missing_files: Vec::new(),
      })),
      ..MessageMeta::default()
    };
    StoredMessage {
      conversation_id: "5".parse().unwrap(),
      client_message_id: ClientMessageId::from_bytes([1; 16]),
      maybe_server_id: None,
      sender_user_id: sender,
      maybe_thread_id: None,
      sent_at_ms: 1,
      content: MessageContent::Text {
        body: "hello".into(),
        mentions: Vec::new(),
        meta,
      },
      is_edited: false,
      edits: Vec::new(),
      maybe_edit_id: None,
      is_pinned: false,
      maybe_verified_commit: None,
      maybe_server_change: None,
    }
  }
}
