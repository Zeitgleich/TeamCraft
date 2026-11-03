//! Space and channel pictures: directory data the server owns, like the name (e2ee.md §4.4).
//!
//! The hub downloads a picture once per version from `GET /api/v1/spaces/{id}/image` (channels:
//! `/api/v1/conversations/{id}/image`) and shows it
//! from a `blob:` URL (the page's CSP only lets images come from `blob:` and `data:`). Uploads are
//! cropped and scaled to 256×256 in the crop dialog first (`image_crop`); the server checks and
//! re-encodes them again.
//!
//! Pictures are also kept in the persistent media cache (#749, [`crate::persisted_media`]): the
//! version the snapshot names comes from the cache without a download, also offline. A new version
//! is downloaded in the background while the cached older one stays on screen, and stays there
//! when the download fails (the server or its storage is down).

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::Deserialize;
use teamcraft_proto::{ErrorBody, ErrorCode, SpaceImage};

use crate::auth::{ApiMethod, Authenticator, BinaryBody};
use crate::components::file_views::blob_url;
use crate::feedback::{ActionFailure, FailureKind};
use crate::model::{ConversationId, SpaceId};
use crate::persisted_media::{self, CachedPicture, Freshness, MediaKey};
use crate::reactive::UpdateIfAlive;

/// What the client uploads after scaling.
const UPLOAD_CONTENT_TYPE: &str = "image/png";

#[derive(Clone, Copy)]
pub struct SpaceImageHub {
  auth: StoredValue<Rc<dyn Authenticator>, LocalStorage>,
  cache: RwSignal<ImageCache>,
  asked: StoredValue<HashSet<(ImageOwner, String)>>,
}

/// Whose picture it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImageOwner {
  Space(SpaceId),
  Channel(ConversationId),
}

/// The loaded pictures: one `blob:` URL per owner, with the version it shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImageCache {
  entries: HashMap<ImageOwner, (String, String)>,
}

/// What the server answers to an upload or removal: the space or channel with its new picture.
#[derive(Deserialize)]
struct PictureAnswer {
  #[serde(rename = "image", default)]
  maybe_image: Option<SpaceImage>,
}

/// A failed upload or removal, for the dialog's message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpaceImageFailure {
  /// The server's error code (`payload_too_large`, `forbidden`, ...).
  Refused(String),
  Network,
}

impl SpaceImageHub {
  pub fn new(auth: Rc<dyn Authenticator>) -> Self {
    Self {
      auth: StoredValue::new_local(auth),
      cache: RwSignal::new(ImageCache::default()),
      asked: StoredValue::new(HashSet::new()),
    }
  }

  /// A `blob:` URL of the owner's picture in `maybe_version` once it has loaded, or of an older
  /// version until then; the first call for a version starts loading it. `None` as the version:
  /// the owner has no picture (any more). Reactive.
  pub fn image_url(&self, owner: ImageOwner, maybe_version: Option<&str>) -> Option<String> {
    let Some(version) = maybe_version else {
      if self.cache.with(|cache| cache.shows(owner)) {
        // Removed: the letter tile again, and the cached copy goes too.
        let hub = *self;
        spawn_local(async move { hub.forget(owner) });
        persisted_media::remove(owner.media_key());
      }
      return None;
    };
    let (maybe_url, is_current) = self.cache.with(|cache| {
      let maybe_url = cache.best_url(owner).map(str::to_owned);
      (maybe_url, cache.url(owner, version).is_some())
    });
    if !is_current
      && self
        .asked
        .try_update_value(|asked| asked.insert((owner, version.to_owned())))
        == Some(true)
    {
      self.load(owner, version.to_owned());
    }
    maybe_url
  }

  /// Uploads an already scaled PNG; the answer carries the new picture.
  pub async fn upload(
    &self,
    owner: ImageOwner,
    png: Vec<u8>,
  ) -> Result<Option<SpaceImage>, SpaceImageFailure> {
    let body = BinaryBody {
      content_type: UPLOAD_CONTENT_TYPE.to_owned(),
      bytes: png.clone(),
    };
    let updated = self.request(ApiMethod::Put, owner, Some(body)).await?;
    // Shown right away; the lists follow with the gateway's update.
    if let Some(image) = &updated
      && let Some(url) = blob_url(&png, UPLOAD_CONTENT_TYPE)
    {
      self.remember(owner, image.version.clone(), url);
      persisted_media::put(owner.media_key(), picture(image.version.clone(), png));
    }
    Ok(updated)
  }

  pub async fn remove(&self, owner: ImageOwner) -> Result<Option<SpaceImage>, SpaceImageFailure> {
    self.request(ApiMethod::Delete, owner, None).await
  }

  async fn request(
    &self,
    method: ApiMethod,
    owner: ImageOwner,
    maybe_body: Option<BinaryBody>,
  ) -> Result<Option<SpaceImage>, SpaceImageFailure> {
    let path = owner.path();
    let response = self
      .auth
      .get_value()
      .api_request_bytes(method, &path, maybe_body)
      .await
      .map_err(|_| SpaceImageFailure::Network)?;
    let text = String::from_utf8_lossy(&response.body);
    if response.status >= 300 {
      let code = serde_json::from_str::<ErrorBody>(&text)
        .map(|body| body.error_code.as_str().to_owned())
        .unwrap_or_else(|_| "unknown".to_owned());
      return Err(SpaceImageFailure::Refused(code));
    }
    serde_json::from_str::<PictureAnswer>(&text)
      .map(|answer| answer.maybe_image)
      .map_err(|_| SpaceImageFailure::Refused("unreadable".to_owned()))
  }

  /// The picture in `version`: from the persistent cache when it has that version, else
  /// downloaded, with a cached older version shown meanwhile.
  fn load(&self, owner: ImageOwner, version: String) {
    let hub = *self;
    spawn_local(async move {
      if let Some(cached) = persisted_media::get(owner.media_key()).await
        && let Some(url) = blob_url(&cached.bytes, UPLOAD_CONTENT_TYPE)
      {
        if cached.freshness(Some(&version)) == Freshness::Current {
          hub.remember(owner, version, url);
          return;
        }
        hub.remember_if_none(owner, cached.version, url);
      }
      let Some(auth) = hub.auth.try_get_value() else {
        return;
      };
      let result = auth.api_request_bytes(ApiMethod::Get, &owner.path(), None).await;
      match result {
        Ok(image) if image.status == 200 => {
          if let Some(url) = blob_url(&image.body, UPLOAD_CONTENT_TYPE) {
            hub.remember(owner, version.clone(), url);
            persisted_media::put(owner.media_key(), picture(version, image.body));
          }
        },
        // What is shown stays (an older cached picture, or the letter tile); the next version (or
        // session) asks again.
        Ok(_) | Err(_) => tracing::debug!(?owner, "picture not downloaded"),
      }
    });
  }

  fn remember(&self, owner: ImageOwner, version: String, url: String) {
    let mut maybe_replaced = None;
    self
      .cache
      .update_if_alive(|cache| maybe_replaced = cache.insert(owner, version, url));
    if let Some(replaced) = maybe_replaced {
      revoke(&replaced);
    }
  }

  /// Shows a cached older picture while nothing else is shown.
  fn remember_if_none(&self, owner: ImageOwner, version: String, url: String) {
    let mut is_kept = false;
    self.cache.update_if_alive(|cache| {
      if !cache.shows(owner) {
        is_kept = true;
        cache.insert(owner, version, url.clone());
      }
    });
    if !is_kept {
      revoke(&url);
    }
  }

  fn forget(&self, owner: ImageOwner) {
    let mut maybe_removed = None;
    self
      .cache
      .update_if_alive(|cache| maybe_removed = cache.remove(owner));
    if let Some(removed) = maybe_removed {
      revoke(&removed);
    }
    self
      .asked
      .try_update_value(|asked| asked.retain(|(asked_owner, _)| *asked_owner != owner));
  }
}

impl ImageCache {
  /// The URL for `owner` if it shows `version`; an older picture doesn't count.
  pub fn url(&self, owner: ImageOwner, version: &str) -> Option<&str> {
    self
      .entries
      .get(&owner)
      .filter(|(cached, _)| cached == version)
      .map(|(_, url)| url.as_str())
  }

  /// The URL for `owner` whatever version it shows: an older picture until the current one loads.
  pub fn best_url(&self, owner: ImageOwner) -> Option<&str> {
    self.entries.get(&owner).map(|(_, url)| url.as_str())
  }

  pub fn shows(&self, owner: ImageOwner) -> bool {
    self.entries.contains_key(&owner)
  }

  /// Drops the picture of `owner`; returns its URL, which the caller releases.
  pub fn remove(&mut self, owner: ImageOwner) -> Option<String> {
    self.entries.remove(&owner).map(|(_, url)| url)
  }

  /// Keeps `url` for `space`; returns the URL it replaced, which the caller releases.
  pub fn insert(&mut self, owner: ImageOwner, version: String, url: String) -> Option<String> {
    self
      .entries
      .insert(owner, (version, url.clone()))
      .map(|(_, previous)| previous)
      .filter(|previous| *previous != url)
  }
}

pub fn use_space_images() -> Option<SpaceImageHub> {
  use_context::<SpaceImageHub>()
}

impl SpaceImageFailure {
  /// The failure to show the user (issue #322).
  pub fn to_failure(&self) -> ActionFailure {
    match self {
      Self::Refused(code) => match ErrorCode::from(code.clone()) {
        ErrorCode::Unknown(_) => ActionFailure::other(format!("space picture refused: {code}")),
        known => ActionFailure::new(FailureKind::Server(known), "space picture refused"),
      },
      Self::Network => ActionFailure::new(FailureKind::Network, "space picture: no connection"),
    }
  }

  pub fn i18n_key(&self) -> &'static str {
    match self {
      Self::Refused(code) if code == "payload_too_large" => "space-image-size",
      Self::Refused(code) if code == "forbidden" => "space-image-forbidden",
      Self::Refused(code) if code == "invalid_request" => "space-image-unreadable",
      Self::Refused(_) => "space-image-failed",
      Self::Network => "space-image-offline",
    }
  }
}

impl ImageOwner {
  fn media_key(self) -> MediaKey {
    match self {
      Self::Space(space) => MediaKey::SpaceImage(space),
      Self::Channel(channel) => MediaKey::ChannelImage(channel),
    }
  }

  fn path(self) -> String {
    match self {
      Self::Space(space) => format!("/api/v1/spaces/{space}/image"),
      Self::Channel(channel) => format!("/api/v1/conversations/{channel}/image"),
    }
  }
}

/// What the persistent cache keeps of a picture (always the PNG the server re-encoded or the crop
/// dialog made).
fn picture(version: String, png: Vec<u8>) -> CachedPicture {
  CachedPicture {
    version,
    content_type: UPLOAD_CONTENT_TYPE.to_owned(),
    bytes: png,
  }
}

/// Frees a picture's `blob:` URL.
pub(crate) fn revoke(url: &str) {
  #[cfg(target_arch = "wasm32")]
  if let Err(error) = web_sys::Url::revoke_object_url(url) {
    tracing::debug!(?error, "revoking a picture URL failed");
  }
  #[cfg(not(target_arch = "wasm32"))]
  let _ = url;
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::id_from_raw;

  #[test]
  fn the_cache_only_serves_the_current_version() {
    let (a, b) = (
      ImageOwner::Space(id_from_raw(1)),
      ImageOwner::Space(id_from_raw(2)),
    );
    let mut cache = ImageCache::default();
    assert_eq!(cache.url(a, "v1"), None);
    assert_eq!(cache.insert(a, "v1".into(), "blob:1".into()), None);
    assert_eq!(cache.url(a, "v1"), Some("blob:1"));
    // A new version is not served from the old picture, and replacing it frees the old URL.
    assert_eq!(cache.url(a, "v2"), None);
    assert_eq!(
      cache.insert(a, "v2".into(), "blob:2".into()),
      Some("blob:1".into())
    );
    assert_eq!(cache.url(a, "v2"), Some("blob:2"));
    assert_eq!(cache.url(b, "v2"), None);
  }

  #[test]
  fn an_older_cached_version_is_shown_until_the_current_one_loads_and_stays_if_it_does_not() {
    let space = ImageOwner::Space(id_from_raw(1));
    let mut cache = ImageCache::default();
    assert_eq!(cache.best_url(space), None);
    cache.insert(space, "v1".into(), "blob:cached".into());
    // The snapshot names v2: v1 isn't current, but it is what the tile shows meanwhile (and during
    // an outage, when v2 doesn't download).
    assert_eq!(cache.url(space, "v2"), None);
    assert_eq!(cache.best_url(space), Some("blob:cached"));
    assert_eq!(
      cache.insert(space, "v2".into(), "blob:new".into()),
      Some("blob:cached".into())
    );
    assert_eq!(cache.url(space, "v2"), Some("blob:new"));
    assert_eq!(
      cache.remove(space),
      Some("blob:new".into()),
      "the picture was removed"
    );
    assert!(!cache.shows(space));
  }

  #[test]
  fn each_owner_has_its_own_cache_entry() {
    let (space, channel) = (id_from_raw(1), id_from_raw(1));
    assert_eq!(ImageOwner::Space(space).media_key(), MediaKey::SpaceImage(space));
    assert_eq!(
      ImageOwner::Channel(channel).media_key(),
      MediaKey::ChannelImage(channel)
    );
  }

  #[test]
  fn a_space_and_a_channel_with_the_same_id_keep_their_own_pictures() {
    let (space, channel) = (
      ImageOwner::Space(id_from_raw(1)),
      ImageOwner::Channel(id_from_raw(1)),
    );
    let mut cache = ImageCache::default();
    cache.insert(space, "v1".into(), "blob:space".into());
    assert_eq!(cache.url(channel, "v1"), None);
    cache.insert(channel, "v1".into(), "blob:channel".into());
    assert_eq!(cache.url(space, "v1"), Some("blob:space"));
    assert_eq!(cache.url(channel, "v1"), Some("blob:channel"));
    assert_eq!(space.path(), "/api/v1/spaces/1/image");
    assert_eq!(channel.path(), "/api/v1/conversations/1/image");
  }

  #[test]
  fn server_refusals_have_their_own_messages() {
    let refused = |code: &str| SpaceImageFailure::Refused(code.into()).i18n_key();
    assert_eq!(refused("payload_too_large"), "space-image-size");
    assert_eq!(refused("forbidden"), "space-image-forbidden");
    assert_eq!(refused("internal"), "space-image-failed");
    assert_eq!(SpaceImageFailure::Network.i18n_key(), "space-image-offline");
  }

  #[test]
  fn failures_keep_the_servers_code_for_the_toast() {
    let refused = SpaceImageFailure::Refused("payload_too_large".into()).to_failure();
    assert_eq!(refused.kind, FailureKind::Server(ErrorCode::PayloadTooLarge));
    let unknown = SpaceImageFailure::Refused("unreadable".into()).to_failure();
    assert_eq!(unknown.kind, FailureKind::Other);
    assert_eq!(SpaceImageFailure::Network.to_failure().kind, FailureKind::Network);
  }
}
