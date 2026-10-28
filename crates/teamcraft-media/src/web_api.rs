//! Bindings for the browser APIs the call driver needs that `web-sys` only offers behind
//! `--cfg web_sys_unstable_apis` (WebTransport, WebCodecs), plus the AudioWorklet code.
//!
//! Declaring the few members used here keeps that flag out of every wasm build of the workspace.
//! The shapes follow the W3C WebTransport and WebCodecs specifications.

use js_sys::{Function, Object, Promise, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;
use web_sys::{ReadableStream, WritableStream};

/// The AudioWorklet module: `teamcraft-capture` posts every 128-sample render quantum of its
/// input to the main thread; `teamcraft-playback` plays queued mono chunks and drops the oldest
/// when more than `processorOptions.maxSamples` wait (latency bound). Also a file of its own, for
/// pages that serve it ([`crate::call_web::WebCallConfig::maybe_worklet_url`]).
pub const WORKLET_SOURCE: &str = include_str!("call_web/teamcraft-call-worklet.js");

#[wasm_bindgen]
extern "C" {
  /// <https://www.w3.org/TR/webtransport/#web-transport>; clones and equality are of the same
  /// JavaScript object.
  #[derive(Clone, PartialEq)]
  pub type WebTransport;

  #[wasm_bindgen(constructor, catch)]
  pub fn new(url: &str, options: &Object) -> Result<WebTransport, JsValue>;

  #[wasm_bindgen(method, getter)]
  pub fn ready(this: &WebTransport) -> Promise;

  #[wasm_bindgen(method, getter)]
  pub fn closed(this: &WebTransport) -> Promise;

  #[wasm_bindgen(method, getter)]
  pub fn datagrams(this: &WebTransport) -> WebTransportDatagramDuplexStream;

  #[wasm_bindgen(method, js_name = createBidirectionalStream)]
  pub fn create_bidirectional_stream(this: &WebTransport) -> Promise;

  /// `close({closeCode, reason})`; closing a closed session does nothing.
  #[wasm_bindgen(method, catch)]
  pub fn close(this: &WebTransport, close_info: &Object) -> Result<(), JsValue>;

  /// `getStats()`: throws where the browser lacks it.
  #[wasm_bindgen(method, catch, js_name = getStats)]
  pub fn get_stats(this: &WebTransport) -> Result<Promise, JsValue>;

  pub type WebTransportDatagramDuplexStream;

  #[wasm_bindgen(method, getter)]
  pub fn readable(this: &WebTransportDatagramDuplexStream) -> ReadableStream;

  pub type WebTransportBidirectionalStream;

  #[wasm_bindgen(method, getter)]
  pub fn readable(this: &WebTransportBidirectionalStream) -> ReadableStream;

  #[wasm_bindgen(method, getter)]
  pub fn writable(this: &WebTransportBidirectionalStream) -> WritableStream;

  /// <https://www.w3.org/TR/webcodecs/#audioencoder-interface>
  pub type AudioEncoder;

  #[wasm_bindgen(constructor, catch)]
  pub fn new(init: &Object) -> Result<AudioEncoder, JsValue>;

  #[wasm_bindgen(method, catch)]
  pub fn configure(this: &AudioEncoder, config: &Object) -> Result<(), JsValue>;

  #[wasm_bindgen(method, catch)]
  pub fn encode(this: &AudioEncoder, data: &AudioData) -> Result<(), JsValue>;

  #[wasm_bindgen(method, catch)]
  pub fn close(this: &AudioEncoder) -> Result<(), JsValue>;

  /// <https://www.w3.org/TR/webcodecs/#audiodecoder-interface>
  pub type AudioDecoder;

  #[wasm_bindgen(constructor, catch)]
  pub fn new(init: &Object) -> Result<AudioDecoder, JsValue>;

  #[wasm_bindgen(method, catch)]
  pub fn configure(this: &AudioDecoder, config: &Object) -> Result<(), JsValue>;

  #[wasm_bindgen(method, catch)]
  pub fn decode(this: &AudioDecoder, chunk: &EncodedAudioChunk) -> Result<(), JsValue>;

  #[wasm_bindgen(method, catch)]
  pub fn close(this: &AudioDecoder) -> Result<(), JsValue>;

  /// <https://www.w3.org/TR/webcodecs/#audiodata-interface>
  pub type AudioData;

  #[wasm_bindgen(constructor, catch)]
  pub fn new(init: &Object) -> Result<AudioData, JsValue>;

  #[wasm_bindgen(method, getter, js_name = numberOfFrames)]
  pub fn number_of_frames(this: &AudioData) -> u32;

  #[wasm_bindgen(method, catch, js_name = copyTo)]
  pub fn copy_to(
    this: &AudioData,
    destination: &js_sys::Float32Array,
    options: &Object,
  ) -> Result<(), JsValue>;

  #[wasm_bindgen(method)]
  pub fn close(this: &AudioData);

  /// <https://www.w3.org/TR/webcodecs/#encodedaudiochunk-interface>
  pub type EncodedAudioChunk;

  #[wasm_bindgen(constructor, catch)]
  pub fn new(init: &Object) -> Result<EncodedAudioChunk, JsValue>;

  #[wasm_bindgen(method, getter, js_name = byteLength)]
  pub fn byte_length(this: &EncodedAudioChunk) -> u32;

  #[wasm_bindgen(method, catch, js_name = copyTo)]
  pub fn copy_to(this: &EncodedAudioChunk, destination: &Uint8Array) -> Result<(), JsValue>;
}

#[wasm_bindgen]
extern "C" {
  /// <https://www.w3.org/TR/webcodecs/#videoencoder-interface>
  pub type VideoEncoder;

  #[wasm_bindgen(constructor, catch)]
  pub fn new(init: &Object) -> Result<VideoEncoder, JsValue>;

  #[wasm_bindgen(method, catch)]
  pub fn configure(this: &VideoEncoder, config: &Object) -> Result<(), JsValue>;

  #[wasm_bindgen(method, catch)]
  pub fn encode(this: &VideoEncoder, frame: &VideoFrame, options: &Object) -> Result<(), JsValue>;

  #[wasm_bindgen(method, getter, js_name = encodeQueueSize)]
  pub fn encode_queue_size(this: &VideoEncoder) -> u32;

  /// `"unconfigured"`, `"configured"` or `"closed"`: WebCodecs closes an encoder after an error.
  #[wasm_bindgen(method, getter)]
  pub fn state(this: &VideoEncoder) -> String;

  #[wasm_bindgen(method, catch)]
  pub fn close(this: &VideoEncoder) -> Result<(), JsValue>;

  /// <https://www.w3.org/TR/webcodecs/#videodecoder-interface>
  pub type VideoDecoder;

  #[wasm_bindgen(constructor, catch)]
  pub fn new(init: &Object) -> Result<VideoDecoder, JsValue>;

  #[wasm_bindgen(method, catch)]
  pub fn configure(this: &VideoDecoder, config: &Object) -> Result<(), JsValue>;

  #[wasm_bindgen(method, catch)]
  pub fn decode(this: &VideoDecoder, chunk: &EncodedVideoChunk) -> Result<(), JsValue>;

  #[wasm_bindgen(method, catch)]
  pub fn close(this: &VideoDecoder) -> Result<(), JsValue>;

  #[wasm_bindgen(method, getter, js_name = decodeQueueSize)]
  pub fn decode_queue_size(this: &VideoDecoder) -> u32;

  /// `"unconfigured"`, `"configured"` or `"closed"`: WebCodecs closes a decoder after an error.
  #[wasm_bindgen(method, getter)]
  pub fn state(this: &VideoDecoder) -> String;

  /// Resolves to `{ supported, config }`.
  #[wasm_bindgen(static_method_of = VideoDecoder, js_name = isConfigSupported, catch)]
  pub fn is_config_supported(config: &Object) -> Result<Promise, JsValue>;

  /// <https://www.w3.org/TR/webcodecs/#videoframe-interface>
  pub type VideoFrame;

  /// From any `CanvasImageSource`, e.g. a playing `<video>` element.
  #[wasm_bindgen(constructor, catch)]
  pub fn new(source: &JsValue, init: &Object) -> Result<VideoFrame, JsValue>;

  #[wasm_bindgen(method, getter, js_name = displayWidth)]
  pub fn display_width(this: &VideoFrame) -> u32;

  #[wasm_bindgen(method, getter, js_name = displayHeight)]
  pub fn display_height(this: &VideoFrame) -> u32;

  #[wasm_bindgen(method)]
  pub fn close(this: &VideoFrame);

  /// <https://www.w3.org/TR/webcodecs/#encodedvideochunk-interface>
  pub type EncodedVideoChunk;

  #[wasm_bindgen(constructor, catch)]
  pub fn new(init: &Object) -> Result<EncodedVideoChunk, JsValue>;

  /// `"key"` or `"delta"`.
  #[wasm_bindgen(method, getter, js_name = type)]
  pub fn chunk_type(this: &EncodedVideoChunk) -> String;

  /// Microseconds.
  #[wasm_bindgen(method, getter)]
  pub fn timestamp(this: &EncodedVideoChunk) -> f64;

  #[wasm_bindgen(method, getter, js_name = byteLength)]
  pub fn byte_length(this: &EncodedVideoChunk) -> u32;

  #[wasm_bindgen(method, catch, js_name = copyTo)]
  pub fn copy_to(this: &EncodedVideoChunk, destination: &Uint8Array) -> Result<(), JsValue>;

  /// `CanvasRenderingContext2D`, declared for `drawImage` with a `VideoFrame` (not among
  /// web-sys' overloads).
  pub type Canvas2d;

  #[wasm_bindgen(method, catch, js_name = drawImage)]
  pub fn draw_video_frame(this: &Canvas2d, frame: &VideoFrame, x: f64, y: f64) -> Result<(), JsValue>;

  /// `drawImage(video, dx, dy, dw, dh)`.
  #[wasm_bindgen(method, catch, js_name = drawImage)]
  pub fn draw_video(
    this: &Canvas2d,
    video: &web_sys::HtmlVideoElement,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
  ) -> Result<(), JsValue>;

  /// `drawImage(bitmap, dx, dy, dw, dh)`.
  #[wasm_bindgen(method, catch, js_name = drawImage)]
  pub fn draw_bitmap(
    this: &Canvas2d,
    bitmap: &web_sys::ImageBitmap,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
  ) -> Result<(), JsValue>;

  #[wasm_bindgen(method, catch, js_name = getImageData)]
  pub fn get_image_data(
    this: &Canvas2d,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
  ) -> Result<web_sys::ImageData, JsValue>;
}

/// A tiny module with a WebAssembly SIMD instruction (`i8x16.popcnt` of a splat): browsers that
/// validate it run SIMD, which the segmentation module of the background effects needs (#439).
const SIMD_PROBE: [u8; 31] = [
  0, 97, 115, 109, 1, 0, 0, 0, 1, 5, 1, 96, 0, 1, 123, 3, 2, 1, 0, 10, 10, 1, 8, 0, 65, 0, 253, 15, 253, 98,
  11,
];

/// What the page's browser offers that the background effects need (#439): WebGPU
/// (`navigator.gpu`), WebGL2 and WebAssembly SIMD. Only whether the APIs exist: a GPU the browser
/// refuses shows when the effects start.
pub fn effects_graphics() -> (bool, bool, bool) {
  thread_local! {
    // The APIs don't change during a page's life; the UI asks on every render of the picker.
    static FOUND: std::cell::OnceCell<(bool, bool, bool)> = const { std::cell::OnceCell::new() };
  }
  FOUND.with(|found| *found.get_or_init(find_effects_graphics))
}

fn find_effects_graphics() -> (bool, bool, bool) {
  let global = js_sys::global();
  let has_webgpu = web_sys::window()
    .and_then(|window| Reflect::get(&window.navigator(), &JsValue::from_str("gpu")).ok())
    .is_some_and(|gpu| gpu.is_object());
  let has_webgl2 = Reflect::has(&global, &JsValue::from_str("WebGL2RenderingContext")).unwrap_or(false);
  let has_wasm_simd =
    js_sys::WebAssembly::validate(&Uint8Array::from(&SIMD_PROBE[..]).into()).unwrap_or(false);
  (has_webgpu, has_webgl2, has_wasm_simd)
}

/// A plain JS object with the given properties (WebIDL dictionaries).
pub fn dictionary(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
  let object = Object::new();
  for (key, value) in entries {
    Reflect::set(&object, &JsValue::from_str(key), value)?;
  }
  Ok(object)
}

/// The stream outgoing datagrams go to: `createWritable()` in the current WebTransport spec
/// (Safari), the `writable` attribute of earlier drafts (Chromium, Firefox). Takes any value, so
/// tests can pass stand-ins.
pub fn datagram_writable(datagrams: &JsValue) -> Result<WritableStream, JsValue> {
  let create = Reflect::get(datagrams, &JsValue::from_str("createWritable"))?;
  let stream = match create.dyn_ref::<Function>() {
    Some(create) => create.call0(datagrams)?,
    None => Reflect::get(datagrams, &JsValue::from_str("writable"))?,
  };
  if !stream.is_object() {
    return Err(JsValue::from_str("this browser offers no way to send datagrams"));
  }
  // Safari's `WebTransportDatagramsWritable` is a `WritableStream`.
  Ok(stream.unchecked_into())
}

/// Property of a call video's canvas that counts the pictures drawn into it, set by whoever draws
/// (the browser's call module, the desktop page's shell video). The page reads it to notice a
/// video that stopped (#921) without asking the call module, which runs as a module of its own.
const PICTURES_DRAWN: &str = "teamcraftPicturesDrawn";

/// Counts one picture drawn into `canvas` ([`pictures_drawn`]).
pub fn count_picture_drawn(canvas: &JsValue) {
  let drawn = pictures_drawn(canvas).unwrap_or(0).saturating_add(1);
  // Exact in an f64 for far longer than any call; a canvas that refuses properties counts nothing.
  let _ = Reflect::set(
    canvas,
    &JsValue::from_str(PICTURES_DRAWN),
    &JsValue::from(drawn as f64),
  );
}

/// The pictures drawn into a call video's element so far; `None` for an element nobody counts on
/// (a `<video>`, or a canvas of an older call module).
pub fn pictures_drawn(element: &JsValue) -> Option<u64> {
  let count = Reflect::get(element, &JsValue::from_str(PICTURES_DRAWN))
    .ok()?
    .as_f64()?;
  // Saturating float-to-int conversion of a count this code wrote.
  Some(count as u64)
}

/// Whether the global scope has a constructor of this name (feature detection).
pub fn has_global(name: &str) -> bool {
  Reflect::has(&js_sys::global(), &JsValue::from_str(name)).unwrap_or(false)
}
