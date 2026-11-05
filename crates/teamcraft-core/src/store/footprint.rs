//! How much memory a decoded [`StoredMessage`] takes, for the caches that are bounded by bytes
//! (issue #1125): the sealed store's decoded messages and [`super::TrackedStore`]'s message cache.
//!
//! The figure is an estimate, not an allocator's: the size of the structs plus the length of every
//! string, list and byte buffer behind them, plus a flat allowance per cache entry for the
//! bookkeeping around it. It costs one pass over the message's own fields and never looks at the
//! conversation, so counting a write or a read stays O(message).

use std::mem::size_of;

use teamcraft_proto::{Base64Bytes, UserId};

use super::{
  AttachmentRef, BookmarkItem, ConversationEvent, EditRecord, Forwarded, LinkPreviewData, MessageContent,
  MessageMeta, ReplyAttachment, ReplyRef, StoredMessage,
};

/// What the map, the recency list and the allocator add around one cached message.
const ENTRY_OVERHEAD_BYTES: usize = 96;
/// For the parts whose strings are not worth walking (an imported author, a source's options).
const SMALL_PART_ALLOWANCE_BYTES: usize = 128;

impl StoredMessage {
  /// About how many bytes this message takes in memory as a cache entry.
  pub(crate) fn footprint_bytes(&self) -> usize {
    ENTRY_OVERHEAD_BYTES
      + size_of::<Self>()
      + self.content.heap_bytes()
      + self.edits.capacity() * size_of::<EditRecord>()
      + self.edits.iter().map(|edit| edit.body.len()).sum::<usize>()
  }
}

impl MessageContent {
  fn heap_bytes(&self) -> usize {
    match self {
      Self::Text { body, mentions, meta } => {
        body.len() + mentions.len() * size_of::<UserId>() + meta.heap_bytes()
      },
      Self::File { attachment } => attachment.heap_bytes(),
      Self::Edit { body, .. } => body.len(),
      Self::IndexSegment { month, segment, .. } => month.len() + segment.heap_bytes(),
      Self::Bookmarks { items } => items.iter().map(BookmarkItem::heap_bytes).sum::<usize>(),
      Self::Canvas { update, .. } => update.len(),
      Self::Event { event } => event.heap_bytes(),
      Self::Reaction { emoji, .. } => emoji.len(),
      Self::IntegrationSource { .. } | Self::Notetaker { .. } => SMALL_PART_ALLOWANCE_BYTES,
      Self::Delete { .. } | Self::Pin { .. } | Self::Deleted | Self::Unknown => 0,
    }
  }
}

impl MessageMeta {
  fn heap_bytes(&self) -> usize {
    let imported = self.maybe_imported.as_ref().map_or(0, |origin| {
      size_of::<crate::imported::ImportedOrigin>()
        + SMALL_PART_ALLOWANCE_BYTES
        + origin.source_id.len()
        + origin.reactions.len() * size_of::<crate::imported::ImportedReaction>()
        + origin
          .missing_files
          .iter()
          .map(|name| name.len() + size_of::<String>())
          .sum::<usize>()
    });
    let workspace_head = self.maybe_workspace_head.as_ref().map_or(0, |_| {
      size_of::<crate::workspace::WorkspaceHead>() + SMALL_PART_ALLOWANCE_BYTES
    });
    let roles_head = self.maybe_channel_roles_head.as_ref().map_or(0, |head| {
      size_of::<crate::channel_roles::ChannelRolesHead>() + head.hash.len()
    });
    let source = self.maybe_source.as_ref().map_or(0, |identity| {
      size_of::<teamcraft_proto::SourceIdentity>()
        + identity.name.len()
        + identity.maybe_picture.as_ref().map_or(0, |picture| picture.len())
    });
    self.maybe_reply_to.as_ref().map_or(0, ReplyRef::heap_bytes)
      + self.maybe_forwarded.as_ref().map_or(0, Forwarded::heap_bytes)
      + self.attachments.capacity() * size_of::<AttachmentRef>()
      + self
        .attachments
        .iter()
        .map(AttachmentRef::heap_bytes)
        .sum::<usize>()
      + self
        .maybe_link_preview
        .as_ref()
        .map_or(0, LinkPreviewData::heap_bytes)
      + workspace_head
      + roles_head
      + imported
      + source
  }
}

impl AttachmentRef {
  fn heap_bytes(&self) -> usize {
    self.key.len()
      + self.ciphertext_sha256.len()
      + self.file_name.len()
      + self.media_type.len()
      + self.maybe_thumbnail.as_ref().map_or(0, Base64Bytes::len)
  }
}

impl ReplyRef {
  fn heap_bytes(&self) -> usize {
    self.excerpt.len()
      + self
        .maybe_attachment
        .as_deref()
        .map_or(0, ReplyAttachment::heap_bytes)
  }
}

impl ReplyAttachment {
  fn heap_bytes(&self) -> usize {
    self.file_name.len()
      + self
        .maybe_thumbnail
        .as_ref()
        .map_or(0, |bytes| bytes.as_slice().len())
  }
}

impl Forwarded {
  fn heap_bytes(&self) -> usize {
    self.author_name.len() + self.maybe_conversation_name.as_ref().map_or(0, String::len) + self.body.len()
  }
}

impl LinkPreviewData {
  fn heap_bytes(&self) -> usize {
    self.url.len()
      + self.maybe_title.as_ref().map_or(0, String::len)
      + self.maybe_description.as_ref().map_or(0, String::len)
      + self.maybe_site_name.as_ref().map_or(0, String::len)
      + self.maybe_image.as_ref().map_or(0, Base64Bytes::len)
  }
}

impl BookmarkItem {
  fn heap_bytes(&self) -> usize {
    size_of::<Self>() + self.emoji.len() + self.title.len() + self.url.len()
  }
}

impl ConversationEvent {
  fn heap_bytes(&self) -> usize {
    match self {
      Self::Renamed { name } => name.len(),
      Self::TopicChanged { topic } => topic.len(),
      Self::Moved { space } => space.len(),
      Self::CallMoved { added, .. } => added.len() * size_of::<UserId>(),
      _ => 0,
    }
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{ClientMessageId, ConversationId, Id, Snowflake};

  use super::*;

  #[test]
  fn a_long_text_counts_its_length_and_a_short_one_little() {
    let short = text_message("hi");
    let long = text_message(&"x".repeat(100_000));
    assert!(short.footprint_bytes() < 2_048, "{}", short.footprint_bytes());
    assert!(long.footprint_bytes() >= 100_000);
    assert!(long.footprint_bytes() < 100_000 + 2_048);
  }

  #[test]
  fn a_sources_name_counts() {
    let mut message = text_message("hi");
    let plain = message.footprint_bytes();
    if let MessageContent::Text { meta, .. } = &mut message.content {
      meta.maybe_source = Some(Box::new(teamcraft_proto::SourceIdentity {
        maybe_id: None,
        name: "n".repeat(5_000),
        maybe_picture_sha256: None,
        maybe_picture: None,
      }));
    }
    assert!(message.footprint_bytes() >= plain + 5_000);
  }

  #[test]
  fn edits_count() {
    let mut message = text_message("hi");
    let plain = message.footprint_bytes();
    message.edits.push(EditRecord {
      body: "e".repeat(5_000),
      replaced_at_ms: 0,
    });
    assert!(message.footprint_bytes() >= plain + 5_000);
  }

  fn text_message(body: &str) -> StoredMessage {
    StoredMessage {
      conversation_id: ConversationId::from_snowflake(Snowflake::new(1).unwrap()),
      client_message_id: ClientMessageId::from_bytes([1; 16]),
      maybe_server_id: None,
      sender_user_id: UserId::from_snowflake(Snowflake::new(1).unwrap()),
      maybe_thread_id: None,
      sent_at_ms: 0,
      content: MessageContent::Text {
        body: body.to_string(),
        mentions: Vec::new(),
        meta: MessageMeta::default(),
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
}
