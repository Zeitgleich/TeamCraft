//! Decrypted bytes shown in the page as `blob:` URLs, released as soon as nothing shows them.
//!
//! Full attachments are end-to-end encrypted like everything else: the client core (the desktop shell
//! or the core in the page) downloads and decrypts them, and the page only ever sees the plaintext as
//! a `Blob` in memory. Nothing is written to disk and no plaintext URL exists outside this page.

use wasm_bindgen::JsCast;

use crate::model::{ConversationId, MessageId};

/// Media type of a blob whose file doesn't claim to be an image: the browser shows nothing for it.
const OPAQUE_TYPE: &str = "application/octet-stream";

/// A `blob:` URL that is revoked when dropped, so the browser frees the bytes behind it.
#[derive(Debug, PartialEq, Eq)]
pub struct ObjectUrl(String);

impl ObjectUrl {
  /// A URL for bytes in memory; `None` where the browser refuses.
  pub fn from_bytes(bytes: &[u8], media_type: &str) -> Option<Self> {
    let array = js_sys::Uint8Array::from(bytes);
    Self::from_parts(&array, media_type)
  }

  /// A URL for a blob the page made itself (a canvas snapshot).
  pub fn from_blob(blob: &web_sys::Blob) -> Option<Self> {
    web_sys::Url::create_object_url_with_blob(blob).ok().map(Self)
  }

  pub fn as_str(&self) -> &str {
    &self.0
  }

  fn from_parts(part: &wasm_bindgen::JsValue, media_type: &str) -> Option<Self> {
    let parts = js_sys::Array::of1(part);
    let options = web_sys::BlobPropertyBag::new();
    options.set_type(blob_type(media_type));
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &options).ok()?;
    web_sys::Url::create_object_url_with_blob(&blob).ok().map(Self)
  }
}

impl Drop for ObjectUrl {
  fn drop(&mut self) {
    // Revoking only fails for a malformed URL, which `create_object_url` never returns.
    drop(web_sys::Url::revoke_object_url(&self.0));
  }
}

/// Downloads and decrypts one file of a message and makes it a URL for an `<img>` or a download link.
pub async fn fetch_attachment(
  conversation: ConversationId,
  message: MessageId,
  file_id: String,
  media_type: &str,
) -> Result<ObjectUrl, String> {
  #[cfg(feature = "demo")]
  if crate::settings::BackendMode::load() == crate::settings::BackendMode::Mock {
    let bytes = crate::mock::files::demo_bytes(&file_id).ok_or_else(|| "no such demo file".to_owned())?;
    return ObjectUrl::from_bytes(bytes, media_type).ok_or_else(refused);
  }
  if crate::auth::tauri::is_available() {
    return fetch_from_shell(conversation, message, file_id, media_type).await;
  }
  fetch_from_page_core(conversation, message, file_id, media_type).await
}

/// The shell answers with the raw bytes as an `ArrayBuffer`, which goes into the blob as it is.
async fn fetch_from_shell(
  conversation: ConversationId,
  message: MessageId,
  file_id: String,
  media_type: &str,
) -> Result<ObjectUrl, String> {
  let args = serde_json::json!({ "conversation": conversation, "message": message, "file_id": file_id });
  let js_args =
    js_sys::JSON::parse(&args.to_string()).map_err(|_| "could not encode the arguments".to_owned())?;
  let answer = crate::auth::tauri::invoke("core_attachment", js_args)
    .await
    .map_err(|error| error.to_string())?;
  if !answer.is_instance_of::<js_sys::ArrayBuffer>() {
    return Err("unreadable answer of core_attachment".to_owned());
  }
  ObjectUrl::from_parts(&answer, media_type).ok_or_else(refused)
}

#[cfg(target_arch = "wasm32")]
async fn fetch_from_page_core(
  conversation: ConversationId,
  message: MessageId,
  file_id: String,
  media_type: &str,
) -> Result<ObjectUrl, String> {
  let plaintext = crate::web_core::attachment(conversation, message, file_id).await?;
  ObjectUrl::from_bytes(plaintext.expose_secret(), media_type).ok_or_else(refused)
}

#[cfg(not(target_arch = "wasm32"))]
async fn fetch_from_page_core(
  _conversation: ConversationId,
  _message: MessageId,
  _file_id: String,
  _media_type: &str,
) -> Result<ObjectUrl, String> {
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
