//! Space pictures. Like the space's name they are directory data the server sees, not end-to-end
//! encrypted content (e2ee.md §4.4), so the server receives the bytes itself, like brand images.
//!
//! An upload is decoded and re-encoded as PNG here: what the server stores and serves is a plain
//! raster image of known size, without script (SVG is refused), metadata or a second file format
//! hidden inside. Only people who may change the space's properties set or remove it; everybody who
//! may see the space may download it.

use std::sync::Arc;

use axum::body::Bytes;
use image::{ImageFormat, ImageReader, Limits};
use serde_json::json;
use teamcraft_db::StoredSpaceImage;
use teamcraft_proto::{AttachmentId, GatewayEvent, Id, Permission, SpaceId, SpaceResponse};
use tracing::warn;

use super::access::{load_space, require};
use super::audit::{self, Target as AuditTarget};
use super::spaces::{get_space, notify_space};
use crate::auth::AuthenticatedUser;
use crate::brand::sniff_image;
use crate::error::ApiError;
use crate::state::AppState;
use crate::storage::BlobStore;

/// Longest side of a stored picture; larger uploads are scaled down (clients send 256×256).
pub const MAX_SPACE_IMAGE_SIDE: u32 = 512;
/// The only type the server stores and serves.
pub const SPACE_IMAGE_CONTENT_TYPE: &str = "image/png";
/// Uploads with more pixels than this are not decoded (decompression bombs).
const MAX_SOURCE_PIXELS: u64 = 4096 * 4096;
/// Memory the decoder may allocate.
const MAX_DECODER_BYTES: u64 = 128 * 1024 * 1024;

/// A stored picture as the download handler serves it.
pub struct SpaceImageFile {
  pub bytes: Vec<u8>,
  /// The `ETag` without quotes.
  pub version: String,
}

/// Why an upload is not a picture the server keeps.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpaceImageError {
  #[error("SVG pictures are not accepted; upload a PNG, JPEG or WebP image")]
  Svg,
  #[error("space pictures are PNG, JPEG or WebP images of the declared type")]
  UnsupportedType,
  #[error("the picture could not be decoded")]
  Undecodable,
  #[error("the picture has too many pixels")]
  TooManyPixels,
}

/// Replaces the space's picture with the uploaded image.
pub async fn put_space_image(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: SpaceId,
  declared_type: &str,
  bytes: Bytes,
) -> Result<SpaceResponse, ApiError> {
  let access = load_space(state, caller, id).await?;
  require(state, caller, Permission::ManageSpaceProperties, &access.target())?;
  let max_bytes = state.settings.get().limits.max_space_image_bytes;
  if bytes.is_empty() || bytes.len() as u64 > max_bytes {
    return Err(ApiError::PayloadTooLarge(format!(
      "space pictures are 1 to {max_bytes} bytes"
    )));
  }
  let declared_type = declared_type.to_owned();
  let png = tokio::task::spawn_blocking(move || sanitize(&bytes, &declared_type))
    .await
    .map_err(|error| ApiError::Internal(format!("re-encoding the picture failed: {error}")))?
    .map_err(|error| ApiError::InvalidRequest(error.to_string()))?;
  if png.len() as u64 > max_bytes {
    return Err(ApiError::PayloadTooLarge(format!(
      "the re-encoded picture is larger than {max_bytes} bytes"
    )));
  }
  let store = blob_store(state)?;
  // A fresh key per picture: a reader never gets a half-replaced object.
  let object: AttachmentId = state.next_id()?;
  let image = StoredSpaceImage {
    key: format!("space-images/{}/{}", id.snowflake(), object.snowflake()),
    version: teamcraft_crypto::account::subresource_integrity(&png),
    size_bytes: png.len() as u64,
  };
  store
    .put_object(&image.key, png, SPACE_IMAGE_CONTENT_TYPE)
    .await
    .map_err(|error| ApiError::Unavailable(format!("storing the picture failed: {error}")))?;
  let replaced = match state.database.set_space_image(id, Some(&image)).await {
    Ok(replaced) => replaced,
    Err(error) => {
      forget_object(store, &image.key).await;
      return Err(error.into());
    },
  };
  if let Some(previous) = replaced {
    forget_object(store, &previous).await;
  }
  announce(state, caller, id, "space_image_set").await?;
  get_space(state, caller, id).await
}

/// Removes the space's picture; the rail shows the letter tile again.
pub async fn delete_space_image(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: SpaceId,
) -> Result<SpaceResponse, ApiError> {
  let access = load_space(state, caller, id).await?;
  require(state, caller, Permission::ManageSpaceProperties, &access.target())?;
  if access.view.space.maybe_image.is_none() {
    return Err(ApiError::space_image_not_found());
  }
  let store = blob_store(state)?;
  if let Some(previous) = state.database.set_space_image(id, None).await? {
    forget_object(store, &previous).await;
  }
  announce(state, caller, id, "space_image_removed").await?;
  get_space(state, caller, id).await
}

/// The picture of a space the caller may see.
pub async fn get_space_image(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: SpaceId,
) -> Result<SpaceImageFile, ApiError> {
  let access = load_space(state, caller, id).await?;
  let image = access
    .view
    .space
    .maybe_image
    .ok_or_else(ApiError::space_image_not_found)?;
  let store = blob_store(state)?;
  let bytes = store
    .get_object(&image.key, image.size_bytes)
    .await
    .map_err(|error| {
      warn!(%error, space_id = %id, "reading a space picture failed");
      ApiError::Unavailable("picture storage".into())
    })?
    .ok_or_else(ApiError::space_image_not_found)?;
  Ok(SpaceImageFile {
    bytes,
    version: image.version,
  })
}

/// Checks that `bytes` are a PNG, JPEG or WebP image of the declared type and re-encodes them as a
/// PNG of at most [`MAX_SPACE_IMAGE_SIDE`] pixels per side.
pub fn sanitize(bytes: &[u8], declared_type: &str) -> Result<Vec<u8>, SpaceImageError> {
  let format = match sniff_image(bytes, declared_type) {
    Some("image/png") => ImageFormat::Png,
    Some("image/jpeg") => ImageFormat::Jpeg,
    Some("image/webp") => ImageFormat::WebP,
    Some("image/svg+xml") => return Err(SpaceImageError::Svg),
    _ => return Err(SpaceImageError::UnsupportedType),
  };
  let (width, height) = reader(bytes, format)
    .into_dimensions()
    .map_err(|_| SpaceImageError::Undecodable)?;
  if u64::from(width) * u64::from(height) > MAX_SOURCE_PIXELS {
    return Err(SpaceImageError::TooManyPixels);
  }
  let mut picture = reader(bytes, format)
    .decode()
    .map_err(|_| SpaceImageError::Undecodable)?;
  if width > MAX_SPACE_IMAGE_SIDE || height > MAX_SPACE_IMAGE_SIDE {
    picture = picture.thumbnail(MAX_SPACE_IMAGE_SIDE, MAX_SPACE_IMAGE_SIDE);
  }
  let mut out = std::io::Cursor::new(Vec::new());
  picture
    .write_to(&mut out, ImageFormat::Png)
    .map_err(|_| SpaceImageError::Undecodable)?;
  Ok(out.into_inner())
}

fn reader(bytes: &[u8], format: ImageFormat) -> ImageReader<std::io::Cursor<&[u8]>> {
  let mut reader = ImageReader::with_format(std::io::Cursor::new(bytes), format);
  let mut limits = Limits::default();
  limits.max_alloc = Some(MAX_DECODER_BYTES);
  reader.limits(limits);
  reader
}

async fn announce(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: SpaceId,
  action: &str,
) -> Result<(), ApiError> {
  audit::record(
    state,
    Some(caller.user.id),
    action,
    Some(AuditTarget::new("space", id)),
    &json!({}),
  )
  .await?;
  notify_space(state, id, &GatewayEvent::SpaceUpdate { space_id: id }).await
}

fn blob_store(state: &AppState) -> Result<&Arc<BlobStore>, ApiError> {
  state
    .maybe_blob_store
    .as_ref()
    .ok_or_else(|| ApiError::Unavailable("storage is not configured".into()))
}

/// Deletes a picture nobody refers to any more; a failure only leaves an orphan behind.
async fn forget_object(store: &BlobStore, key: &str) {
  if let Err(error) = store.delete_object(key).await {
    warn!(%error, "deleting an old space picture failed");
  }
}

#[cfg(test)]
mod tests {
  use image::{ImageBuffer, Rgb, Rgba};

  use super::*;

  #[test]
  fn raster_images_are_re_encoded_as_png() {
    for (format, declared) in [
      (ImageFormat::Png, "image/png"),
      (ImageFormat::Jpeg, "image/jpeg"),
      (ImageFormat::WebP, "image/webp"),
    ] {
      let bytes = encoded(64, 32, format);
      let png = sanitize(&bytes, declared).unwrap();
      assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"), "{declared}");
      let decoded = image::load_from_memory(&png).unwrap();
      assert_eq!((decoded.width(), decoded.height()), (64, 32), "{declared}");
    }
  }

  #[test]
  fn large_pictures_are_scaled_down() {
    let png = sanitize(&encoded(2000, 1000, ImageFormat::Png), "image/png").unwrap();
    let decoded = image::load_from_memory(&png).unwrap();
    assert_eq!(
      (decoded.width(), decoded.height()),
      (MAX_SPACE_IMAGE_SIDE, MAX_SPACE_IMAGE_SIDE / 2)
    );
  }

  #[test]
  fn svg_is_refused_even_when_declared() {
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#;
    assert_eq!(sanitize(svg, "image/svg+xml"), Err(SpaceImageError::Svg));
    assert_eq!(sanitize(svg, "image/png"), Err(SpaceImageError::UnsupportedType));
  }

  #[test]
  fn other_and_mislabelled_files_are_refused() {
    let png = encoded(8, 8, ImageFormat::Png);
    assert_eq!(
      sanitize(&png, "image/jpeg"),
      Err(SpaceImageError::UnsupportedType)
    );
    assert_eq!(
      sanitize(b"GIF89a....", "image/gif"),
      Err(SpaceImageError::UnsupportedType)
    );
    assert_eq!(
      sanitize(b"<html></html>", "text/html"),
      Err(SpaceImageError::UnsupportedType)
    );
    assert_eq!(
      sanitize(&[0, 0, 1, 0, 1], "image/x-icon"),
      Err(SpaceImageError::UnsupportedType)
    );
  }

  #[test]
  fn truncated_images_are_refused() {
    let png = encoded(32, 32, ImageFormat::Png);
    assert_eq!(
      sanitize(&png[..png.len() / 2], "image/png"),
      Err(SpaceImageError::Undecodable)
    );
    assert_eq!(
      sanitize(b"\x89PNG\r\n\x1a\n", "image/png"),
      Err(SpaceImageError::Undecodable)
    );
  }

  #[test]
  fn a_png_claiming_huge_dimensions_is_not_decoded() {
    // A valid header announcing 20000×20000 pixels; the body is never reached.
    let mut png = encoded(1, 1, ImageFormat::Png);
    png[16..20].copy_from_slice(&20_000u32.to_be_bytes());
    png[20..24].copy_from_slice(&20_000u32.to_be_bytes());
    let result = sanitize(&png, "image/png");
    assert!(
      matches!(
        result,
        Err(SpaceImageError::TooManyPixels | SpaceImageError::Undecodable)
      ),
      "{result:?}"
    );
  }

  fn encoded(width: u32, height: u32, format: ImageFormat) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    if format == ImageFormat::Jpeg {
      ImageBuffer::from_fn(width, height, |x, _| Rgb([(x % 256) as u8, 40, 80]))
        .write_to(&mut out, format)
        .unwrap();
    } else {
      ImageBuffer::from_fn(width, height, |x, _| Rgba([(x % 256) as u8, 40, 80, 255]))
        .write_to(&mut out, format)
        .unwrap();
    }
    out.into_inner()
  }
}
