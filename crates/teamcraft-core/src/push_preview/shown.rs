//! What a notification reads of a message (#1452): the text, the first file, whether it was sent
//! silently, the integration source's name, whether it adds somebody, and whether it is a change to
//! the conversation rather than a message. The browser's service worker reads only this, so it
//! carries no reader of every kind of [`MessageContent`] (an AI notetaker's request, a workspace head,
//! an imported message's origin, …), which a notification never shows.
//!
//! [`ShownContent`] reads the same JSON as [`MessageContent`] (the plaintext of a message and the
//! local store's records) and comes from a [`MessageContent`] the same way
//! (`From<&MessageContent>`): the tests check both for every kind.

use serde::Deserialize;
use teamcraft_proto::{ConversationId, MessageId, SourceIdentity, UserId};

use crate::store::{AttachmentRef, ConversationEvent, MessageContent, StoredMessage};

/// A stored message as far as its notification reads it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ShownMessage {
  pub conversation_id: ConversationId,
  #[serde(rename = "server_id", default)]
  pub maybe_server_id: Option<MessageId>,
  pub sender_user_id: UserId,
  pub content: ShownContent,
}

impl From<&StoredMessage> for ShownMessage {
  fn from(message: &StoredMessage) -> Self {
    Self {
      conversation_id: message.conversation_id,
      maybe_server_id: message.maybe_server_id,
      sender_user_id: message.sender_user_id,
      content: ShownContent::from(&message.content),
    }
  }
}

/// A message's content as far as its notification reads it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(from = "ContentWire")]
pub enum ShownContent {
  Text {
    body: String,
    is_silent: bool,
    /// The first of its files: a picture sent without a caption shows as one.
    maybe_file: Option<ShownFile>,
    /// What a forward without a text of its own shows.
    maybe_forwarded_body: Option<String>,
    /// The name of the integration source that posted it (believed only from a bot account).
    maybe_source_name: Option<String>,
  },
  File(ShownFile),
  /// Somebody was added to the conversation.
  MemberAdded(UserId),
  /// A change to the conversation (an edit, a reaction, a system note, …): no message to announce.
  Change,
  /// Of a kind this version doesn't know (from a newer client).
  Unknown,
}

/// A file as its notification names it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ShownFile {
  pub file_name: String,
  pub media_type: String,
}

impl ShownContent {
  /// The integration source as [`crate::integration_sources::shown_source`] takes it.
  pub(crate) fn source(&self) -> Option<SourceIdentity> {
    let Self::Text {
      maybe_source_name: Some(name),
      ..
    } = self
    else {
      return None;
    };
    Some(SourceIdentity {
      maybe_id: None,
      name: name.clone(),
      maybe_picture_sha256: None,
      maybe_picture: None,
    })
  }
}

impl From<&AttachmentRef> for ShownFile {
  fn from(attachment: &AttachmentRef) -> Self {
    Self {
      file_name: attachment.file_name.clone(),
      media_type: attachment.media_type.clone(),
    }
  }
}

impl From<&MessageContent> for ShownContent {
  fn from(content: &MessageContent) -> Self {
    match content {
      MessageContent::Text { body, meta, .. } => Self::Text {
        body: body.clone(),
        is_silent: meta.is_silent,
        maybe_file: meta.attachments.first().map(ShownFile::from),
        maybe_forwarded_body: meta.maybe_forwarded.as_ref().map(|forwarded| forwarded.body.clone()),
        maybe_source_name: meta.maybe_source.as_ref().map(|source| source.name.clone()),
      },
      MessageContent::File { attachment } => Self::File(ShownFile::from(attachment)),
      MessageContent::Event {
        event: ConversationEvent::MemberAdded { user, .. },
      } => Self::MemberAdded(*user),
      MessageContent::Edit { .. }
      | MessageContent::Delete { .. }
      | MessageContent::Deleted
      | MessageContent::IndexSegment { .. }
      | MessageContent::Pin { .. }
      | MessageContent::Bookmarks { .. }
      // A hidden message by design (e2ee.md, "Transport: a hidden message"): an edit of the notes page.
      | MessageContent::Canvas { .. }
      | MessageContent::Event { .. }
      | MessageContent::Reaction { .. }
      // Votes and closes of a poll (e2ee.md §4.12) are hidden like reactions.
      | MessageContent::PollVote { .. }
      | MessageContent::PollClose { .. }
      | MessageContent::IntegrationSource { .. }
      | MessageContent::Notetaker { .. }
      | MessageContent::ChannelRolesEntry { .. } => Self::Change,
      MessageContent::Unknown => Self::Unknown,
    }
  }
}

/// [`MessageContent`]'s JSON with only what a notification shows. The changes are read by their
/// name alone: whatever else they carry, they stay hidden.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentWire {
  Text {
    body: String,
    #[serde(default)]
    meta: MetaWire,
  },
  File {
    attachment: ShownFile,
  },
  Event {
    event: EventWire,
  },
  Edit,
  Delete,
  Deleted,
  IndexSegment,
  Pin,
  Bookmarks,
  Canvas,
  Reaction,
  PollVote,
  PollClose,
  IntegrationSource,
  Notetaker,
  ChannelRolesEntry,
  #[serde(other)]
  Unknown,
}

/// The part of [`crate::store::MessageMeta`] a notification reads.
#[derive(Default, Deserialize)]
struct MetaWire {
  #[serde(default)]
  is_silent: bool,
  #[serde(rename = "forwarded", default)]
  maybe_forwarded: Option<ForwardedWire>,
  #[serde(default)]
  attachments: Vec<ShownFile>,
  #[serde(rename = "source", default)]
  maybe_source: Option<SourceWire>,
}

#[derive(Deserialize)]
struct ForwardedWire {
  body: String,
}

#[derive(Deserialize)]
struct SourceWire {
  name: String,
}

/// [`ConversationEvent`]: only being added is announced.
#[derive(Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum EventWire {
  MemberAdded {
    user: UserId,
  },
  #[serde(other)]
  Other,
}

impl From<ContentWire> for ShownContent {
  fn from(wire: ContentWire) -> Self {
    match wire {
      ContentWire::Text { body, meta } => Self::Text {
        body,
        is_silent: meta.is_silent,
        maybe_file: meta.attachments.into_iter().next(),
        maybe_forwarded_body: meta.maybe_forwarded.map(|forwarded| forwarded.body),
        maybe_source_name: meta.maybe_source.map(|source| source.name),
      },
      ContentWire::File { attachment } => Self::File(attachment),
      ContentWire::Event {
        event: EventWire::MemberAdded { user },
      } => Self::MemberAdded(user),
      ContentWire::Event {
        event: EventWire::Other,
      }
      | ContentWire::Edit
      | ContentWire::Delete
      | ContentWire::Deleted
      | ContentWire::IndexSegment
      | ContentWire::Pin
      | ContentWire::Bookmarks
      | ContentWire::Canvas
      | ContentWire::Reaction
      | ContentWire::PollVote
      | ContentWire::PollClose
      | ContentWire::IntegrationSource
      | ContentWire::Notetaker
      | ContentWire::ChannelRolesEntry => Self::Change,
      ContentWire::Unknown => Self::Unknown,
    }
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{
    Base64Bytes, ClientMessageId, HistoryPolicy, Id, NotetakerRequest, Snowflake, SourceChange,
  };

  use super::*;
  use crate::store::{BookmarkItem, Forwarded, MessageMeta};

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn attachment(name: &str, media_type: &str) -> AttachmentRef {
    AttachmentRef {
      attachment_id: id(7),
      key: Base64Bytes::new(vec![1; 32]),
      ciphertext_sha256: Base64Bytes::new(vec![2; 32]),
      file_name: name.into(),
      media_type: media_type.into(),
      size_bytes: 3,
      maybe_thumbnail: Some(Base64Bytes::new(vec![3; 8])),
      maybe_dimensions: None,
      maybe_duration_ms: Some(1_000),
      maybe_waveform: None,
    }
  }

  /// Which kind a content is. Doesn't compile once a kind is added: give `every_kind` a sample of
  /// it, and [`ContentWire`] its name.
  fn kind(content: &MessageContent) -> usize {
    match content {
      MessageContent::Text { .. } => 0,
      MessageContent::File { .. } => 1,
      MessageContent::Edit { .. } => 2,
      MessageContent::Delete { .. } => 3,
      MessageContent::Deleted => 4,
      MessageContent::IndexSegment { .. } => 5,
      MessageContent::Pin { .. } => 6,
      MessageContent::Bookmarks { .. } => 7,
      MessageContent::Canvas { .. } => 8,
      MessageContent::Event { .. } => 9,
      MessageContent::Reaction { .. } => 10,
      MessageContent::IntegrationSource { .. } => 11,
      MessageContent::Notetaker { .. } => 12,
      MessageContent::ChannelRolesEntry { .. } => 13,
      MessageContent::Unknown => 14,
      MessageContent::PollVote { .. } => 15,
      MessageContent::PollClose { .. } => 16,
    }
  }
  const KINDS: usize = 17;

  fn every_kind() -> Vec<MessageContent> {
    let target = ClientMessageId::from_bytes([9; 16]);
    let text = |body: &str, meta: MessageMeta| MessageContent::Text {
      body: body.into(),
      mentions: vec![id(3)],
      meta,
    };
    vec![
      text("hello", MessageMeta::default()),
      text(
        "",
        MessageMeta {
          is_silent: true,
          attachments: vec![
            attachment("a.png", "image/png"),
            attachment("b.pdf", "application/pdf"),
          ],
          ..MessageMeta::default()
        },
      ),
      text(
        " ",
        MessageMeta {
          maybe_forwarded: Some(Forwarded {
            author_name: "Alice".into(),
            maybe_conversation_name: Some("planning".into()),
            sent_at_ms: 1,
            body: "lunch?".into(),
          }),
          maybe_source: Some(Box::new(SourceIdentity {
            maybe_id: None,
            name: "Monitoring".into(),
            maybe_picture_sha256: None,
            maybe_picture: Some(Base64Bytes::new(vec![4; 4])),
          })),
          ..MessageMeta::default()
        },
      ),
      MessageContent::File {
        attachment: attachment("voice.ogg", "audio/ogg"),
      },
      MessageContent::Edit {
        target,
        body: "edited".into(),
        custom_emoji: Vec::new(),
      },
      MessageContent::Delete { target },
      MessageContent::Deleted,
      MessageContent::IndexSegment {
        month: "2026-11".into(),
        segment: attachment("segment", "application/octet-stream"),
        message_count: 1,
      },
      MessageContent::Pin {
        target,
        is_pinned: true,
      },
      MessageContent::Bookmarks {
        items: vec![BookmarkItem {
          emoji: "x".into(),
          title: "Docs".into(),
          url: "https://example.com".into(),
          maybe_message: None,
        }],
      },
      MessageContent::Canvas {
        update: Base64Bytes::new(vec![1]),
        is_snapshot: true,
      },
      MessageContent::Event {
        event: ConversationEvent::MemberAdded {
          user: id(5),
          history: HistoryPolicy::Full,
        },
      },
      MessageContent::Event {
        event: ConversationEvent::Renamed { name: "new".into() },
      },
      MessageContent::Event {
        event: ConversationEvent::Joined,
      },
      MessageContent::Event {
        event: ConversationEvent::Unknown,
      },
      MessageContent::Reaction {
        target,
        emoji: "x".into(),
        is_added: true,
        maybe_custom: None,
      },
      MessageContent::IntegrationSource {
        change: SourceChange::Unknown,
      },
      MessageContent::Notetaker {
        request: NotetakerRequest::Stop,
      },
      MessageContent::ChannelRolesEntry {
        version: 2,
        payload: Base64Bytes::new(vec![1]),
        signature: Base64Bytes::new(vec![2]),
      },
      MessageContent::Unknown,
      MessageContent::PollVote {
        target,
        choices: vec![1],
      },
      MessageContent::PollClose { target },
    ]
  }

  #[test]
  fn every_kind_reads_as_the_whole_content_says() {
    let contents = every_kind();
    let mut seen = [false; KINDS];
    for content in &contents {
      seen[kind(content)] = true;
      let json = serde_json::to_vec(content).unwrap();
      let read: ShownContent = serde_json::from_slice(&json).unwrap();
      assert_eq!(read, ShownContent::from(content), "{content:?}");
    }
    assert!(seen.iter().all(|seen| *seen), "a kind without a sample");
  }

  #[test]
  fn what_a_notification_shows_is_kept() {
    let contents = every_kind();
    assert_eq!(
      ShownContent::from(&contents[1]),
      ShownContent::Text {
        body: String::new(),
        is_silent: true,
        maybe_file: Some(ShownFile {
          file_name: "a.png".into(),
          media_type: "image/png".into(),
        }),
        maybe_forwarded_body: None,
        maybe_source_name: None,
      }
    );
    let ShownContent::Text {
      maybe_forwarded_body,
      maybe_source_name,
      ..
    } = ShownContent::from(&contents[2])
    else {
      panic!("a text");
    };
    assert_eq!(maybe_forwarded_body.as_deref(), Some("lunch?"));
    assert_eq!(maybe_source_name.as_deref(), Some("Monitoring"));
    assert_eq!(
      ShownContent::from(&contents[11]),
      ShownContent::MemberAdded(id(5))
    );
    assert_eq!(ShownContent::from(&contents[12]), ShownContent::Change);
  }

  #[test]
  fn kinds_and_fields_of_newer_clients() {
    let read = |json: &str| serde_json::from_str::<ShownContent>(json).unwrap();
    assert_eq!(read(r#"{"type":"poll","question":"?"}"#), ShownContent::Unknown);
    assert_eq!(
      read(r#"{"type":"event","event":{"event":"made_secret","by":1}}"#),
      ShownContent::Change
    );
    assert_eq!(
      read(r#"{"type":"reaction","target":"x","new":true}"#),
      ShownContent::Change
    );
    assert_eq!(
      read(r#"{"type":"text","body":"hi","meta":{"is_silent":true,"sticker":{"id":1}},"style":"bold"}"#),
      ShownContent::Text {
        body: "hi".into(),
        is_silent: true,
        maybe_file: None,
        maybe_forwarded_body: None,
        maybe_source_name: None,
      }
    );
  }

  #[test]
  fn a_stored_message_reads_as_the_whole_message_says() {
    for (n, content) in every_kind().into_iter().enumerate() {
      let message = StoredMessage {
        conversation_id: id(1),
        client_message_id: ClientMessageId::from_bytes([1; 16]),
        // One still in the outbox, without a server ID.
        maybe_server_id: (n > 0).then(|| id(2)),
        sender_user_id: id(3),
        maybe_thread_id: Some(id(4)),
        sent_at_ms: 5,
        content,
        is_edited: true,
        edits: Vec::new(),
        maybe_edit_id: None,
        is_pinned: true,
        maybe_verified_commit: None,
        maybe_server_change: None,
        maybe_unreadable: Some(crate::store::Unreadable::Failed),
      };
      let json = serde_json::to_vec(&message).unwrap();
      let read: ShownMessage = serde_json::from_slice(&json).unwrap();
      assert_eq!(read, ShownMessage::from(&message));
    }
  }
}
