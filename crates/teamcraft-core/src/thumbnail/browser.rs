//! The browser's [`PictureCodec`] (#135): in the web app the engine makes its thumbnails and the
//! problem report's screenshot with the browser's own decoders and a canvas, so the web app carries
//! no image codecs (the `image` crate with its decoders was about 0.2 MB gzip of every first visit).
//! The contract is the native one (`native.rs`): the same sizes, steps and limits, upright, and
//! `None` instead of an error for anything it can't do.
//!
//! - Only the formats the native codec reads are decoded (PNG, JPEG, GIF, WebP, BMP, ICO), told by
//!   their bytes, not by the media type the file claims. The rest (SVG, AVIF, HEIC, …) the web app
//!   draws itself (teamcraft-ui `raster::app_thumbnail`), as before.
//! - A picture over [`MAX_PIXELS`] is refused from its header, before the browser decodes it.
//! - The `<img>` that decodes is never attached to the page; the browser turns the picture by its
//!   EXIF orientation as it draws it (`image-orientation: from-image`, every current browser).
//! - It needs a document (a page); in a service worker every picture is `None`.
//! - The web app's crop dialog uses [`decoded`] and [`encode`] too (teamcraft-ui `image_crop.rs`).

use std::time::Duration;

use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

use super::{
  CodecFuture, JPEG_STEPS, MAX_PIXELS, MAX_THUMBNAIL_BYTES, PNG_STEPS, PictureCodec, SCREENSHOT_JPEG_QUALITY,
  essence, fitted_size,
};
use crate::image_header;

/// How long the browser gets to decode a picture before it counts as undecodable.
const DECODE_TIMEOUT: Duration = Duration::from_secs(10);

/// The [`PictureCodec`] of the browser: what [`super::make_thumbnail`] uses there.
#[derive(Clone, Copy, Debug, Default)]
pub struct BrowserCodec;

impl PictureCodec for BrowserCodec {
  fn thumbnail<'a>(&'a self, bytes: &'a [u8], media_type: &'a str) -> CodecFuture<'a, Option<Vec<u8>>> {
    Box::pin(thumbnail(bytes, media_type))
  }

  fn without_metadata<'a>(&'a self, bytes: &'a [u8]) -> CodecFuture<'a, Option<(Vec<u8>, &'static str)>> {
    Box::pin(without_metadata(bytes))
  }
}

/// How a canvas is encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
  Png,
  /// JPEG at this quality, in percent.
  Jpeg(u8),
}

/// A picture the browser decoded, upright, with its size; its `blob:` URL goes when it is dropped.
pub struct Decoded {
  image: web_sys::HtmlImageElement,
  source: String,
  /// The format, read from the bytes.
  pub media_type: &'static str,
  pub width: u32,
  pub height: u32,
}

impl Drop for Decoded {
  fn drop(&mut self) {
    drop(web_sys::Url::revoke_object_url(&self.source));
    self.image.set_src("");
  }
}

impl Decoded {
  /// The decoded picture, to draw from.
  pub fn image(&self) -> &web_sys::HtmlImageElement {
    &self.image
  }

  /// The picture scaled to at most `side` pixels on its longer side ([`fitted_size`]).
  pub fn fitted(&self, side: u32) -> Option<web_sys::HtmlCanvasElement> {
    let (width, height) = fitted_size(self.width, self.height, side);
    let (canvas, context) = canvas(width, height)?;
    context
      .draw_image_with_html_image_element_and_dw_and_dh(
        &self.image,
        0.0,
        0.0,
        f64::from(width),
        f64::from(height),
      )
      .ok()?;
    Some(canvas)
  }
}

/// Decodes a PNG, JPEG, GIF, WebP, BMP or ICO picture within [`MAX_PIXELS`]; `None` for anything
/// else, a picture the browser can't decode in time, or one without a size.
pub async fn decoded(bytes: &[u8]) -> Option<Decoded> {
  let media_type = image_header::media_type(bytes)?;
  let (width, height) = image_header::upright_size(bytes)?;
  if u64::from(width) * u64::from(height) > MAX_PIXELS {
    return None;
  }
  let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
  let options = web_sys::BlobPropertyBag::new();
  options.set_type(media_type);
  let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &options).ok()?;
  let source = web_sys::Url::create_object_url_with_blob(&blob).ok()?;
  let mut picture = Decoded {
    image: web_sys::HtmlImageElement::new().ok()?,
    source,
    media_type,
    width: 0,
    height: 0,
  };
  picture.image.set_src(&picture.source);
  if !decode_within(&picture.image, DECODE_TIMEOUT).await {
    return None;
  }
  // The browser's own size: upright, as it draws the picture.
  picture.width = picture.image.natural_width();
  picture.height = picture.image.natural_height();
  (picture.width > 0 && picture.height > 0).then_some(picture)
}

/// A new canvas of `width` × `height` pixels with its 2D context, smoothing at high quality.
pub fn canvas(
  width: u32,
  height: u32,
) -> Option<(web_sys::HtmlCanvasElement, web_sys::CanvasRenderingContext2d)> {
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
  // Not in web-sys' bindings; a browser without it keeps its default smoothing.
  drop(js_sys::Reflect::set(
    &context,
    &"imageSmoothingQuality".into(),
    &"high".into(),
  ));
  Some((canvas, context))
}

/// The canvas encoded; `None` when the browser makes nothing (a canvas too large for it) or the
/// result is larger than `max_bytes`.
pub async fn encode(
  canvas: &web_sys::HtmlCanvasElement,
  encoding: Encoding,
  max_bytes: usize,
) -> Option<Vec<u8>> {
  let blob: web_sys::Blob = JsFuture::from(to_blob(canvas, encoding))
    .await
    .ok()?
    .dyn_into()
    .ok()?;
  if blob.size() > max_bytes as f64 {
    return None;
  }
  let buffer = JsFuture::from(blob.array_buffer()).await.ok()?;
  Some(js_sys::Uint8Array::new(&buffer).to_vec())
}

/// [`PictureCodec::thumbnail`] by the browser.
async fn thumbnail(bytes: &[u8], media_type: &str) -> Option<Vec<u8>> {
  if !essence(media_type).starts_with("image/") {
    return None;
  }
  let picture = decoded(bytes).await?;
  // The first step's canvas tells whether the picture has transparency; a JPEG has none. Pixels
  // the browser won't hand out count as transparent: a PNG is always right.
  let first = picture.fitted(PNG_STEPS[0])?;
  let is_transparent = picture.media_type != "image/jpeg" && has_transparency(&first).unwrap_or(true);
  let mut drawn = vec![(PNG_STEPS[0], first)];
  let mut canvas_of = |side: u32| -> Option<web_sys::HtmlCanvasElement> {
    if let Some((_, canvas)) = drawn.iter().find(|(drawn_side, _)| *drawn_side == side) {
      return Some(canvas.clone());
    }
    let canvas = picture.fitted(side)?;
    drawn.push((side, canvas.clone()));
    Some(canvas)
  };
  if is_transparent {
    for side in PNG_STEPS {
      let Some(canvas) = canvas_of(side) else { continue };
      if let Some(png) = encode(&canvas, Encoding::Png, MAX_THUMBNAIL_BYTES).await {
        return Some(png);
      }
    }
  } else {
    for (side, quality) in JPEG_STEPS {
      let Some(canvas) = canvas_of(side) else { continue };
      if let Some(jpeg) = encode(&canvas, Encoding::Jpeg(quality), MAX_THUMBNAIL_BYTES).await {
        return Some(jpeg);
      }
    }
  }
  None
}

/// [`PictureCodec::without_metadata`] by the browser: drawn at its full size and encoded again, so
/// only pixels are left.
async fn without_metadata(bytes: &[u8]) -> Option<(Vec<u8>, &'static str)> {
  let picture = decoded(bytes).await?;
  let canvas = picture.fitted(picture.width.max(picture.height))?;
  match picture.media_type {
    "image/jpeg" => encode(&canvas, Encoding::Jpeg(SCREENSHOT_JPEG_QUALITY), usize::MAX)
      .await
      .map(|jpeg| (jpeg, "image/jpeg")),
    _ => encode(&canvas, Encoding::Png, usize::MAX)
      .await
      .map(|png| (png, "image/png")),
  }
}

/// Whether any pixel of the canvas is not fully opaque; `None` when the browser doesn't hand out
/// the pixels.
fn has_transparency(canvas: &web_sys::HtmlCanvasElement) -> Option<bool> {
  let context = canvas
    .get_context("2d")
    .ok()??
    .dyn_into::<web_sys::CanvasRenderingContext2d>()
    .ok()?;
  let pixels = context
    .get_image_data(0.0, 0.0, f64::from(canvas.width()), f64::from(canvas.height()))
    .ok()?
    .data();
  Some(pixels.as_chunks::<4>().0.iter().any(|pixel| pixel[3] < u8::MAX))
}

/// Waits for the `<img>` to decode, at most `timeout`.
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

/// The canvas as a blob; rejects when the browser makes none.
fn to_blob(canvas: &web_sys::HtmlCanvasElement, encoding: Encoding) -> js_sys::Promise {
  let canvas = canvas.clone();
  js_sys::Promise::new(&mut |resolve, reject| {
    let on_reject = reject.clone();
    let done = Closure::once_into_js(move |blob: JsValue| {
      // A canvas too large for the browser yields no blob.
      let result = match blob.is_null() {
        true => reject.call0(&JsValue::NULL),
        false => resolve.call1(&JsValue::NULL, &blob),
      };
      drop(result);
    });
    let started = match encoding {
      Encoding::Png => canvas.to_blob(done.unchecked_ref()),
      Encoding::Jpeg(quality) => canvas.to_blob_with_type_and_encoder_options(
        done.unchecked_ref(),
        "image/jpeg",
        &JsValue::from_f64(f64::from(quality) / 100.0),
      ),
    };
    if started.is_err() {
      drop(on_reject.call0(&JsValue::NULL));
    }
  })
}
