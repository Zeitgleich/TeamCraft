//! Decrypted bytes shown in the page as `blob:` URLs, released as soon as nothing shows them.
//!
//! Full attachments are end-to-end encrypted like everything else: the client core (the desktop shell
//! or the core in the page) downloads and decrypts them, and the page only ever sees the plaintext as
//! a `Blob` in memory. Nothing is written to disk and no plaintext URL exists outside this page.

use wasm_bindgen::JsCast;

use crate::media_formats::is_animated;
use crate::model::{ConversationId, MessageId};
use crate::raster::{Encoding, rasterize};

/// Media type of a blob whose file doesn't claim to be an image: the browser shows nothing for it.
const OPAQUE_TYPE: &str = "application/octet-stream";

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

/// Downloads and decrypts one file of a message and makes it a URL for an `<img>` or a download link.
/// With `maybe_max_side`, a still picture comes as a rendition of at most that many pixels per side
/// (inline previews); the full file is only decoded where it is looked at closely (the lightbox).
pub async fn fetch_attachment(
  conversation: ConversationId,
  message: MessageId,
  file_id: String,
  media_type: &str,
  maybe_max_side: Option<u32>,
) -> Result<ObjectUrl, String> {
  let bytes = fetch_bytes(conversation, message, file_id).await?;
  shown(&bytes, media_type, maybe_max_side).await
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
  let is_picture = media_type.starts_with("image/");
  let header = bytes
    .subarray(0, bytes.length().min(ANIMATION_SNIFF_BYTES))
    .to_vec();
  let is_animated = is_picture && is_animated(&header);
  if let Some(max_side) = maybe_max_side
    && is_picture
    && !is_animated
  {
    let encoding = match media_type.eq_ignore_ascii_case("image/jpeg") {
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

/// Only images and videos get their own type; anything else stays opaque bytes, so a file that lies
/// about its type is never rendered as a document. The page only ever puts these URLs into `<img>`
/// and `<video>`, where an SVG can't run scripts.
fn blob_type(media_type: &str) -> &str {
  match media_type.starts_with("image/") || media_type.starts_with("video/") {
    true => media_type,
    false => OPAQUE_TYPE,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_images_and_videos_keep_their_media_type() {
    assert_eq!(blob_type("image/jpeg"), "image/jpeg");
    assert_eq!(blob_type("video/mp4"), "video/mp4");
    assert_eq!(blob_type("text/html"), OPAQUE_TYPE);
    assert_eq!(blob_type(""), OPAQUE_TYPE);
  }
}
