//! Decrypted bytes shown in the page as `blob:` URLs, released as soon as nothing shows them.
//!
//! Full attachments are end-to-end encrypted like everything else: the client core (the desktop shell
//! or the core in the page) downloads and decrypts them, and the page only ever sees the plaintext as
//! a `Blob` in memory. Nothing is written to disk and no plaintext URL exists outside this page.
//! An SVG is only ever shown drawn into a PNG ([`crate::raster`]), never as itself.

use wasm_bindgen::JsCast;

use crate::media_formats::{displayable_type, essence, is_animated, is_svg};
use crate::model::{ConversationId, MessageId};
use crate::raster::{Encoding, SVG_DISPLAY_SIDE, rasterize};

/// Media type of a blob whose file doesn't claim to be an image: the browser shows nothing for it.
const OPAQUE_TYPE: &str = "application/octet-stream";

/// Why a file couldn't be shown.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FetchError {
  /// Not downloaded or decrypted (offline, server trouble): worth trying again.
  #[error("not loaded: {0}")]
  Unavailable(String),
  /// Here, but this browser can't decode it: shown as a file instead.
  #[error("not decodable: {0}")]
  Undecodable(String),
}

/// A `blob:` URL that is revoked when dropped, so the browser frees the bytes behind it.
#[derive(Debug, PartialEq, Eq)]
pub struct ObjectUrl {
  url: String,
  /// Size of the blob behind it, for memory budgets ([`crate::media_cache`]).
  byte_len: u64,
  /// The picture moves (GIF, animated WebP, APNG), whatever its media type says.
  is_animated: bool,
}

/// How much of a file's start is read to tell whether it is an animation.
const ANIMATION_SNIFF_BYTES: u32 = 64 * 1024;

impl ObjectUrl {
  /// A URL for bytes in memory; `None` where the browser refuses.
  pub fn from_bytes(bytes: &[u8], media_type: &str) -> Option<Self> {
    let array = js_sys::Uint8Array::from(bytes);
    Self::from_parts(&array, media_type)
  }

  /// A URL for a thumbnail sent with a file (PNG or JPEG, told apart by its bytes).
  pub fn from_thumbnail(bytes: &[u8]) -> Option<Self> {
    Self::from_bytes(bytes, crate::media_layout::thumbnail_type(bytes))
  }

  /// A URL for a blob the page made itself (a canvas snapshot).
  pub fn from_blob(blob: &web_sys::Blob) -> Option<Self> {
    let url = web_sys::Url::create_object_url_with_blob(blob).ok()?;
    Some(Self {
      url,
      byte_len: blob.size() as u64,
      is_animated: false,
    })
  }

  pub fn is_animated(&self) -> bool {
    self.is_animated
  }

  pub fn as_str(&self) -> &str {
    &self.url
  }

  pub fn byte_len(&self) -> u64 {
    self.byte_len
  }

  fn from_parts(part: &wasm_bindgen::JsValue, media_type: &str) -> Option<Self> {
    let parts = js_sys::Array::of1(part);
    let options = web_sys::BlobPropertyBag::new();
    options.set_type(blob_type(media_type));
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &options).ok()?;
    Self::from_blob(&blob)
  }
}

impl Drop for ObjectUrl {
  fn drop(&mut self) {
    // Revoking only fails for a malformed URL, which `create_object_url` never returns.
    drop(web_sys::Url::revoke_object_url(&self.url));
  }
}

/// Downloads and decrypts one file of a message. [`crate::media_cache`] makes sure one file is
/// fetched only once at a time.
pub async fn fetch_decrypted(
  conversation: ConversationId,
  message: MessageId,
  file_id: String,
) -> Result<js_sys::Uint8Array, FetchError> {
  fetch_bytes(conversation, message, file_id)
    .await
    .map_err(FetchError::Unavailable)
}

/// Makes decrypted bytes a URL for an `<img>` or a download link. With `maybe_max_side`, a still
/// picture comes as a rendition of at most that many pixels per side (inline previews); the full file
/// is only decoded where it is looked at closely (the lightbox).
pub async fn url_for(
  bytes: &js_sys::Uint8Array,
  media_type: &str,
  maybe_max_side: Option<u32>,
) -> Result<ObjectUrl, FetchError> {
  shown(bytes, media_type, maybe_max_side)
    .await
    .map_err(FetchError::Undecodable)
}

/// The decrypted bytes, from the demo data, the desktop shell or the core in the page.
async fn fetch_bytes(
  conversation: ConversationId,
  message: MessageId,
  file_id: String,
) -> Result<js_sys::Uint8Array, String> {
  #[cfg(feature = "demo")]
  if crate::settings::BackendMode::load() == crate::settings::BackendMode::Mock {
    let bytes = crate::mock::files::demo_bytes(&file_id).ok_or_else(|| "no such demo file".to_owned())?;
    return Ok(js_sys::Uint8Array::from(bytes));
  }
  if crate::auth::tauri::is_available() {
    return fetch_from_shell(conversation, message, file_id).await;
  }
  fetch_from_page_core(conversation, message, file_id).await
}

/// A URL the page shows for decrypted bytes: a still picture scaled down when asked to, anything
/// else as it is, marked when it moves.
async fn shown(
  bytes: &js_sys::Uint8Array,
  media_type: &str,
  maybe_max_side: Option<u32>,
) -> Result<ObjectUrl, String> {
  if is_svg(media_type) {
    let side = maybe_max_side.unwrap_or(SVG_DISPLAY_SIDE);
    let blob = rasterize(bytes, media_type, side, Encoding::Png)
      .await
      .ok_or_else(|| "the browser could not draw the SVG".to_owned())?;
    return ObjectUrl::from_blob(&blob).ok_or_else(refused);
  }
  let is_picture = essence(media_type).starts_with("image/");
  let header = bytes
    .subarray(0, bytes.length().min(ANIMATION_SNIFF_BYTES))
    .to_vec();
  let is_animated = is_picture && is_animated(&header);
  if let Some(max_side) = maybe_max_side
    && is_picture
    && !is_animated
  {
    let encoding = match essence(media_type) == "image/jpeg" {
      true => Encoding::Jpeg,
      false => Encoding::Png,
    };
    let blob = rasterize(bytes, blob_type(media_type), max_side, encoding)
      .await
      .ok_or_else(|| "the browser could not decode the picture".to_owned())?;
    return ObjectUrl::from_blob(&blob).ok_or_else(refused);
  }
  let mut url = ObjectUrl::from_parts(bytes, media_type).ok_or_else(refused)?;
  url.is_animated = is_animated;
  Ok(url)
}

/// The shell answers with the raw bytes as an `ArrayBuffer`.
async fn fetch_from_shell(
  conversation: ConversationId,
  message: MessageId,
  file_id: String,
) -> Result<js_sys::Uint8Array, String> {
  let args = serde_json::json!({ "conversation": conversation, "message": message, "file_id": file_id });
  let js_args =
    js_sys::JSON::parse(&args.to_string()).map_err(|_| "could not encode the arguments".to_owned())?;
  let answer = crate::auth::tauri::invoke("core_attachment", js_args)
    .await
    .map_err(|error| error.to_string())?;
  if !answer.is_instance_of::<js_sys::ArrayBuffer>() {
    return Err("unreadable answer of core_attachment".to_owned());
  }
  Ok(js_sys::Uint8Array::new(&answer))
}

#[cfg(target_arch = "wasm32")]
async fn fetch_from_page_core(
  conversation: ConversationId,
  message: MessageId,
  file_id: String,
) -> Result<js_sys::Uint8Array, String> {
  let plaintext = crate::web_core::attachment(conversation, message, file_id).await?;
  Ok(js_sys::Uint8Array::from(plaintext.expose_secret()))
}

#[cfg(not(target_arch = "wasm32"))]
async fn fetch_from_page_core(
  _conversation: ConversationId,
  _message: MessageId,
  _file_id: String,
) -> Result<js_sys::Uint8Array, String> {
  Err("files need the desktop app or a browser".to_owned())
}

fn refused() -> String {
  "the browser refused the file".to_owned()
}

/// Only allowlisted pictures and videos get their own type; anything else stays opaque bytes, so a
/// file that lies about its type is never rendered as a document. SVG is a document too (its scripts
/// would run if the URL were opened in a tab): it stays opaque and is shown drawn into a PNG instead.
fn blob_type(media_type: &str) -> &'static str {
  displayable_type(media_type).unwrap_or(OPAQUE_TYPE)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_images_and_videos_keep_their_media_type() {
    assert_eq!(blob_type("image/jpeg"), "image/jpeg");
    assert_eq!(blob_type("video/mp4"), "video/mp4");
    assert_eq!(blob_type("text/html"), OPAQUE_TYPE);
    assert_eq!(
      blob_type("image/svg+xml"),
      OPAQUE_TYPE,
      "SVG is never a blob of its own type"
    );
    assert_eq!(blob_type("IMAGE/SVG+XML"), OPAQUE_TYPE);
    assert_eq!(
      blob_type("image/svg+xml; charset=utf-8"),
      OPAQUE_TYPE,
      "parameters don't sneak SVG in"
    );
    assert_eq!(blob_type("text/html; charset=utf-8"), OPAQUE_TYPE);
    assert_eq!(
      blob_type("image/x-made-up"),
      OPAQUE_TYPE,
      "unknown image types stay opaque"
    );
    assert_eq!(blob_type("Image/JPEG; q=1"), "image/jpeg");
    assert_eq!(blob_type(""), OPAQUE_TYPE);
  }
}
