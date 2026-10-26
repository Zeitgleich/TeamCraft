//! What the persistent media cache keeps (issue #749; `teamcraft-core` `media_cache`): the key of
//! an entry as the page names it to the desktop shell or the core in the page.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{AttachmentId, ConversationId, MessageId, SpaceId, UserId};

/// Size limit of the cache unless the user picks another one.
pub const DEFAULT_MEDIA_CACHE_BYTES: u64 = 200 * 1024 * 1024;
/// The largest size limit the settings offer; hosts clamp what the page asks for to it.
pub const MAX_MEDIA_CACHE_BYTES: u64 = 1024 * 1024 * 1024;
/// How long an entry is kept after it was written, unless the user picks another age: a backstop,
/// so a picture whose message went away (or that nobody shows any more) doesn't stay for good.
pub const DEFAULT_MEDIA_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(30 * 24 * 60 * 60);
/// The longest age the settings offer; hosts clamp what the page asks for to it.
pub const MAX_MEDIA_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(365 * 24 * 60 * 60);

/// What a cache entry shows; one entry per key, holding the version it was made from. Serialized
/// as `{"kind": "avatar", "id": "…"}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum MediaKey {
  Avatar(UserId),
  SpaceImage(SpaceId),
  ChannelImage(ConversationId),
  /// The inline rendition of an image attachment (scaled down for the conversation), bound to the
  /// message that carries it and the SHA-256 of the attachment's ciphertext named in that message:
  /// another message referencing the same attachment ID is another entry, and never shows this
  /// one's picture.
  Rendition {
    conversation: ConversationId,
    message: MessageId,
    attachment: AttachmentId,
    content_sha256: [u8; 32],
  },
}

impl MediaKey {
  /// The name an entry's storage ID is derived from; the cache never stores it in the clear.
  pub fn name(&self) -> String {
    match self {
      Self::Avatar(user) => format!("avatar/{user}"),
      Self::SpaceImage(space) => format!("space_image/{space}"),
      Self::ChannelImage(channel) => format!("channel_image/{channel}"),
      Self::Rendition {
        conversation,
        message,
        attachment,
        content_sha256,
      } => format!(
        "rendition/{conversation}/{message}/{attachment}/{}",
        to_hex(content_sha256)
      ),
    }
  }
}

/// A SHA-256 as 64 lowercase hex digits (how [`crate::actions::FileInfo`] carries it).
pub fn to_hex(digest: &[u8; 32]) -> String {
  digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A SHA-256 from its 64 hex digits; `None` for anything else.
pub fn sha256_from_hex(hex: &str) -> Option<[u8; 32]> {
  if hex.len() != 64 {
    return None;
  }
  let mut digest = [0u8; 32];
  let (pairs, _) = hex.as_bytes().as_chunks::<2>();
  for (byte, pair) in digest.iter_mut().zip(pairs) {
    *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
  }
  Some(digest)
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  #[test]
  fn keys_keep_their_wire_form_and_names_apart() {
    let user = MediaKey::Avatar(id(7));
    assert_eq!(
      serde_json::to_value(user).expect("serialize"),
      serde_json::json!({ "kind": "avatar", "id": serde_json::to_value(id::<UserId>(7)).expect("id") })
    );
    let parsed: MediaKey =
      serde_json::from_value(serde_json::to_value(rendition(9, 1)).expect("serialize")).expect("parse");
    assert_eq!(parsed, rendition(9, 1));
    let names = [
      user.name(),
      MediaKey::SpaceImage(id(7)).name(),
      MediaKey::ChannelImage(id(7)).name(),
      rendition(7, 1).name(),
    ];
    let distinct: std::collections::HashSet<_> = names.iter().collect();
    assert_eq!(distinct.len(), 4, "the same ID of another kind is another entry");
  }

  #[test]
  fn a_rendition_is_bound_to_its_message_and_ciphertext() {
    let own = rendition(7, 1);
    let mut other_message = own;
    if let MediaKey::Rendition { message, .. } = &mut other_message {
      *message = id(8);
    }
    let mut other_blob = own;
    if let MediaKey::Rendition { content_sha256, .. } = &mut other_blob {
      content_sha256[0] ^= 1;
    }
    assert_ne!(
      own.name(),
      other_message.name(),
      "another message with the same attachment ID"
    );
    assert_ne!(
      own.name(),
      other_blob.name(),
      "the same IDs with another ciphertext"
    );
  }

  #[test]
  fn digests_round_trip_through_hex_and_garbage_is_none() {
    let digest = [0xab; 32];
    assert_eq!(sha256_from_hex(&to_hex(&digest)), Some(digest));
    assert_eq!(sha256_from_hex("ab"), None);
    assert_eq!(sha256_from_hex(&"zz".repeat(32)), None);
  }

  fn rendition(message: i64, digest_byte: u8) -> MediaKey {
    MediaKey::Rendition {
      conversation: id(1),
      message: id(message),
      attachment: id(3),
      content_sha256: [digest_byte; 32],
    }
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).expect("a valid test ID"))
  }
}
