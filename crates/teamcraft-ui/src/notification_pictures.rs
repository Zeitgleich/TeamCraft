//! The picture beside a system notification (#543): the sender's avatar for direct messages and
//! calls, the channel's picture (else its space's) for channel messages.
//!
//! The UI picks the picture; the desktop shell or the page shows it. Both read it from the
//! persistent media cache ([`crate::persisted_media`]), where the hubs keep every avatar and space
//! or channel picture they showed, so a notification never fetches one. Without a cached copy the
//! notification shows the tile the app draws instead (initials on the person's or space's colour),
//! drawn here into a canvas. Pictures follow the names: only a notification that may name the
//! sender gets one (`notifier::limited_to`, `call_alerts::may_name_names`).

use std::cell::RefCell;

#[cfg(target_arch = "wasm32")]
use teamcraft_client_api::os_notify::NOTIFICATION_PICTURE_PX;
use teamcraft_client_api::os_notify::{NotificationPicture, PictureShape};
use teamcraft_proto::Base64Bytes;

use crate::backend::Backend;
use crate::model::{Conversation, Space, User, UserId, initials};
use crate::notifier::PictureSubject;
use crate::persisted_media::MediaKey;

/// Drawn tiles kept for the next notification of the same person or space, newest last.
const MAX_DRAWN_TILES: usize = 32;

thread_local! {
  /// Tiles drawn this session with their PNG; a person who writes again is not drawn again.
  static DRAWN_TILES: RefCell<Vec<(Tile, Vec<u8>)>> = const { RefCell::new(Vec::new()) };
}

/// The generated stand-in for a picture, as the app shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tile {
  pub letters: String,
  /// CSS colour of the background: `.avatar` and `.space-btn` in main.css.
  pub background: String,
  pub shape: PictureShape,
}

/// The picture of `subject`; `None` where there is nothing to show (a channel outside any space
/// without a picture of its own).
pub fn picture_of(backend: &dyn Backend, subject: PictureSubject) -> Option<NotificationPicture> {
  match subject {
    PictureSubject::Person(user) => Some(person_picture(user, backend.user(user).as_ref())),
    PictureSubject::Place {
      conversation,
      maybe_space,
    } => {
      let maybe_conversation = backend.conversation(conversation);
      let maybe_space =
        maybe_space.and_then(|space| backend.spaces().into_iter().find(|known| known.id == space));
      place_picture(maybe_conversation.as_ref(), maybe_space.as_ref())
    },
  }
}

/// A person's avatar, cut to a circle; their initials tile while the cache has no picture (none
/// for a deleted account, which has no name to take letters from).
pub fn person_picture(user: UserId, maybe_user: Option<&User>) -> NotificationPicture {
  let maybe_tile = maybe_user.filter(|user| !user.is_deleted).map(person_tile);
  NotificationPicture {
    shape: PictureShape::Circle,
    maybe_key: Some(MediaKey::Avatar(user)),
    maybe_fallback_png: maybe_tile.and_then(|tile| drawn(&tile)),
  }
}

/// A channel's picture where it has one, else its space's, as a rounded tile; the space's letter
/// tile while the cache has neither. `None` when there is neither a picture nor a space.
pub fn place_picture(
  maybe_conversation: Option<&Conversation>,
  maybe_space: Option<&Space>,
) -> Option<NotificationPicture> {
  let maybe_channel_key = maybe_conversation
    .filter(|conversation| conversation.maybe_image_version.is_some())
    .map(|conversation| MediaKey::ChannelImage(conversation.id));
  let maybe_space_key = maybe_space
    .filter(|space| space.maybe_image_version.is_some())
    .map(|space| MediaKey::SpaceImage(space.id));
  let maybe_key = maybe_channel_key.or(maybe_space_key);
  let maybe_tile = maybe_space.map(space_tile);
  if maybe_key.is_none() && maybe_tile.is_none() {
    return None;
  }
  Some(NotificationPicture {
    shape: PictureShape::RoundedSquare,
    maybe_key,
    maybe_fallback_png: maybe_tile.and_then(|tile| drawn(&tile)),
  })
}

/// The data URL of the picture for a browser notification's `icon`: the cached picture drawn in its
/// shape, else the generated tile. `None` when there is neither.
pub async fn icon_url(picture: &NotificationPicture) -> Option<String> {
  if let Some(key) = picture.maybe_key
    && let Some(cached) = crate::persisted_media::get(key).await
    && let Some(url) = shaped_icon(&cached.bytes, &cached.content_type, picture.shape).await
  {
    return Some(url);
  }
  picture
    .maybe_fallback_png
    .as_ref()
    .map(|png| png_data_url(png.as_slice()))
}

/// The tile of a person: as `.avatar` draws it.
pub fn person_tile(user: &User) -> Tile {
  Tile {
    letters: user.initials(),
    background: hsl(user.avatar_hue, 45, 32),
    shape: PictureShape::Circle,
  }
}

/// The tile of a space: as `.space-btn` draws it.
pub fn space_tile(space: &Space) -> Tile {
  Tile {
    letters: initials(&space.name),
    background: hsl(space.hue, 40, 32),
    shape: PictureShape::RoundedSquare,
  }
}

/// `hsl()` in the comma syntax every canvas understands.
fn hsl(hue: u16, saturation_percent: u8, lightness_percent: u8) -> String {
  format!("hsl({}, {saturation_percent}%, {lightness_percent}%)", hue % 360)
}

fn png_data_url(png: &[u8]) -> String {
  use base64::Engine as _;
  format!(
    "data:image/png;base64,{}",
    base64::engine::general_purpose::STANDARD.encode(png)
  )
}

/// The tile's PNG, drawn once per session.
fn drawn(tile: &Tile) -> Option<Base64Bytes> {
  let maybe_known = DRAWN_TILES.with_borrow(|tiles| {
    tiles
      .iter()
      .find(|(known, _)| known == tile)
      .map(|(_, png)| png.clone())
  });
  if let Some(png) = maybe_known {
    return Some(Base64Bytes::new(png));
  }
  let png = draw_tile(tile)?;
  DRAWN_TILES.with_borrow_mut(|tiles| {
    if tiles.len() >= MAX_DRAWN_TILES {
      tiles.remove(0);
    }
    tiles.push((tile.clone(), png.clone()));
  });
  Some(Base64Bytes::new(png))
}

#[cfg(target_arch = "wasm32")]
fn draw_tile(tile: &Tile) -> Option<Vec<u8>> {
  crate::raster::tile_png(
    &tile.letters,
    &tile.background,
    NOTIFICATION_PICTURE_PX,
    tile.shape,
  )
}

/// Tests run natively, without a canvas.
#[cfg(not(target_arch = "wasm32"))]
fn draw_tile(_tile: &Tile) -> Option<Vec<u8>> {
  None
}

#[cfg(target_arch = "wasm32")]
async fn shaped_icon(bytes: &[u8], media_type: &str, shape: PictureShape) -> Option<String> {
  crate::raster::shaped_square_png_url(bytes, media_type, NOTIFICATION_PICTURE_PX, shape).await
}

#[cfg(not(target_arch = "wasm32"))]
async fn shaped_icon(_bytes: &[u8], _media_type: &str, _shape: PictureShape) -> Option<String> {
  None
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::{ConversationKind, DeviceSetup, PresenceStatus, SidebarCategory, id_from_raw};

  #[test]
  fn a_person_shows_their_avatar_in_a_circle() {
    let user = person();
    let picture = person_picture(user.id, Some(&user));
    assert_eq!(picture.shape, PictureShape::Circle);
    assert_eq!(picture.maybe_key, Some(MediaKey::Avatar(user.id)));
    let unknown = person_picture(id_from_raw(9), None);
    assert_eq!(
      unknown.maybe_key,
      Some(MediaKey::Avatar(id_from_raw(9))),
      "a cached picture still shows"
    );
    assert_eq!(unknown.maybe_fallback_png, None, "no name, no letters");
  }

  #[test]
  fn a_channel_shows_its_own_picture_else_its_spaces() {
    let mut channel = channel();
    let mut space = space();
    let picture = place_picture(Some(&channel), Some(&space)).expect("the space's tile at least");
    assert_eq!(picture.shape, PictureShape::RoundedSquare);
    assert_eq!(picture.maybe_key, None, "neither has a picture: the letter tile");
    space.maybe_image_version = Some("s1".into());
    assert_eq!(
      place_picture(Some(&channel), Some(&space)).and_then(|picture| picture.maybe_key),
      Some(MediaKey::SpaceImage(space.id))
    );
    channel.maybe_image_version = Some("c1".into());
    assert_eq!(
      place_picture(Some(&channel), Some(&space)).and_then(|picture| picture.maybe_key),
      Some(MediaKey::ChannelImage(channel.id))
    );
    assert_eq!(
      place_picture(Some(&channel), None).and_then(|picture| picture.maybe_key),
      Some(MediaKey::ChannelImage(channel.id)),
      "a channel picture needs no space"
    );
    channel.maybe_image_version = None;
    assert_eq!(place_picture(Some(&channel), None), None, "nothing to show");
  }

  #[test]
  fn tiles_use_the_apps_letters_and_colours() {
    assert_eq!(
      person_tile(&person()),
      Tile {
        letters: "AL".into(),
        background: "hsl(200, 45%, 32%)".into(),
        shape: PictureShape::Circle,
      }
    );
    assert_eq!(
      space_tile(&space()),
      Tile {
        letters: "PA".into(),
        background: "hsl(30, 40%, 32%)".into(),
        shape: PictureShape::RoundedSquare,
      }
    );
  }

  #[test]
  fn a_generated_tile_is_a_png_data_url() {
    assert_eq!(png_data_url(b"png"), "data:image/png;base64,cG5n");
  }

  fn person() -> User {
    User {
      id: id_from_raw(2),
      username: "ada".into(),
      display_name: "Ada Lovelace".into(),
      presence: PresenceStatus::Online,
      maybe_custom_status: None,
      avatar_hue: 200,
      is_guest: false,
      is_bot: false,
      maybe_organization: None,
      is_deactivated: false,
      is_deleted: false,
      device_setup: DeviceSetup::Unknown,
      maybe_email: None,
    }
  }

  fn channel() -> Conversation {
    Conversation {
      id: id_from_raw(7),
      maybe_space: Some(id_from_raw(3)),
      kind: ConversationKind::Public,
      category: SidebarCategory::Channels,
      name: "launch".into(),
      topic: String::new(),
      member_ids: Vec::new(),
      unread_count: 0,
      mention_count: 0,
      is_favorite: false,
      is_muted: false,
      can_post: true,
      bookmarks: Vec::new(),
      is_catching_up: false,
      is_notes_to_self: false,
      maybe_permissions: None,
      has_only_deactivated_others: false,
      maybe_image_version: None,
    }
  }

  fn space() -> Space {
    serde_json::from_value(serde_json::json!({
      "id": "3",
      "name": "Project A",
      "hue": 30,
      "unread_count": 0,
      "mention_count": 0
    }))
    .expect("a space")
  }
}
