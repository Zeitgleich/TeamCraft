//! Screen share in the browser: `getDisplayMedia` → WebCodecs VP9 → the engine, and other
//! devices' tracks decoded into canvases the app provides.
//!
//! One layer, no simulcast (screen content needs its full resolution to stay readable); the SFU
//! never pauses it. A `<video>` element plays the captured stream and frames are taken from it at
//! a fixed rate, which works in every browser with WebCodecs (Firefox has no
//! MediaStreamTrackProcessor).

use futures::channel::mpsc;
use js_sys::Uint8Array;
use teamcraft_proto::{MediaStreamId, VideoLayerInfo};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
  DisplayMediaStreamConstraints, HtmlCanvasElement, HtmlVideoElement, MediaStream, MediaStreamTrack,
};

use super::{Input, Interval, WebCallError, browser, dictionary, js_message, window};
use crate::call_video::ReceivedVideoFrame;
use crate::web_api::{Canvas2d, EncodedVideoChunk, VideoDecoder, VideoEncoder, VideoFrame as WebVideoFrame};

/// WebCodecs codec string: VP9 profile 0, level 1.0, 8 bit.
const VP9_CODEC: &str = "vp09.00.10.08";
const SCREEN_FRAMERATE: u8 = 15;
const SCREEN_MAX_WIDTH: u32 = 1920;
const SCREEN_MAX_HEIGHT: u32 = 1080;
const SCREEN_BITRATE_BPS: u32 = 1_500_000;
/// A keyframe at least this often, so a lost frame doesn't freeze the picture for long.
const KEYFRAME_INTERVAL_FRAMES: u32 = 4 * SCREEN_FRAMERATE as u32;
/// Frames waiting in the encoder above this are skipped (slow machine or encoder).
const MAX_ENCODE_QUEUE: u32 = 2;

/// This device's screen share.
pub(super) struct ScreenShare {
  stream: MediaStream,
  video: HtmlVideoElement,
  encoder: VideoEncoder,
  layer: VideoLayerInfo,
  pub maybe_stream_id: Option<MediaStreamId>,
  wants_keyframe: bool,
  frames_since_keyframe: u32,
  timestamp_us: f64,
  _timer: Interval,
  _output: Closure<dyn FnMut(EncodedVideoChunk)>,
  _error: Closure<dyn FnMut(JsValue)>,
  _ended: Closure<dyn FnMut()>,
}

/// Decodes one remote track into a canvas.
pub(super) struct VideoView {
  decoder: VideoDecoder,
  is_waiting_for_keyframe: bool,
  _output: Closure<dyn FnMut(WebVideoFrame)>,
  _error: Closure<dyn FnMut(JsValue)>,
}

impl ScreenShare {
  /// Starts encoding the stream's video track.
  pub async fn start(
    stream: MediaStream,
    inputs: &mpsc::UnboundedSender<Input>,
  ) -> Result<Self, WebCallError> {
    let track: MediaStreamTrack = stream
      .get_video_tracks()
      .get(0)
      .dyn_into()
      .map_err(|_| WebCallError::Unsupported("a video track from getDisplayMedia"))?;
    let settings = track.get_settings();
    let width = settings
      .get_width()
      .and_then(|width| u16::try_from(width).ok())
      .unwrap_or(1280);
    let height = settings
      .get_height()
      .and_then(|height| u16::try_from(height).ok())
      .unwrap_or(720);
    let layer = VideoLayerInfo {
      layer: 0,
      width,
      height,
      max_framerate: SCREEN_FRAMERATE,
      max_bitrate_bps: SCREEN_BITRATE_BPS,
    };
    let video = play_hidden(&stream).await?;
    let sender = inputs.clone();
    let output = Closure::<dyn FnMut(EncodedVideoChunk)>::new(move |chunk: EncodedVideoChunk| {
      let bytes = Uint8Array::new_with_length(chunk.byte_length());
      let input = match chunk.copy_to(&bytes) {
        Ok(()) => Input::ScreenEncoded {
          data: bytes.to_vec(),
          is_keyframe: chunk.chunk_type() == "key",
          timestamp_us: chunk.timestamp(),
        },
        Err(error) => Input::CodecFailed(js_message(&error)),
      };
      let _ = sender.unbounded_send(input);
    });
    let error = super::codec_error_callback(inputs);
    let encoder = VideoEncoder::new(
      &dictionary(&[
        ("output", output.as_ref().clone()),
        ("error", error.as_ref().clone()),
      ])
      .map_err(browser("VideoEncoder"))?,
    )
    .map_err(browser("VideoEncoder"))?;
    encoder
      .configure(
        &dictionary(&[
          ("codec", JsValue::from_str(VP9_CODEC)),
          ("width", JsValue::from(width)),
          ("height", JsValue::from(height)),
          ("bitrate", JsValue::from(SCREEN_BITRATE_BPS)),
          ("framerate", JsValue::from(SCREEN_FRAMERATE)),
          ("latencyMode", JsValue::from_str("realtime")),
          ("contentHint", JsValue::from_str("detail")),
        ])
        .map_err(browser("VideoEncoder"))?,
      )
      .map_err(browser("VideoEncoder"))?;
    // "Stop sharing" in the browser's own UI ends the track.
    let ended_sender = inputs.clone();
    let ended = Closure::<dyn FnMut()>::new(move || {
      let _ = ended_sender.unbounded_send(Input::ScreenEnded);
    });
    track.set_onended(Some(ended.as_ref().unchecked_ref()));
    let timer = Interval::start(1000 / i32::from(SCREEN_FRAMERATE), inputs, || Input::ScreenTick)?;
    Ok(Self {
      stream,
      video,
      encoder,
      layer,
      maybe_stream_id: None,
      wants_keyframe: true,
      frames_since_keyframe: 0,
      timestamp_us: 0.0,
      _timer: timer,
      _output: output,
      _error: error,
      _ended: ended,
    })
  }

  pub fn layers(&self) -> Vec<VideoLayerInfo> {
    vec![self.layer]
  }

  pub fn request_keyframe(&mut self) {
    self.wants_keyframe = true;
  }

  /// Encodes the current picture; skipped while the encoder is behind or nothing plays yet.
  pub fn capture(&mut self) -> Result<(), WebCallError> {
    if self.maybe_stream_id.is_none() || self.encoder.encode_queue_size() > MAX_ENCODE_QUEUE {
      return Ok(());
    }
    if self.video.video_width() == 0 {
      return Ok(());
    }
    let frame = WebVideoFrame::new(
      self.video.as_ref(),
      &dictionary(&[("timestamp", JsValue::from(self.timestamp_us))]).map_err(browser("VideoFrame"))?,
    )
    .map_err(browser("VideoFrame"))?;
    self.timestamp_us += 1_000_000.0 / f64::from(SCREEN_FRAMERATE);
    let is_keyframe = self.wants_keyframe || self.frames_since_keyframe >= KEYFRAME_INTERVAL_FRAMES;
    let options =
      dictionary(&[("keyFrame", JsValue::from_bool(is_keyframe))]).map_err(browser("VideoEncoder"))?;
    let result = self.encoder.encode(&frame, &options);
    frame.close();
    result.map_err(browser("VideoEncoder"))?;
    if is_keyframe {
      self.wants_keyframe = false;
      self.frames_since_keyframe = 0;
    } else {
      self.frames_since_keyframe += 1;
    }
    Ok(())
  }

  pub fn stop(self) {
    for track in self.stream.get_tracks().iter() {
      track.unchecked_into::<MediaStreamTrack>().stop();
    }
    self.video.set_src_object(None);
    // Closing an encoder that already failed throws; nothing left to release then.
    let _ = self.encoder.close();
  }
}

impl VideoView {
  pub fn new(canvas: HtmlCanvasElement, inputs: &mpsc::UnboundedSender<Input>) -> Result<Self, WebCallError> {
    let context: Canvas2d = canvas
      .get_context("2d")
      .map_err(browser("canvas"))?
      .ok_or(WebCallError::Unsupported("a 2D canvas"))?
      .unchecked_into();
    let error_sender = inputs.clone();
    let output = Closure::<dyn FnMut(WebVideoFrame)>::new(move |frame: WebVideoFrame| {
      let (width, height) = (frame.display_width(), frame.display_height());
      if canvas.width() != width || canvas.height() != height {
        canvas.set_width(width);
        canvas.set_height(height);
      }
      if let Err(error) = context.draw_video_frame(&frame, 0.0, 0.0) {
        let _ = error_sender.unbounded_send(Input::CodecFailed(js_message(&error)));
      }
      frame.close();
    });
    let error = super::codec_error_callback(inputs);
    let decoder = VideoDecoder::new(
      &dictionary(&[
        ("output", output.as_ref().clone()),
        ("error", error.as_ref().clone()),
      ])
      .map_err(browser("VideoDecoder"))?,
    )
    .map_err(browser("VideoDecoder"))?;
    decoder
      .configure(&dictionary(&[("codec", JsValue::from_str(VP9_CODEC))]).map_err(browser("VideoDecoder"))?)
      .map_err(browser("VideoDecoder"))?;
    Ok(Self {
      decoder,
      is_waiting_for_keyframe: true,
      _output: output,
      _error: error,
    })
  }

  /// Decodes a frame; deltas before the first keyframe can't be decoded and are skipped.
  pub fn decode(&mut self, frame: &ReceivedVideoFrame) -> Result<(), WebCallError> {
    if self.is_waiting_for_keyframe && !frame.is_keyframe {
      return Ok(());
    }
    self.is_waiting_for_keyframe = false;
    let chunk = EncodedVideoChunk::new(
      &dictionary(&[
        (
          "type",
          JsValue::from_str(if frame.is_keyframe { "key" } else { "delta" }),
        ),
        // RTP video clock (90 kHz) to microseconds; only the order matters to the decoder.
        (
          "timestamp",
          JsValue::from(f64::from(frame.timestamp.0) * 1_000_000.0 / 90_000.0),
        ),
        ("data", Uint8Array::from(frame.data.as_slice()).into()),
      ])
      .map_err(browser("EncodedVideoChunk"))?,
    )
    .map_err(browser("EncodedVideoChunk"))?;
    self.decoder.decode(&chunk).map_err(browser("VideoDecoder"))
  }
}

impl Drop for VideoView {
  fn drop(&mut self) {
    let _ = self.decoder.close();
  }
}

/// Asks the user what to share (`getDisplayMedia`). Needs a recent user gesture.
pub(super) async fn capture_screen() -> Result<MediaStream, WebCallError> {
  let video = dictionary(&[
    (
      "frameRate",
      dictionary(&[("max", JsValue::from(SCREEN_FRAMERATE))])
        .map_err(browser("getDisplayMedia"))?
        .into(),
    ),
    (
      "width",
      dictionary(&[("max", JsValue::from(SCREEN_MAX_WIDTH))])
        .map_err(browser("getDisplayMedia"))?
        .into(),
    ),
    (
      "height",
      dictionary(&[("max", JsValue::from(SCREEN_MAX_HEIGHT))])
        .map_err(browser("getDisplayMedia"))?
        .into(),
    ),
  ])
  .map_err(browser("getDisplayMedia"))?;
  let constraints = DisplayMediaStreamConstraints::new();
  constraints.set_video(&video);
  let promise = window()?
    .navigator()
    .media_devices()
    .map_err(browser("mediaDevices"))?
    .get_display_media_with_constraints(&constraints)
    .map_err(browser("getDisplayMedia"))?;
  Ok(
    JsFuture::from(promise)
      .await
      .map_err(browser("getDisplayMedia"))?
      .unchecked_into(),
  )
}

/// A muted, detached `<video>` playing the stream, to take frames from.
async fn play_hidden(stream: &MediaStream) -> Result<HtmlVideoElement, WebCallError> {
  let video: HtmlVideoElement = window()?
    .document()
    .ok_or(WebCallError::Unsupported("a document"))?
    .create_element("video")
    .map_err(browser("video element"))?
    .unchecked_into();
  video.set_muted(true);
  video.set_src_object(Some(stream));
  let promise = video.play().map_err(browser("video playback"))?;
  JsFuture::from(promise).await.map_err(browser("video playback"))?;
  Ok(video)
}
