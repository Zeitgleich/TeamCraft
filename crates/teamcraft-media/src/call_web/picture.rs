//! Own and organization background pictures (#439), decoded by the browser: the call module
//! carries no image decoder of its own, and the browser's is the one it trusts with every other
//! picture of the page anyway. The same limits as the apps' decoder apply (JPEG or PNG, file size,
//! pixel count), and the picture comes out scaled to fit the presets' size.

use js_sys::{Array, Reflect, Uint8Array};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Blob, BlobPropertyBag, HtmlCanvasElement, ImageBitmap};

use super::{dictionary, js_message, window};
use crate::effects::{
  BackgroundImage, EffectsError, MAX_BACKGROUND_FILE_BYTES, MAX_DECODED_DIMENSION, MAX_DECODED_PIXELS,
  PRESET_HEIGHT, PRESET_WIDTH, fitted_size,
};
use crate::web_api::Canvas2d;

/// Decodes a JPEG or PNG `file` to RGBA, scaled down to fit the presets' size.
pub async fn decode_picture(file: &[u8]) -> Result<BackgroundImage, EffectsError> {
  if file.len() > MAX_BACKGROUND_FILE_BYTES {
    return Err(EffectsError::UnsupportedImage(format!(
      "larger than {MAX_BACKGROUND_FILE_BYTES} bytes"
    )));
  }
  let kind = if file.starts_with(&[0xff, 0xd8, 0xff]) {
    "image/jpeg"
  } else if file.starts_with(b"\x89PNG\r\n\x1a\n") {
    "image/png"
  } else {
    return Err(EffectsError::UnsupportedImage(
      "only JPEG and PNG pictures".into(),
    ));
  };
  let options = BlobPropertyBag::new();
  options.set_type(kind);
  let blob = Blob::new_with_u8_array_sequence_and_options(&Array::of1(&Uint8Array::from(file)), &options)
    .map_err(failed("the picture"))?;
  let window = window().map_err(|error| EffectsError::UnsupportedImage(error.to_string()))?;
  let decoding = window
    .create_image_bitmap_with_blob(&blob)
    .map_err(failed("decoding the picture"))?;
  let bitmap: ImageBitmap = JsFuture::from(decoding)
    .await
    .map_err(failed("decoding the picture"))?
    .unchecked_into();
  let result = draw(&bitmap);
  bitmap.close();
  result
}

/// The decoded picture's pixels at the size that fits the presets'.
fn draw(bitmap: &ImageBitmap) -> Result<BackgroundImage, EffectsError> {
  let (width, height) = (bitmap.width(), bitmap.height());
  if width == 0
    || height == 0
    || width > MAX_DECODED_DIMENSION
    || height > MAX_DECODED_DIMENSION
    || u64::from(width) * u64::from(height) > MAX_DECODED_PIXELS
  {
    return Err(EffectsError::UnsupportedImage(format!(
      "{width} x {height} is no picture size this takes"
    )));
  }
  let (width, height) = fitted_size(width, height, PRESET_WIDTH, PRESET_HEIGHT);
  let canvas: HtmlCanvasElement = window()
    .map_err(|error| EffectsError::UnsupportedImage(error.to_string()))?
    .document()
    .ok_or_else(|| EffectsError::UnsupportedImage("no document".into()))?
    .create_element("canvas")
    .map_err(failed("canvas"))?
    .unchecked_into();
  canvas.set_width(width);
  canvas.set_height(height);
  let options = dictionary(&[("willReadFrequently", JsValue::TRUE)]).map_err(failed("canvas"))?;
  let context: Canvas2d = canvas
    .get_context_with_context_options("2d", &options)
    .map_err(failed("canvas"))?
    .ok_or_else(|| EffectsError::UnsupportedImage("no 2D canvas".into()))?
    .unchecked_into();
  // Scaling down: the browser's best filter.
  let _ = Reflect::set(
    &context,
    &JsValue::from_str("imageSmoothingQuality"),
    &JsValue::from_str("high"),
  );
  let (w, h) = (f64::from(width), f64::from(height));
  context
    .draw_bitmap(bitmap, 0.0, 0.0, w, h)
    .map_err(failed("drawing the picture"))?;
  let pixels = context
    .get_image_data(0.0, 0.0, w, h)
    .map_err(failed("reading the picture"))?
    .data()
    .0;
  BackgroundImage::new(width, height, pixels)
}

fn failed(what: &'static str) -> impl Fn(JsValue) -> EffectsError {
  move |error| EffectsError::UnsupportedImage(format!("{what}: {}", js_message(&error)))
}
