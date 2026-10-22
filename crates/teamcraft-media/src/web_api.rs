//! Bindings for the browser APIs the call driver needs that `web-sys` only offers behind
//! `--cfg web_sys_unstable_apis` (WebTransport, WebCodecs), plus the AudioWorklet code.
//!
//! Declaring the few members used here keeps that flag out of every wasm build of the workspace.
//! The shapes follow the W3C WebTransport and WebCodecs specifications.

use js_sys::{Object, Promise, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;
use web_sys::{ReadableStream, WritableStream};

/// The AudioWorklet module: `teamcraft-capture` posts every 128-sample render quantum of its
/// input to the main thread; `teamcraft-playback` plays queued mono chunks and drops the oldest
/// when more than `processorOptions.maxSamples` wait (latency bound).
pub const WORKLET_SOURCE: &str = r#"
class TeamcraftCapture extends AudioWorkletProcessor {
  process(inputs) {
    const channel = inputs[0] && inputs[0][0];
    if (channel) this.port.postMessage(channel.slice(0));
    return true;
  }
}

class TeamcraftPlayback extends AudioWorkletProcessor {
  constructor(options) {
    super();
    this.chunks = [];
    this.offset = 0;
    this.available = 0;
    this.maxSamples = options.processorOptions.maxSamples;
    this.port.onmessage = (event) => {
      this.chunks.push(event.data);
      this.available += event.data.length;
      while (this.available > this.maxSamples && this.chunks.length > 1) {
        this.available -= this.chunks[0].length - this.offset;
        this.chunks.shift();
        this.offset = 0;
      }
    };
  }

  process(_inputs, outputs) {
    const output = outputs[0];
    const out = output[0];
    let written = 0;
    while (written < out.length && this.chunks.length > 0) {
      const chunk = this.chunks[0];
      const count = Math.min(out.length - written, chunk.length - this.offset);
      out.set(chunk.subarray(this.offset, this.offset + count), written);
      written += count;
      this.offset += count;
      this.available -= count;
      if (this.offset === chunk.length) {
        this.chunks.shift();
        this.offset = 0;
      }
    }
    out.fill(0, written);
    for (let channel = 1; channel < output.length; channel++) output[channel].set(out);
    return true;
  }
}

registerProcessor("teamcraft-capture", TeamcraftCapture);
registerProcessor("teamcraft-playback", TeamcraftPlayback);
"#;

#[wasm_bindgen]
extern "C" {
  /// <https://www.w3.org/TR/webtransport/#web-transport>
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

  #[wasm_bindgen(method)]
  pub fn close(this: &WebTransport);

  pub type WebTransportDatagramDuplexStream;

  #[wasm_bindgen(method, getter)]
  pub fn readable(this: &WebTransportDatagramDuplexStream) -> ReadableStream;

  #[wasm_bindgen(method, getter)]
  pub fn writable(this: &WebTransportDatagramDuplexStream) -> WritableStream;

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
}

/// A plain JS object with the given properties (WebIDL dictionaries).
pub fn dictionary(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
  let object = Object::new();
  for (key, value) in entries {
    Reflect::set(&object, &JsValue::from_str(key), value)?;
  }
  Ok(object)
}

/// Whether the global scope has a constructor of this name (feature detection).
pub fn has_global(name: &str) -> bool {
  Reflect::has(&js_sys::global(), &JsValue::from_str(name)).unwrap_or(false)
}
