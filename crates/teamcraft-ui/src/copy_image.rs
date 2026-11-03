//! "Copy image" (#719): the picture itself goes on the clipboard, as PNG (the type every platform
//! and browser accepts) and, where the platform clipboard takes more, also as the original file.
//!
//! - Browsers, macOS (WKWebView) and Windows (WebView2): the web Clipboard API with a
//!   `ClipboardItem` whose PNG is a promise. The write starts inside the click (Safari allows it
//!   only then); the file is fetched and converted while the promise runs.
//! - Linux (WebKitGTK): its Clipboard API doesn't write pictures reliably, so the shell puts them
//!   on the GTK clipboard (X11 and Wayland alike), PNG plus the original (apps/desktop/src/clipboard.rs).
//!
//! Other formats (SVG, WebP, GIF, AVIF, HEIC) are drawn into a canvas by the webview's own decoder
//! and copied as PNG; an SVG never goes on the clipboard as itself (its scripts would run where it
//! is pasted as a document).

use futures_util::future::LocalBoxFuture;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

use teamcraft_client_api::remote::AttachmentPriority;

use crate::auth::tauri;
use crate::media_formats::{displayable_type, essence, is_svg};
use crate::model::{ConversationId, MessageId};
use crate::object_url::fetch_decrypted;
use crate::raster::{Encoding, rasterize};

/// Longest side of a picture copied as PNG; larger ones are scaled down (a raster picture is never
/// enlarged, an SVG is drawn at this size).
pub const COPY_MAX_SIDE: u32 = 4096;
/// Header of the shell's `clipboard_write_image`: how many bytes of the body are the PNG.
pub const PNG_LENGTH_HEADER: &str = "x-png-bytes";
/// Header naming the original's media type, whose bytes follow the PNG in the body.
pub const ORIGINAL_TYPE_HEADER: &str = "x-original-type";

/// Where a copied picture goes to the clipboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyRoute {
  /// The web Clipboard API of the page.
  WebClipboard,
  /// The desktop shell's clipboard (Linux).
  Shell,
}

impl CopyRoute {
  /// The route of this page: the shell on the Linux desktop app, the web API everywhere else.
  pub fn current() -> Self {
    let platform = web_sys::window()
      .and_then(|window| window.navigator().platform().ok())
      .unwrap_or_default();
    Self::for_platform(tauri::is_available(), &platform)
  }

  /// `platform` is `navigator.platform` ("Linux x86_64", "MacIntel", "Win32").
  pub fn for_platform(is_desktop_app: bool, platform: &str) -> Self {
    match is_desktop_app && platform.to_ascii_lowercase().contains("linux") {
      true => Self::Shell,
      false => Self::WebClipboard,
    }
  }
}

/// A picture to copy: one file of a message.
#[derive(Clone, Debug)]
pub struct CopiedPicture {
  pub conversation: ConversationId,
  pub message: MessageId,
  pub file_id: String,
  pub media_type: String,
}

/// Whether "Copy image" is offered for a file of `media_type`: pictures the webview can draw.
/// Videos and other files aren't copied (their decrypted bytes would have to be written to disk).
pub fn can_copy(media_type: &str) -> bool {
  let is_drawable_picture = displayable_type(media_type).is_some_and(|shown| shown.starts_with("image/"));
  is_drawable_picture || is_svg(media_type)
}

/// The original's media type the shell offers beside the PNG: raster pictures other than PNG; never
/// SVG (see the module docs).
pub fn original_type_for_clipboard(media_type: &str) -> Option<String> {
  let essence = essence(media_type);
  let is_offered = essence.starts_with("image/") && essence != "image/png" && !is_svg(media_type);
  is_offered.then_some(essence)
}

/// Copies the picture; the future answers `true` once it is on the clipboard. Call it from the
/// click handler itself: on the web route the write starts right here, before anything awaits.
pub fn copy(picture: CopiedPicture) -> LocalBoxFuture<'static, bool> {
  match CopyRoute::current() {
    CopyRoute::WebClipboard => {
      let maybe_write = web_write(picture);
      Box::pin(async move {
        match maybe_write {
          Some(write) => JsFuture::from(write).await.is_ok(),
          None => false,
        }
      })
    },
    CopyRoute::Shell => Box::pin(async move {
      match shell_write(&picture).await {
        Ok(()) => true,
        Err(error) => {
          tracing::warn!(%error, "could not copy the picture");
          false
        },
      }
    }),
  }
}

/// `navigator.clipboard.write([new ClipboardItem({"image/png": promise})])`, started at once.
/// web-sys keeps the API unstable, so it is reached through `Reflect`.
fn web_write(picture: CopiedPicture) -> Option<js_sys::Promise> {
  let window = web_sys::window()?;
  let png = wasm_bindgen_futures::future_to_promise(async move {
    let bytes = fetch(&picture).await.map_err(|error| JsValue::from_str(&error))?;
    let blob = png_blob(&bytes, &picture.media_type)
      .await
      .ok_or_else(|| JsValue::from_str("the picture could not be converted"))?;
    Ok(blob.into())
  });
  let constructor = js_sys::Reflect::get(&window, &"ClipboardItem".into())
    .ok()?
    .dyn_into::<js_sys::Function>()
    .ok()?;
  let parts = js_sys::Object::new();
  js_sys::Reflect::set(&parts, &"image/png".into(), &png).ok()?;
  let item = js_sys::Reflect::construct(&constructor, &js_sys::Array::of1(&parts)).ok()?;
  let clipboard = js_sys::Reflect::get(&window.navigator(), &"clipboard".into()).ok()?;
  let write = js_sys::Reflect::get(&clipboard, &"write".into())
    .ok()?
    .dyn_into::<js_sys::Function>()
    .ok()?;
  write
    .call1(&clipboard, &js_sys::Array::of1(&item))
    .ok()?
    .dyn_into::<js_sys::Promise>()
    .ok()
}

/// Hands the PNG (and the original, where it is one the shell offers) to the shell's clipboard.
async fn shell_write(picture: &CopiedPicture) -> Result<(), String> {
  let bytes = fetch(picture).await?;
  let blob = png_blob(&bytes, &picture.media_type)
    .await
    .ok_or_else(|| "the picture could not be converted".to_owned())?;
  let buffer = JsFuture::from(blob.array_buffer())
    .await
    .map_err(|_| "the converted picture could not be read".to_owned())?;
  let png = js_sys::Uint8Array::new(&buffer);
  let maybe_original_type = original_type_for_clipboard(&picture.media_type);
  let body = match maybe_original_type {
    Some(_) => concat(&png, &bytes),
    None => png.clone(),
  };
  let mut headers = vec![(PNG_LENGTH_HEADER, png.length().to_string())];
  if let Some(original_type) = maybe_original_type {
    headers.push((ORIGINAL_TYPE_HEADER, original_type));
  }
  tauri::invoke_raw("clipboard_write_image", &body, &headers)
    .await
    .map(|_| ())
}

async fn fetch(picture: &CopiedPicture) -> Result<js_sys::Uint8Array, String> {
  fetch_decrypted(
    picture.conversation,
    picture.message,
    picture.file_id.clone(),
    AttachmentPriority::Now,
  )
  .await
  .map_err(|error| error.to_string())
}

/// The picture as a PNG blob: a PNG as it is, anything else drawn by the webview's decoder.
async fn png_blob(bytes: &js_sys::Uint8Array, media_type: &str) -> Option<web_sys::Blob> {
  if essence(media_type) == "image/png" {
    let options = web_sys::BlobPropertyBag::new();
    options.set_type("image/png");
    return web_sys::Blob::new_with_u8_array_sequence_and_options(&js_sys::Array::of1(bytes), &options).ok();
  }
  rasterize(bytes, media_type, COPY_MAX_SIDE, Encoding::Png).await
}

fn concat(first: &js_sys::Uint8Array, second: &js_sys::Uint8Array) -> js_sys::Uint8Array {
  let joined = js_sys::Uint8Array::new_with_length(first.length() + second.length());
  joined.set(first, 0);
  joined.set(second, first.length());
  joined
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_the_linux_desktop_app_copies_through_the_shell() {
    assert_eq!(CopyRoute::for_platform(true, "Linux x86_64"), CopyRoute::Shell);
    assert_eq!(CopyRoute::for_platform(true, "Linux aarch64"), CopyRoute::Shell);
    assert_eq!(CopyRoute::for_platform(true, "MacIntel"), CopyRoute::WebClipboard);
    assert_eq!(CopyRoute::for_platform(true, "Win32"), CopyRoute::WebClipboard);
    // A browser on Linux has a working Clipboard API.
    assert_eq!(
      CopyRoute::for_platform(false, "Linux x86_64"),
      CopyRoute::WebClipboard
    );
  }

  #[test]
  fn pictures_are_copied_other_files_are_not() {
    for picture in [
      "image/png",
      "image/jpeg",
      "image/gif",
      "image/webp",
      "image/svg+xml",
      "IMAGE/JPEG; q=1",
    ] {
      assert!(can_copy(picture), "{picture}");
    }
    for other in [
      "video/mp4",
      "application/pdf",
      "text/plain",
      "application/octet-stream",
    ] {
      assert!(!can_copy(other), "{other}");
    }
  }

  #[test]
  fn the_original_goes_along_except_png_and_svg() {
    assert_eq!(
      original_type_for_clipboard("image/jpeg"),
      Some("image/jpeg".to_owned())
    );
    assert_eq!(
      original_type_for_clipboard("image/gif"),
      Some("image/gif".to_owned())
    );
    assert_eq!(
      original_type_for_clipboard("image/png"),
      None,
      "the PNG is the original"
    );
    assert_eq!(original_type_for_clipboard("image/svg+xml"), None);
    assert_eq!(original_type_for_clipboard("video/mp4"), None);
  }
}
