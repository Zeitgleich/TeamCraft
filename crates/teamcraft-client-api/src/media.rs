//! What the persistent media cache keeps (issue #749; `teamcraft-core` `media_cache`): the key of
//! an entry as the page names it to the desktop shell or the core in the page.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{AttachmentId, ConversationId, SpaceId, UserId};

/// Size limit of the cache unless the user picks another one.
pub const DEFAULT_MEDIA_CACHE_BYTES: u64 = 200 * 1024 * 1024;

/// What a cache entry shows; one entry per key, holding the version it was made from. Serialized
/// as `{"kind": "avatar", "id": "…"}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum MediaKey {
  Avatar(UserId),
  SpaceImage(SpaceId),
  ChannelImage(ConversationId),
  /// The inline rendition of an image attachment (scaled down for the conversation).
  Rendition(AttachmentId),
}

impl MediaKey {
  /// The name an entry's storage ID is derived from; the cache never stores it in the clear.
  pub fn name(&self) -> String {
    match self {
      Self::Avatar(user) => format!("avatar/{user}"),
      Self::SpaceImage(space) => format!("space_image/{space}"),
      Self::ChannelImage(channel) => format!("channel_image/{channel}"),
      Self::Rendition(attachment) => format!("rendition/{attachment}"),
    }
  }
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
      serde_json::from_value(serde_json::to_value(MediaKey::Rendition(id(9))).expect("serialize"))
        .expect("parse");
    assert_eq!(parsed, MediaKey::Rendition(id(9)));
    let names = [
      user.name(),
      MediaKey::SpaceImage(id(7)).name(),
      MediaKey::ChannelImage(id(7)).name(),
      MediaKey::Rendition(id(7)).name(),
    ];
    let distinct: std::collections::HashSet<_> = names.iter().collect();
    assert_eq!(distinct.len(), 4, "the same ID of another kind is another entry");
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).expect("a valid test ID"))
  }
}
