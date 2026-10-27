//! The picture of a Web Push notification (#543), as the app's own notifications show it: the
//! sender's avatar for direct conversations and calls, the channel's picture (else its space's) for
//! channels. The service worker reads it from the account's media cache in the browser
//! ([`crate::media_cache`]), where the page kept every picture it showed; a push never fetches one.
//!
//! Pictures follow the names: a preview the workspace policy or the device setting cut down to
//! "New message" names nobody, and so shows nobody's picture. The worker has no canvas of its own
//! here, so the picture goes as it is cached (the browser scales it), without the app's round cut
//! and without a generated initials tile.

use base64::Engine as _;
use teamcraft_proto::{ConversationResponse, PushKind, UserId};

use crate::error::CoreError;
use crate::media_cache::MediaKey;
use crate::push_preview::PushPreview;
use crate::records;
use crate::store::{LocalStore, Table};

/// Largest cached picture used as a notification's icon: a 512 px avatar fits; the data URL stays
/// well below the 2 MB browsers allow for a URL.
pub const MAX_ICON_BYTES: usize = 1024 * 1024;
/// The picture types every browser shows as an icon; the server re-encodes uploads to these.
const ICON_TYPES: [&str; 3] = ["image/png", "image/jpeg", "image/webp"];

/// The cached pictures `preview` may show, best first: none where it names nobody.
pub fn push_picture_keys(store: &impl LocalStore, preview: &PushPreview) -> Vec<MediaKey> {
  let conversation = preview.wake_up.conversation_id.to_string();
  // A record that can't be read only costs the channel's and space's picture.
  let maybe_conversation =
    records::get_json::<ConversationResponse>(store, Table::Conversations, &conversation)
      .ok()
      .flatten();
  picture_keys(preview, maybe_conversation.as_ref())
}

/// [`push_picture_keys`] from the conversation's record, if the store has it.
pub fn picture_keys(
  preview: &PushPreview,
  maybe_conversation: Option<&ConversationResponse>,
) -> Vec<MediaKey> {
  let is_person = preview.is_direct || preview.wake_up.kind == PushKind::Call;
  match preview.wake_up.kind {
    PushKind::Message | PushKind::Mention | PushKind::Call => {},
    _ => return Vec::new(),
  }
  if is_person {
    return match (preview.wake_up.maybe_sender_id, &preview.maybe_sender_name) {
      (Some(sender), Some(_)) => vec![MediaKey::Avatar(sender)],
      _ => Vec::new(),
    };
  }
  if preview.maybe_conversation_name.is_none() {
    return Vec::new();
  }
  let mut keys = Vec::new();
  if let Some(conversation) = maybe_conversation {
    if conversation.maybe_image.is_some() {
      keys.push(MediaKey::ChannelImage(conversation.id));
    }
    if let Some(space) = conversation.maybe_space_id {
      keys.push(MediaKey::SpaceImage(space));
    }
  }
  keys
}

/// The account whose media cache the worker reads: the store's own user.
pub fn account_user(store: &impl LocalStore) -> Result<Option<UserId>, CoreError> {
  records::account_user_id(store)
}

/// A cached picture as a `data:` URL for a notification's `icon`; `None` for a type browsers may
/// not show or a picture too large for a URL.
pub fn icon_data_url(content_type: &str, bytes: &[u8]) -> Option<String> {
  let essence = content_type
    .split(';')
    .next()
    .unwrap_or_default()
    .trim()
    .to_ascii_lowercase();
  let known = ICON_TYPES.into_iter().find(|known| *known == essence)?;
  if bytes.is_empty() || bytes.len() > MAX_ICON_BYTES {
    return None;
  }
  Some(format!(
    "data:{known};base64,{}",
    base64::engine::general_purpose::STANDARD.encode(bytes)
  ))
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{
    ChannelModeration, ConversationKind, ConversationVisibility, HistoryPolicy, Id, PushWakeUp, Snowflake,
    SpaceImage,
  };

  use super::*;
  use crate::push_preview::PreviewContent;

  #[test]
  fn a_direct_message_or_call_shows_the_sender() {
    let mut preview = preview(PushKind::Message, true);
    assert_eq!(picture_keys(&preview, None), [MediaKey::Avatar(id(2))]);
    preview.wake_up.kind = PushKind::Mention;
    assert_eq!(picture_keys(&preview, None), [MediaKey::Avatar(id(2))]);
    let mut call = self::preview(PushKind::Call, false);
    call.maybe_conversation_name = None;
    assert_eq!(
      picture_keys(&call, Some(&channel(true))),
      [MediaKey::Avatar(id(2))],
      "a call shows who calls, also in a channel"
    );
  }

  #[test]
  fn a_channel_shows_its_picture_then_its_spaces() {
    let preview = preview(PushKind::Message, false);
    assert_eq!(
      picture_keys(&preview, Some(&channel(true))),
      [MediaKey::ChannelImage(id(1)), MediaKey::SpaceImage(id(3))]
    );
    assert_eq!(
      picture_keys(&preview, Some(&channel(false))),
      [MediaKey::SpaceImage(id(3))]
    );
    assert_eq!(picture_keys(&preview, None), Vec::new(), "an unknown channel");
  }

  #[test]
  fn a_preview_that_names_nobody_shows_no_picture() {
    let mut direct = preview(PushKind::Message, true);
    direct.maybe_sender_name = None;
    assert_eq!(picture_keys(&direct, None), Vec::new());
    let mut channel_message = preview(PushKind::Message, false);
    channel_message.maybe_conversation_name = None;
    channel_message.maybe_sender_name = None;
    assert_eq!(picture_keys(&channel_message, Some(&channel(true))), Vec::new());
    let limited =
      self::preview(PushKind::Message, true).limited_to(&teamcraft_proto::PushContentMode::Generic);
    assert_eq!(picture_keys(&limited, None), Vec::new(), "the workspace policy");
    let sender_only =
      self::preview(PushKind::Message, true).limited_to(&teamcraft_proto::PushContentMode::Sender);
    assert_eq!(
      picture_keys(&sender_only, None),
      [MediaKey::Avatar(id(2))],
      "names, but no text"
    );
  }

  #[test]
  fn other_wake_ups_show_no_picture() {
    for kind in [PushKind::Test, PushKind::Dismiss] {
      assert_eq!(picture_keys(&preview(kind, true), None), Vec::new());
    }
  }

  #[test]
  fn icons_are_data_urls_of_known_types_within_the_limit() {
    assert_eq!(
      icon_data_url("image/png", b"png").as_deref(),
      Some("data:image/png;base64,cG5n")
    );
    assert_eq!(
      icon_data_url("Image/JPEG; charset=binary", b"jpg").as_deref(),
      Some("data:image/jpeg;base64,anBn")
    );
    assert_eq!(
      icon_data_url("image/svg+xml", b"<svg/>"),
      None,
      "SVG may carry script"
    );
    assert_eq!(icon_data_url("text/html", b"<b>"), None);
    assert_eq!(icon_data_url("image/png", &[]), None);
    assert_eq!(icon_data_url("image/png", &vec![0; MAX_ICON_BYTES + 1]), None);
    assert!(icon_data_url("image/webp", &vec![0; MAX_ICON_BYTES]).is_some());
  }

  fn preview(kind: PushKind, is_direct: bool) -> PushPreview {
    PushPreview {
      wake_up: PushWakeUp {
        kind,
        conversation_id: id(1),
        maybe_message_id: None,
        maybe_sender_id: Some(id(2)),
        is_urgent: false,
        maybe_thread_id: None,
        maybe_call_id: None,
        maybe_badge: None,
        maybe_sent_at: None,
      },
      maybe_conversation_name: Some("planning".into()),
      maybe_sender_name: Some("Alice".into()),
      content: PreviewContent::Text("lunch?".into()),
      is_direct,
      is_silent: false,
      is_conversation_verified: true,
      maybe_gap: None,
    }
  }

  fn channel(has_picture: bool) -> ConversationResponse {
    ConversationResponse {
      id: id(1),
      maybe_space_id: Some(id(3)),
      kind: ConversationKind::Channel,
      maybe_name: Some("planning".into()),
      maybe_topic: None,
      maybe_description: None,
      visibility: ConversationVisibility::Public,
      history_policy: HistoryPolicy::Full,
      maybe_category_id: None,
      is_archived: false,
      moderation: ChannelModeration::default(),
      is_integration: false,
      contains_guests: false,
      maybe_organizer_user_id: None,
      maybe_my_permissions: None,
      maybe_my_membership: None,
      participants: Vec::new(),
      maybe_image: has_picture.then(|| SpaceImage {
        version: "v1".into(),
        size_bytes: 3,
      }),
    }
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).expect("a valid test ID"))
  }
}
