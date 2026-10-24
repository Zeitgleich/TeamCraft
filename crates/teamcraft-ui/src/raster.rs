//! Pictures drawn by the browser's own decoder into a canvas of a bounded size:
//! - the inline previews in the conversation show a rendition at their display size, so a
//!   48-megapixel photo doesn't sit decoded at full size in memory for every row (issue #316); the
//!   full file is only decoded in the lightbox;
//! - thumbnails of formats the client core can't decode in the page (SVG, AVIF, HEIC; issue #337);
//! - SVG for display. SVG is never shown as itself: a `blob:` URL of an SVG has this page's origin,
//!   and opened as a document ("Open image in new tab") its scripts would run with the user's
//!   session. It is decoded by an `<img>` that is never attached to the page (in that mode it runs
//!   no scripts and loads nothing), drawn into a canvas, and only the resulting PNG is shown.

use std::time::Duration;

use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

use crate::media_formats::{displayable_type, is_svg};

/// Largest SVG the browser is asked to draw; a bigger one shows as a file.
pub const MAX_SVG_BYTES: u32 = 4 * 1024 * 1024;
/// Longest side of an SVG drawn for the lightbox: sharp on a large HiDPI screen.
pub const SVG_DISPLAY_SIDE: u32 = 2048;
/// How long the browser gets to decode a picture before it counts as undecodable.
const DECODE_TIMEOUT: Duration = Duration::from_secs(10);
/// JPEG quality of renditions of photos.
const JPEG_QUALITY: f64 = 0.85;

/// How the canvas is encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
  /// Keeps transparency.
  Png,
  /// Smaller, for photos.
  Jpeg,
}

/// The picture drawn into a canvas whose longer side is at most `max_side`. A raster picture is never
/// enlarged; an SVG is drawn at exactly `max_side`. `None` where the browser can't decode the
/// format, takes too long, or refuses the canvas.
pub async fn rasterize(
  bytes: &js_sys::Uint8Array,
  media_type: &str,
  max_side: u32,
  encoding: Encoding,
) -> Option<web_sys::Blob> {
  let is_vector = is_svg(media_type);
  if is_vector && bytes.length() > MAX_SVG_BYTES {
    return None;
  }
  // An SVG keeps its type only for this off-page decode; any other type goes as the page shows it.
  let source_type = match is_vector {
    true => "image/svg+xml",
    false => displayable_type(media_type)?,
  };
  let source = source_url(bytes, source_type)?;
  let image = web_sys::HtmlImageElement::new().ok()?;
  image.set_src(&source);
  let is_decoded = decode_within(&image, DECODE_TIMEOUT).await;
  let canvas = is_decoded.then(|| draw(&image, max_side, is_vector)).flatten();
  // The decoded picture is in the canvas; the source goes now.
  drop(web_sys::Url::revoke_object_url(&source));
  image.set_src("");
  JsFuture::from(to_blob(&canvas?, encoding)?)
    .await
    .ok()?
    .dyn_into()
    .ok()
}

/// The thumbnail bytes of a picture the client core can't decode (PNG for SVG, which may be
/// transparent; JPEG otherwise), or `None`.
pub async fn app_thumbnail(bytes: &[u8], media_type: &str, max_side: u32) -> Option<Vec<u8>> {
  let encoding = match is_svg(media_type) {
    true => Encoding::Png,
    false => Encoding::Jpeg,
  };
  let blob = rasterize(&js_sys::Uint8Array::from(bytes), media_type, max_side, encoding).await?;
  let buffer = JsFuture::from(blob.array_buffer()).await.ok()?;
  Some(js_sys::Uint8Array::new(&buffer).to_vec())
}

/// A temporary URL of the source bytes for the off-page `<img>`. SVG keeps its type only here.
fn source_url(bytes: &js_sys::Uint8Array, media_type: &str) -> Option<String> {
  let parts = js_sys::Array::of1(bytes);
  let options = web_sys::BlobPropertyBag::new();
  options.set_type(media_type);
  let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &options).ok()?;
  web_sys::Url::create_object_url_with_blob(&blob).ok()
}

/// Waits for the `<img>` to decode, at most `timeout`. An SVG without a size of its own decodes with
/// a natural size of zero in some browsers; [`target_size`] handles that.
async fn decode_within(image: &web_sys::HtmlImageElement, timeout: Duration) -> bool {
  let Some(window) = web_sys::window() else {
    return false;
  };
  let timer = js_sys::Promise::new(&mut |_, reject| {
    let millis = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    drop(window.set_timeout_with_callback_and_timeout_and_arguments_0(&reject, millis));
  });
  let race = js_sys::Promise::race(&js_sys::Array::of2(&image.decode(), &timer));
  JsFuture::from(race).await.is_ok()
}

/// Draws the picture into a new canvas of the target size.
fn draw(
  image: &web_sys::HtmlImageElement,
  max_side: u32,
  is_vector: bool,
) -> Option<web_sys::HtmlCanvasElement> {
  let (width, height) = target_size(image.natural_width(), image.natural_height(), max_side, is_vector)?;
  let canvas = web_sys::window()?
    .document()?
    .create_element("canvas")
    .ok()?
    .dyn_into::<web_sys::HtmlCanvasElement>()
    .ok()?;
  canvas.set_width(width);
  canvas.set_height(height);
  let context = canvas
    .get_context("2d")
    .ok()??
    .dyn_into::<web_sys::CanvasRenderingContext2d>()
    .ok()?;
  context
    .draw_image_with_html_image_element_and_dw_and_dh(image, 0.0, 0.0, f64::from(width), f64::from(height))
    .ok()?;
  Some(canvas)
}

/// The canvas size for a picture of `width` × `height`: a raster picture keeps its size, scaled down to
/// fit `max_side`; a vector picture is drawn at `max_side`, as a square when it has no size of its own.
pub fn target_size(width: u32, height: u32, max_side: u32, is_vector: bool) -> Option<(u32, u32)> {
  if max_side == 0 {
    return None;
  }
  if width == 0 || height == 0 {
    return is_vector.then_some((max_side, max_side));
  }
  let longest = width.max(height);
  let target = match is_vector {
    true => max_side,
    false => longest.min(max_side),
  };
  let scale = |side: u32| {
    let scaled = u64::from(side) * u64::from(target) / u64::from(longest);
    u32::try_from(scaled).unwrap_or(target).clamp(1, target)
  };
  Some((scale(width), scale(height)))
}

fn to_blob(canvas: &web_sys::HtmlCanvasElement, encoding: Encoding) -> Option<js_sys::Promise> {
  let canvas = canvas.clone();
  Some(js_sys::Promise::new(&mut |resolve, reject| {
    let on_reject = reject.clone();
    let done = Closure::once_into_js(move |blob: JsValue| {
      // A canvas too large for the browser, or a tainted one, yields no blob.
      let result = match blob.is_null() {
        true => reject.call0(&JsValue::NULL),
        false => resolve.call1(&JsValue::NULL, &blob),
      };
      drop(result);
    });
    let started = match encoding {
      Encoding::Png => canvas.to_blob(done.unchecked_ref()),
      Encoding::Jpeg => canvas.to_blob_with_type_and_encoder_options(
        done.unchecked_ref(),
        "image/jpeg",
        &JsValue::from_f64(JPEG_QUALITY),
      ),
    };
    if started.is_err() {
      drop(on_reject.call0(&JsValue::NULL));
    }
  }))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn pictures_shrink_to_the_side_and_are_never_enlarged() {
    assert_eq!(target_size(300, 150, 960, false), Some((300, 150)));
    assert_eq!(target_size(8000, 6000, 960, false), Some((960, 720)));
    assert_eq!(target_size(1, 10_000, 960, false), Some((1, 960)));
    assert_eq!(target_size(0, 10, 960, false), None);
  }

  #[test]
  fn vector_pictures_are_drawn_at_the_requested_side() {
    assert_eq!(target_size(300, 150, 640, true), Some((640, 320)));
    assert_eq!(
      target_size(0, 0, 640, true),
      Some((640, 640)),
      "an SVG without a size"
    );
  }
}
