//! Space pictures: directory data the server owns, like the space's name (e2ee.md §4.4).
//!
//! The hub downloads a picture once per version from `GET /api/v1/spaces/{id}/image` and shows it
//! from a `blob:` URL (the page's CSP only lets images come from `blob:` and `data:`). Uploads are
//! cut to the centered square and scaled to 256×256 here first (teamcraft-core); the server checks
//! and re-encodes them again.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_proto::{ErrorBody, SpaceResponse};

use crate::auth::{ApiMethod, Authenticator, BinaryBody};
use crate::components::file_views::blob_url;
use crate::model::SpaceId;

/// Largest file the picker reads; it is scaled down before the upload (the server has its own
/// limit for what arrives).
pub const MAX_SPACE_IMAGE_PICK_BYTES: f64 = 20.0 * 1024.0 * 1024.0;
/// What the client uploads after scaling.
const UPLOAD_CONTENT_TYPE: &str = "image/png";

#[derive(Clone, Copy)]
pub struct SpaceImageHub {
  auth: StoredValue<Rc<dyn Authenticator>, LocalStorage>,
  cache: RwSignal<ImageCache>,
  asked: StoredValue<HashSet<(SpaceId, String)>>,
}

/// Why a picked file is not uploaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImagePickError {
  /// Not PNG, JPEG or WebP (SVG included: it may carry script).
  Type,
  Size,
  Unreadable,
}

/// The loaded pictures: one `blob:` URL per space, for the version it shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImageCache {
  entries: HashMap<SpaceId, (String, String)>,
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

  /// A `blob:` URL of the space's picture in `maybe_version`, once it has loaded; the first call
  /// for a version starts the download. Reactive.
  pub fn image_url(&self, space: SpaceId, maybe_version: Option<&str>) -> Option<String> {
    let version = maybe_version?;
    let maybe_url = self
      .cache
      .with(|cache| cache.url(space, version).map(str::to_owned));
    if maybe_url.is_none()
      && self
        .asked
        .try_update_value(|asked| asked.insert((space, version.to_owned())))
        == Some(true)
    {
      self.load(space, version.to_owned());
    }
    maybe_url
  }

  /// Uploads an already scaled PNG; the answer carries the new version.
  pub async fn upload(&self, space: SpaceId, png: Vec<u8>) -> Result<SpaceResponse, SpaceImageFailure> {
    let body = BinaryBody {
      content_type: UPLOAD_CONTENT_TYPE.to_owned(),
      bytes: png.clone(),
    };
    let updated = self.request(ApiMethod::Put, space, Some(body)).await?;
    // Shown right away; the space list follows with the gateway's space update.
    if let Some(image) = &updated.maybe_image
      && let Some(url) = blob_url(&png, UPLOAD_CONTENT_TYPE)
    {
      self.remember(space, image.version.clone(), url);
    }
    Ok(updated)
  }

  pub async fn remove(&self, space: SpaceId) -> Result<SpaceResponse, SpaceImageFailure> {
    self.request(ApiMethod::Delete, space, None).await
  }

  async fn request(
    &self,
    method: ApiMethod,
    space: SpaceId,
    maybe_body: Option<BinaryBody>,
  ) -> Result<SpaceResponse, SpaceImageFailure> {
    let path = image_path(space);
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
    serde_json::from_str(&text).map_err(|_| SpaceImageFailure::Refused("unreadable".to_owned()))
  }

  fn load(&self, space: SpaceId, version: String) {
    let hub = *self;
    spawn_local(async move {
      let path = image_path(space);
      let result = hub
        .auth
        .get_value()
        .api_request_bytes(ApiMethod::Get, &path, None)
        .await;
      match result {
        Ok(image) if image.status == 200 => {
          if let Some(url) = blob_url(&image.body, UPLOAD_CONTENT_TYPE) {
            hub.remember(space, version, url);
          }
        },
        // The letter tile stays; the next version (or session) asks again.
        Ok(_) | Err(_) => tracing::debug!(space_id = %space, "space picture not downloaded"),
      }
    });
  }

  fn remember(&self, space: SpaceId, version: String, url: String) {
    let mut maybe_replaced = None;
    self
      .cache
      .update(|cache| maybe_replaced = cache.insert(space, version, url));
    if let Some(replaced) = maybe_replaced {
      revoke(&replaced);
    }
  }
}

impl ImageCache {
  /// The URL for `space` if it shows `version`; an older picture doesn't count.
  pub fn url(&self, space: SpaceId, version: &str) -> Option<&str> {
    self
      .entries
      .get(&space)
      .filter(|(cached, _)| cached == version)
      .map(|(_, url)| url.as_str())
  }

  /// Keeps `url` for `space`; returns the URL it replaced, which the caller releases.
  pub fn insert(&mut self, space: SpaceId, version: String, url: String) -> Option<String> {
    self
      .entries
      .insert(space, (version, url.clone()))
      .map(|(_, previous)| previous)
      .filter(|previous| *previous != url)
  }
}

pub fn use_space_images() -> Option<SpaceImageHub> {
  use_context::<SpaceImageHub>()
}

/// Checks a picked file before it is read: PNG, JPEG or WebP, at most
/// [`MAX_SPACE_IMAGE_PICK_BYTES`].
pub fn check_pick(media_type: &str, size_bytes: f64) -> Result<(), ImagePickError> {
  if !matches!(media_type, "image/png" | "image/jpeg" | "image/webp") {
    return Err(ImagePickError::Type);
  }
  if size_bytes > MAX_SPACE_IMAGE_PICK_BYTES {
    return Err(ImagePickError::Size);
  }
  Ok(())
}

/// The scaled PNG of a picked file's bytes (teamcraft-core, tested there). The UI only runs as wasm
/// (browser and desktop webview); the native build of this crate is for tests and has no core.
pub fn prepare_upload(bytes: &[u8]) -> Result<Vec<u8>, ImagePickError> {
  #[cfg(target_arch = "wasm32")]
  let maybe_png = teamcraft_core::thumbnail::space_image_png(bytes);
  #[cfg(not(target_arch = "wasm32"))]
  let maybe_png: Option<Vec<u8>> = {
    let _ = bytes;
    None
  };
  maybe_png.ok_or(ImagePickError::Unreadable)
}

impl ImagePickError {
  pub fn i18n_key(self) -> &'static str {
    match self {
      Self::Type => "space-image-type",
      Self::Size => "space-image-size",
      Self::Unreadable => "space-image-unreadable",
    }
  }
}

impl SpaceImageFailure {
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

fn image_path(space: SpaceId) -> String {
  format!("/api/v1/spaces/{space}/image")
}

fn revoke(url: &str) {
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
    let (a, b) = (id_from_raw(1), id_from_raw(2));
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
  fn only_raster_images_of_bounded_size_are_picked() {
    assert_eq!(check_pick("image/png", 1000.0), Ok(()));
    assert_eq!(check_pick("image/jpeg", 1000.0), Ok(()));
    assert_eq!(check_pick("image/webp", 1000.0), Ok(()));
    assert_eq!(check_pick("image/svg+xml", 10.0), Err(ImagePickError::Type));
    assert_eq!(check_pick("image/gif", 10.0), Err(ImagePickError::Type));
    assert_eq!(check_pick("", 10.0), Err(ImagePickError::Type));
    assert_eq!(
      check_pick("image/png", MAX_SPACE_IMAGE_PICK_BYTES + 1.0),
      Err(ImagePickError::Size)
    );
    assert_eq!(prepare_upload(b"not an image"), Err(ImagePickError::Unreadable));
  }

  #[test]
  fn server_refusals_have_their_own_messages() {
    let refused = |code: &str| SpaceImageFailure::Refused(code.into()).i18n_key();
    assert_eq!(refused("payload_too_large"), "space-image-size");
    assert_eq!(refused("forbidden"), "space-image-forbidden");
    assert_eq!(refused("internal"), "space-image-failed");
    assert_eq!(SpaceImageFailure::Network.i18n_key(), "space-image-offline");
  }
}
