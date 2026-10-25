//! Call video on the desktop (#437). The shell receives and decrypts the call's media; for every
//! track the page shows it sends the frames over a Tauri channel, one binary message per frame
//! (`video_message` in `apps/desktop/src/calls/video.rs`): H.264 (or VP9 from older browsers)
//! that WebCodecs decodes here, or JPEG images where the webview can't decode H.264 (WebKitGTK
//! without the codec). The own camera comes as JPEG images, as the call sends it (with effects).
//!
//! [`LiveVideo`](crate::components::call_video::LiveVideo) shows the canvases made here, the same
//! way it shows the browser's call module's; a track is watched only while a tile shows it.

use std::collections::BTreeMap;

use thiserror::Error;

#[cfg(target_arch = "wasm32")]
pub use self::view::{can_show_video, hide, show};

/// Version of the frame message this page reads.
const MESSAGE_VERSION: u8 = 1;
/// Version, flags, codec, reserved, stream id (u32), timestamp (u64).
const HEADER_BYTES: usize = 16;
const KEYFRAME_FLAG: u8 = 0x01;
const CODEC_H264: u8 = 1;
const CODEC_VP9: u8 = 2;
const CODEC_JPEG: u8 = 3;
/// Messages held back for a missing one before it counts as lost (fetches can overtake each
/// other, a few frames' worth; a lost one never comes).
const MAX_WAITING: usize = 8;

/// The header of one frame message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameHeader {
  codec: FrameCodec,
  is_keyframe: bool,
  timestamp_us: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameCodec {
  H264,
  Vp9,
  /// One complete image.
  Jpeg,
}

/// Puts a channel's messages back in send order: Tauri numbers them, but large ones are fetched
/// and can arrive after later small ones.
struct InOrder<T> {
  next: u64,
  waiting: BTreeMap<u64, T>,
}

/// A message in send order; `is_after_gap` when messages before it were given up as lost.
#[derive(Debug, PartialEq, Eq)]
struct Delivery<T> {
  message: T,
  is_after_gap: bool,
}

#[derive(Debug, Error, PartialEq, Eq)]
enum FrameMessageError {
  #[error("a frame message of {0} bytes is shorter than its header")]
  Truncated(usize),
  #[error("frame message version {0} is not known")]
  UnknownVersion(u8),
  #[error("video codec {0} is not known")]
  UnknownCodec(u8),
}

impl FrameHeader {
  fn parse(bytes: &[u8]) -> Result<Self, FrameMessageError> {
    let Some(header) = bytes.get(..HEADER_BYTES) else {
      return Err(FrameMessageError::Truncated(bytes.len()));
    };
    if header[0] != MESSAGE_VERSION {
      return Err(FrameMessageError::UnknownVersion(header[0]));
    }
    let codec = match header[2] {
      CODEC_H264 => FrameCodec::H264,
      CODEC_VP9 => FrameCodec::Vp9,
      CODEC_JPEG => FrameCodec::Jpeg,
      other => return Err(FrameMessageError::UnknownCodec(other)),
    };
    let mut timestamp = [0; 8];
    timestamp.copy_from_slice(&header[8..16]);
    Ok(Self {
      codec,
      is_keyframe: header[1] & KEYFRAME_FLAG != 0,
      timestamp_us: u64::from_be_bytes(timestamp),
    })
  }
}

impl<T> InOrder<T> {
  fn new() -> Self {
    Self {
      next: 0,
      waiting: BTreeMap::new(),
    }
  }

  /// Takes message `index`; returns what can be delivered now, in order. Late duplicates and
  /// messages already given up on are dropped.
  fn push(&mut self, index: u64, message: T) -> Vec<Delivery<T>> {
    if index < self.next {
      return Vec::new();
    }
    self.waiting.insert(index, message);
    let mut ready = Vec::new();
    let mut is_after_gap = false;
    loop {
      while let Some(message) = self.waiting.remove(&self.next) {
        ready.push(Delivery {
          message,
          is_after_gap,
        });
        is_after_gap = false;
        self.next += 1;
      }
      if self.waiting.len() <= MAX_WAITING {
        return ready;
      }
      // Invariant: more than `MAX_WAITING` (> 0) entries wait, so there is a first one.
      if let Some((&first, _)) = self.waiting.first_key_value() {
        self.next = first;
        is_after_gap = true;
      }
    }
  }
}

#[cfg(target_arch = "wasm32")]
mod view {
  use std::cell::{Cell, RefCell};
  use std::collections::HashMap;
  use std::rc::{Rc, Weak};
  use std::time::Duration;

  use js_sys::{Array, Reflect, Uint8Array};
  use leptos::task::spawn_local;
  use serde_json::json;
  use teamcraft_media::web_api::{
    Canvas2d, EncodedVideoChunk, VideoDecoder, VideoFrame as WebVideoFrame, dictionary, has_global,
  };
  use wasm_bindgen::JsCast;
  use wasm_bindgen::prelude::*;
  use wasm_bindgen_futures::JsFuture;
  use web_sys::{Blob, BlobPropertyBag, CanvasRenderingContext2d, Element, HtmlCanvasElement, ImageBitmap};

  use super::{Delivery, FrameCodec, FrameHeader, HEADER_BYTES, InOrder};
  use crate::auth::tauri::{call, close_channel, open_channel};
  use crate::components::call_video::VideoSource;

  /// A track a tile stops showing keeps running this long, so a re-layout (the speaker changed,
  /// the window was resized) doesn't restart it and wait for a keyframe.
  const STOP_DELAY: Duration = Duration::from_millis(500);
  /// JPEG images being decoded at once; more are dropped (each image stands alone).
  const MAX_IMAGES_IN_FLIGHT: u32 = 2;
  /// WebCodecs codec strings, as the browser's call module uses them (`call_web/video.rs`).
  const H264_CODEC: &str = "avc1.42e028";
  const VP9_CODEC: &str = "vp09.00.10.08";

  thread_local! {
    static VIEWS: RefCell<HashMap<VideoSource, Rc<StreamView>>> = RefCell::default();
    /// Whether this webview decodes H.264 with WebCodecs; asked once.
    static CAN_DECODE_H264: Cell<Option<bool>> = const { Cell::new(None) };
  }

  /// Whether the page shows call video from the shell (it runs in the desktop app).
  pub fn can_show_video() -> bool {
    crate::auth::tauri::is_available()
  }

  /// The canvas showing `source` until [`hide`]; the shell starts sending it.
  pub fn show(source: VideoSource) -> Option<Element> {
    let maybe_running = VIEWS.with(|views| views.borrow().get(&source).cloned());
    let view = match maybe_running {
      Some(view) => view,
      None => {
        let view = StreamView::start(source)?;
        VIEWS.with(|views| views.borrow_mut().insert(source, Rc::clone(&view)));
        view
      },
    };
    view.watchers.set(view.watchers.get() + 1);
    Some(view.canvas.clone().into())
  }

  /// A tile stopped showing `source`; the shell stops sending it shortly after, unless a tile
  /// shows it again.
  pub fn hide(source: VideoSource) {
    let Some(view) = VIEWS.with(|views| views.borrow().get(&source).cloned()) else {
      return;
    };
    let watchers = view.watchers.get().saturating_sub(1);
    view.watchers.set(watchers);
    if watchers == 0 {
      spawn_local(async move {
        teamcraft_core::platform::sleep(STOP_DELAY).await;
        view.stop_if_unwatched();
      });
    }
  }

  /// One shown track: its canvas, the channel from the shell and the decoder.
  struct StreamView {
    source: VideoSource,
    canvas: HtmlCanvasElement,
    context: CanvasRenderingContext2d,
    watchers: Cell<u32>,
    is_stopped: Cell<bool>,
    maybe_channel: RefCell<Option<FrameChannel>>,
    /// Counts channels, so a callback of a replaced one is recognised.
    channel_generation: Cell<u64>,
    maybe_decoder: RefCell<Option<Decoder>>,
    is_waiting_for_keyframe: Cell<bool>,
    /// The shell was asked for JPEG images instead of H.264.
    is_sending_images: Cell<bool>,
    images_in_flight: Cell<u32>,
    image_sequence: Cell<u64>,
    last_drawn_image: Cell<u64>,
  }

  struct FrameChannel {
    generation: u64,
    callback_id: u32,
    order: InOrder<Uint8Array>,
    _callback: Closure<dyn FnMut(JsValue)>,
  }

  /// A WebCodecs decoder drawing into the view's canvas.
  struct Decoder {
    codec: FrameCodec,
    decoder: VideoDecoder,
    _output: Closure<dyn FnMut(WebVideoFrame)>,
    _error: Closure<dyn FnMut(JsValue)>,
  }

  impl StreamView {
    fn start(source: VideoSource) -> Option<Rc<Self>> {
      let document = web_sys::window()?.document()?;
      let canvas: HtmlCanvasElement = document.create_element("canvas").ok()?.dyn_into().ok()?;
      let context: CanvasRenderingContext2d = canvas.get_context("2d").ok()??.dyn_into().ok()?;
      let view = Rc::new(Self {
        source,
        canvas,
        context,
        watchers: Cell::new(0),
        is_stopped: Cell::new(false),
        maybe_channel: RefCell::new(None),
        channel_generation: Cell::new(0),
        maybe_decoder: RefCell::new(None),
        is_waiting_for_keyframe: Cell::new(true),
        is_sending_images: Cell::new(false),
        images_in_flight: Cell::new(0),
        image_sequence: Cell::new(0),
        last_drawn_image: Cell::new(0),
      });
      let starting = Rc::clone(&view);
      spawn_local(async move {
        let prefer_jpeg = match starting.source {
          VideoSource::Remote(_) => !can_decode_h264().await,
          VideoSource::OwnCamera => true,
        };
        starting.watch(prefer_jpeg);
      });
      Some(view)
    }

    /// Opens a channel and asks the shell to send the track through it (again, replacing the
    /// previous channel, when the decoding changes).
    fn watch(self: &Rc<Self>, prefer_jpeg: bool) {
      if self.is_stopped.get() {
        return;
      }
      self.is_sending_images.set(prefer_jpeg);
      let Some(frames) = self.open_frame_channel() else {
        tracing::warn!("the desktop bridge has no channels; the video stays blank");
        return;
      };
      let (command, args) = match self.source {
        VideoSource::Remote(stream_id) => (
          "call_watch_video",
          json!({ "stream_id": stream_id, "prefer_jpeg": prefer_jpeg, "frames": frames }),
        ),
        VideoSource::OwnCamera => ("call_watch_camera", json!({ "frames": frames })),
      };
      spawn_local(async move {
        if let Err(error) = call::<()>(command, &args).await {
          // The tile keeps its blank canvas; the next time it is shown it asks again.
          tracing::warn!(%error, command, "the shell does not send the video");
        }
      });
    }

    fn open_frame_channel(self: &Rc<Self>) -> Option<String> {
      self.close_frame_channel();
      let generation = self.channel_generation.get() + 1;
      self.channel_generation.set(generation);
      let weak: Weak<Self> = Rc::downgrade(self);
      let callback = Closure::<dyn FnMut(JsValue)>::new(move |message: JsValue| {
        if let Some(view) = weak.upgrade() {
          view.on_channel_message(generation, &message);
        }
      });
      let (callback_id, argument) = open_channel(callback.as_ref().unchecked_ref())?;
      *self.maybe_channel.borrow_mut() = Some(FrameChannel {
        generation,
        callback_id,
        order: InOrder::new(),
        _callback: callback,
      });
      self.is_waiting_for_keyframe.set(true);
      Some(argument)
    }

    fn close_frame_channel(&self) {
      if let Some(channel) = self.maybe_channel.borrow_mut().take() {
        close_channel(channel.callback_id);
      }
    }

    /// `{ message, index }` or `{ end: true, index }` from the bridge.
    fn on_channel_message(self: &Rc<Self>, generation: u64, message: &JsValue) {
      let field = |name: &str| Reflect::get(message, &JsValue::from_str(name)).unwrap_or(JsValue::UNDEFINED);
      // The shell dropped the channel (unwatched, the call ended): nothing more comes.
      if field("end").is_truthy() {
        return;
      }
      let Some(index) = field("index").as_f64() else {
        return;
      };
      // An `ArrayBuffer`, or an array of bytes where the bridge falls back to JSON.
      let bytes = Uint8Array::new(&field("message"));
      let deliveries = {
        let mut maybe_channel = self.maybe_channel.borrow_mut();
        match maybe_channel.as_mut() {
          // Indexes count up from 0 and stay far below 2^53.
          Some(channel) if channel.generation == generation => channel.order.push(index as u64, bytes),
          _ => return,
        }
      };
      for delivery in deliveries {
        self.on_frame(delivery);
      }
    }

    fn on_frame(self: &Rc<Self>, delivery: Delivery<Uint8Array>) {
      let Delivery {
        message: bytes,
        is_after_gap,
      } = delivery;
      let header_length = bytes.length().min(HEADER_BYTES as u32);
      let mut head = [0; HEADER_BYTES];
      let head = &mut head[..header_length as usize];
      bytes.subarray(0, header_length).copy_to(head);
      let header = match FrameHeader::parse(head) {
        Ok(header) => header,
        Err(error) => {
          tracing::debug!(%error, "video message skipped");
          return;
        },
      };
      let payload = bytes.subarray(HEADER_BYTES as u32, bytes.length());
      if is_after_gap {
        self.is_waiting_for_keyframe.set(true);
      }
      match header.codec {
        FrameCodec::Jpeg => self.draw_image(payload),
        FrameCodec::H264 | FrameCodec::Vp9 => self.decode(header, &payload),
      }
    }

    fn draw_image(self: &Rc<Self>, jpeg: Uint8Array) {
      if self.images_in_flight.get() >= MAX_IMAGES_IN_FLIGHT {
        return;
      }
      let Some(promise) = image_bitmap(&jpeg) else {
        return;
      };
      let sequence = self.image_sequence.get() + 1;
      self.image_sequence.set(sequence);
      self.images_in_flight.set(self.images_in_flight.get() + 1);
      let view = Rc::clone(self);
      spawn_local(async move {
        let maybe_bitmap = JsFuture::from(promise).await.ok();
        view.finish_image(sequence, maybe_bitmap.and_then(|bitmap| bitmap.dyn_into().ok()));
      });
    }

    /// Draws a decoded image unless a newer one is already on the canvas.
    fn finish_image(&self, sequence: u64, maybe_bitmap: Option<ImageBitmap>) {
      self
        .images_in_flight
        .set(self.images_in_flight.get().saturating_sub(1));
      let Some(bitmap) = maybe_bitmap else {
        tracing::debug!("video image not decodable");
        return;
      };
      if !self.is_stopped.get() && sequence > self.last_drawn_image.get() {
        self.last_drawn_image.set(sequence);
        fit_canvas(&self.canvas, bitmap.width(), bitmap.height());
        if self
          .context
          .draw_image_with_image_bitmap(&bitmap, 0.0, 0.0)
          .is_err()
        {
          tracing::debug!("video image not drawn");
        }
      }
      bitmap.close();
    }

    /// Decodes a frame; deltas before the first keyframe (or after a lost frame) can't be decoded
    /// and are skipped.
    fn decode(self: &Rc<Self>, header: FrameHeader, payload: &Uint8Array) {
      if self.is_waiting_for_keyframe.get() && !header.is_keyframe {
        return;
      }
      let mut maybe_decoder = self.maybe_decoder.borrow_mut();
      if maybe_decoder
        .as_ref()
        .is_none_or(|decoder| decoder.codec != header.codec)
      {
        *maybe_decoder = Decoder::new(header.codec, self);
      }
      let Some(decoder) = maybe_decoder.as_ref() else {
        self.decoding_failed();
        return;
      };
      self.is_waiting_for_keyframe.set(false);
      let chunk = dictionary(&[
        (
          "type",
          JsValue::from_str(if header.is_keyframe { "key" } else { "delta" }),
        ),
        // Only the order matters to the decoder; microseconds stay exact in an f64 for ages.
        ("timestamp", JsValue::from(header.timestamp_us as f64)),
        ("data", payload.into()),
      ])
      .and_then(|init| EncodedVideoChunk::new(&init));
      if chunk.and_then(|chunk| decoder.decoder.decode(&chunk)).is_err() {
        self.decoding_failed();
      }
    }

    /// WebCodecs can't decode this track here after all: the shell sends JPEG images instead,
    /// for this track and the ones shown later.
    fn decoding_failed(self: &Rc<Self>) {
      if self.is_sending_images.get() {
        return;
      }
      self.is_sending_images.set(true);
      tracing::warn!("the webview does not decode the call video; using images");
      CAN_DECODE_H264.set(Some(false));
      // Not right here: the failing decoder may be the caller (its error callback).
      let view = Rc::clone(self);
      spawn_local(async move {
        view.restart_with_images();
      });
    }

    fn restart_with_images(self: &Rc<Self>) {
      self.maybe_decoder.borrow_mut().take();
      self.watch(true);
    }

    fn stop_if_unwatched(self: &Rc<Self>) {
      if self.watchers.get() > 0 || self.is_stopped.get() {
        return;
      }
      self.is_stopped.set(true);
      VIEWS.with(|views| {
        let mut views = views.borrow_mut();
        if views
          .get(&self.source)
          .is_some_and(|current| Rc::ptr_eq(current, self))
        {
          views.remove(&self.source);
        }
      });
      self.close_frame_channel();
      self.maybe_decoder.borrow_mut().take();
      let (command, args) = match self.source {
        VideoSource::Remote(stream_id) => ("call_unwatch_video", json!({ "stream_id": stream_id })),
        VideoSource::OwnCamera => ("call_unwatch_camera", json!({})),
      };
      spawn_local(async move {
        if let Err(error) = call::<()>(command, &args).await {
          // The call may have ended meanwhile; the shell then sends nothing anyway.
          tracing::debug!(%error, command, "video not unwatched");
        }
      });
    }
  }

  impl Decoder {
    fn new(codec: FrameCodec, view: &Rc<StreamView>) -> Option<Self> {
      let codec_string = match codec {
        FrameCodec::H264 => H264_CODEC,
        FrameCodec::Vp9 => VP9_CODEC,
        FrameCodec::Jpeg => return None,
      };
      let canvas = view.canvas.clone();
      let context: Canvas2d = view.context.clone().unchecked_into();
      let output = Closure::<dyn FnMut(WebVideoFrame)>::new(move |frame: WebVideoFrame| {
        fit_canvas(&canvas, frame.display_width(), frame.display_height());
        if context.draw_video_frame(&frame, 0.0, 0.0).is_err() {
          tracing::debug!("video frame not drawn");
        }
        frame.close();
      });
      let weak = Rc::downgrade(view);
      let error = Closure::<dyn FnMut(JsValue)>::new(move |_error: JsValue| {
        if let Some(view) = weak.upgrade() {
          view.decoding_failed();
        }
      });
      let init = dictionary(&[
        ("output", output.as_ref().clone()),
        ("error", error.as_ref().clone()),
      ])
      .ok()?;
      let decoder = VideoDecoder::new(&init).ok()?;
      // H.264 in Annex B carries its parameter sets in band: no `description` needed.
      let config = dictionary(&[("codec", JsValue::from_str(codec_string))]).ok()?;
      decoder.configure(&config).ok()?;
      Some(Self {
        codec,
        decoder,
        _output: output,
        _error: error,
      })
    }
  }

  impl Drop for Decoder {
    fn drop(&mut self) {
      // Closing an already closed (failed) decoder throws; nothing is left to release then.
      let _ = self.decoder.close();
    }
  }

  /// Whether WebCodecs decodes H.264 here (`VideoDecoder.isConfigSupported`); asked once.
  async fn can_decode_h264() -> bool {
    if let Some(is_known) = CAN_DECODE_H264.get() {
      return is_known;
    }
    let is_supported = is_h264_supported().await;
    // async-ok: a thread-local Cell, not a signal.
    CAN_DECODE_H264.set(Some(is_supported));
    is_supported
  }

  async fn is_h264_supported() -> bool {
    if !has_global("VideoDecoder") {
      return false;
    }
    let Some(promise) = dictionary(&[("codec", JsValue::from_str(H264_CODEC))])
      .and_then(|config| VideoDecoder::is_config_supported(&config))
      .ok()
    else {
      return false;
    };
    JsFuture::from(promise)
      .await
      .ok()
      .and_then(|support| Reflect::get(&support, &JsValue::from_str("supported")).ok())
      .and_then(|is_supported| is_supported.as_bool())
      .unwrap_or(false)
  }

  /// `createImageBitmap` of a JPEG image.
  fn image_bitmap(jpeg: &Uint8Array) -> Option<js_sys::Promise> {
    let options = BlobPropertyBag::new();
    options.set_type("image/jpeg");
    let blob = Blob::new_with_u8_array_sequence_and_options(&Array::of1(jpeg), &options).ok()?;
    web_sys::window()?.create_image_bitmap_with_blob(&blob).ok()
  }

  fn fit_canvas(canvas: &HtmlCanvasElement, width: u32, height: u32) {
    if canvas.width() != width || canvas.height() != height {
      canvas.set_width(width);
      canvas.set_height(height);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  mod headers {
    use super::*;

    #[test]
    fn the_shells_messages_are_read() {
      // As `video_messages_have_a_fixed_header` in the desktop app writes it.
      let message = [
        1, 1, 1, 0, 1, 2, 3, 4, 0, 0, 0, 0, 0, 0x0f, 0x42, 0x40, 0, 0, 0, 1, 0x65,
      ];
      assert_eq!(
        FrameHeader::parse(&message),
        Ok(FrameHeader {
          codec: FrameCodec::H264,
          is_keyframe: true,
          timestamp_us: 1_000_000,
        })
      );
      let jpeg = [1, 3, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xd8];
      assert_eq!(
        FrameHeader::parse(&jpeg).map(|header| header.codec),
        Ok(FrameCodec::Jpeg)
      );
    }

    #[test]
    fn short_unknown_and_empty_messages_are_refused() {
      assert_eq!(FrameHeader::parse(&[]), Err(FrameMessageError::Truncated(0)));
      assert_eq!(
        FrameHeader::parse(&[1, 0, 1]),
        Err(FrameMessageError::Truncated(3))
      );
      let mut message = [0; HEADER_BYTES];
      message[0] = 2;
      assert_eq!(
        FrameHeader::parse(&message),
        Err(FrameMessageError::UnknownVersion(2))
      );
      message[0] = 1;
      assert_eq!(
        FrameHeader::parse(&message),
        Err(FrameMessageError::UnknownCodec(0))
      );
    }
  }

  mod ordering {
    use super::*;

    #[test]
    fn messages_overtaken_by_later_ones_are_delivered_in_order() {
      let mut order = InOrder::new();
      assert_eq!(messages(order.push(1, 'b')), vec![]);
      assert_eq!(messages(order.push(0, 'a')), vec![('a', false), ('b', false)]);
      assert_eq!(messages(order.push(2, 'c')), vec![('c', false)]);
      // Late duplicates are dropped.
      assert_eq!(messages(order.push(1, 'x')), vec![]);
    }

    #[test]
    fn a_lost_message_is_given_up_once_too_many_wait() {
      let mut order = InOrder::new();
      for index in 1..=MAX_WAITING as u64 {
        assert!(order.push(index, index).is_empty());
      }
      let delivered = order.push(MAX_WAITING as u64 + 1, MAX_WAITING as u64 + 1);
      assert_eq!(delivered.len(), MAX_WAITING + 1);
      assert!(delivered[0].is_after_gap);
      assert!(delivered[1..].iter().all(|delivery| !delivery.is_after_gap));
      // The lost one arriving after all is too late.
      assert!(order.push(0, 0).is_empty());
    }

    fn messages<T>(deliveries: Vec<Delivery<T>>) -> Vec<(T, bool)> {
      deliveries
        .into_iter()
        .map(|delivery| (delivery.message, delivery.is_after_gap))
        .collect()
    }
  }
}
