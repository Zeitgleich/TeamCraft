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

  #[wasm_bindgen(method, catch, js_name = getImageData)]
  pub fn get_image_data(
    this: &Canvas2d,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
  ) -> Result<web_sys::ImageData, JsValue>;
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

/// Whether the global scope has a constructor of this name (feature detection).
pub fn has_global(name: &str) -> bool {
  Reflect::has(&js_sys::global(), &JsValue::from_str(name)).unwrap_or(false)
}
