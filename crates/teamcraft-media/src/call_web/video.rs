//! Video in the browser: camera (`getUserMedia`, three simulcast layers, optional background
//! effects) and screen share (`getDisplayMedia`, one layer) → WebCodecs H.264 → the engine; other
//! devices' tracks decoded into canvases the app provides.
//!
//! H.264 Constrained Baseline in Annex B, like the native encoder, so browsers and desktops decode
//! each other. A `<video>` element plays the captured stream and frames are taken from it at a
//! fixed rate, which works in every browser with WebCodecs (Firefox has no
//! MediaStreamTrackProcessor). Each simulcast layer has its own encoder configured at the layer's
//! size; WebCodecs scales the frame to it.

use futures::channel::mpsc;
use js_sys::Uint8Array;
use teamcraft_proto::{MediaStreamId, VideoCodec, VideoLayerInfo, VideoTrackKind};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
  DisplayMediaStreamConstraints, HtmlCanvasElement, HtmlVideoElement, MediaStream, MediaStreamConstraints,
  MediaStreamTrack, MediaTrackConstraints,
};

use super::{Input, Interval, WebCallError, browser, dictionary, js_message, window};
use crate::call_video::ReceivedVideoFrame;
use crate::video::{VideoResolution, simulcast_layers};
use crate::web_api::{Canvas2d, EncodedVideoChunk, VideoDecoder, VideoEncoder, VideoFrame as WebVideoFrame};

/// WebCodecs codec string: H.264 Constrained Baseline, level 4.0 (up to 1080p).
const H264_CODEC: &str = "avc1.42e028";
/// WebCodecs codec string: VP9 profile 0 (older browser clients).
const VP9_CODEC: &str = "vp09.00.10.08";
const CAMERA_WIDTH: u32 = 1280;
const CAMERA_HEIGHT: u32 = 720;
const CAMERA_FRAMERATE: u8 = 30;
const CAMERA_BITRATE_BPS: u32 = 1_500_000;
const SCREEN_FRAMERATE: u8 = 15;
const SCREEN_MAX_WIDTH: u32 = 1920;
const SCREEN_MAX_HEIGHT: u32 = 1080;
const SCREEN_BITRATE_BPS: u32 = 2_000_000;
/// A keyframe at least this often (seconds), so a lost frame doesn't freeze the picture for long.
const KEYFRAME_INTERVAL_SECONDS: u32 = 4;
/// Frames waiting in an encoder above this are skipped (slow machine or encoder).
const MAX_ENCODE_QUEUE: u32 = 2;

/// One own track: capture, optional effects, an encoder per layer.
pub(super) struct TrackSender {
  pub kind: VideoTrackKind,
  pub maybe_stream_id: Option<MediaStreamId>,
  stream: MediaStream,
  video: HtmlVideoElement,
  layers: Vec<LayerEncoder>,
  framerate: u8,
  frames_since_keyframe: u32,
  timestamp_us: f64,
  #[cfg(feature = "effects")]
  maybe_effects: Option<effects::WebEffects>,
  _timer: Interval,
  _ended: Closure<dyn FnMut()>,
}

struct LayerEncoder {
  info: VideoLayerInfo,
  encoder: VideoEncoder,
  is_needed: bool,
  wants_keyframe: bool,
  _output: Closure<dyn FnMut(EncodedVideoChunk)>,
  _error: Closure<dyn FnMut(JsValue)>,
}

/// Decodes one remote track into a canvas; the decoder is set up for the codec of the first frame.
pub(super) struct VideoView {
  canvas: HtmlCanvasElement,
  inputs: mpsc::UnboundedSender<Input>,
  maybe_decoder: Option<Decoder>,
}

struct Decoder {
  codec: VideoCodec,
  decoder: VideoDecoder,
  is_waiting_for_keyframe: bool,
  _output: Closure<dyn FnMut(WebVideoFrame)>,
  _error: Closure<dyn FnMut(JsValue)>,
}

/// What [`TrackSender::start`] needs besides the stream.
pub(super) struct TrackPlan {
  pub kind: VideoTrackKind,
  pub framerate: u8,
  pub bitrate_bps: u32,
  pub max_layers: u8,
  #[cfg(feature = "effects")]
  pub maybe_effects: Option<crate::effects::EffectsPipeline>,
}

impl TrackSender {
  /// Starts encoding the stream's video track as `plan.kind`.
  pub async fn start(
    stream: MediaStream,
    plan: TrackPlan,
    inputs: &mpsc::UnboundedSender<Input>,
  ) -> Result<Self, WebCallError> {
    let track: MediaStreamTrack = stream
      .get_video_tracks()
      .get(0)
      .dyn_into()
      .map_err(|_| WebCallError::Unsupported("a video track in the stream"))?;
    let settings = track.get_settings();
    let resolution = VideoResolution {
      width: settings
        .get_width()
        .and_then(|width| u16::try_from(width & !1).ok())
        .unwrap_or(1280),
      height: settings
        .get_height()
        .and_then(|height| u16::try_from(height & !1).ok())
        .unwrap_or(720),
    };
    let video = play_hidden(&stream).await?;
    let infos = simulcast_layers(resolution, plan.framerate, plan.bitrate_bps, plan.max_layers);
    let layers = infos
      .into_iter()
      .map(|info| LayerEncoder::new(info, plan.kind.clone(), plan.framerate, inputs))
      .collect::<Result<Vec<_>, _>>()?;
    // "Stop sharing" in the browser's own UI (or an unplugged camera) ends the track.
    let ended_sender = inputs.clone();
    let ended_kind = plan.kind.clone();
    let ended = Closure::<dyn FnMut()>::new(move || {
      let _ = ended_sender.unbounded_send(Input::VideoEnded(ended_kind.clone()));
    });
    track.set_onended(Some(ended.as_ref().unchecked_ref()));
    let period_ms = 1000 / i32::from(plan.framerate.max(1));
    let timer = match plan.kind {
      VideoTrackKind::Screen => {
        Interval::start(period_ms, inputs, || Input::VideoTick(VideoTrackKind::Screen))?
      },
      _ => Interval::start(period_ms, inputs, || Input::VideoTick(VideoTrackKind::Camera))?,
    };
    Ok(Self {
      kind: plan.kind,
      maybe_stream_id: None,
      stream,
      video,
      layers,
      framerate: plan.framerate,
      frames_since_keyframe: 0,
      timestamp_us: 0.0,
      #[cfg(feature = "effects")]
      maybe_effects: plan.maybe_effects.map(effects::WebEffects::new).transpose()?,
      _timer: timer,
      _ended: ended,
    })
  }

  pub fn layers(&self) -> Vec<VideoLayerInfo> {
    self.layers.iter().map(|layer| layer.info).collect()
  }

  pub fn request_keyframe(&mut self, layer: u8) {
    if let Some(encoder) = self.layers.iter_mut().find(|encoder| encoder.info.layer == layer) {
      encoder.wants_keyframe = true;
    }
  }

  /// Only `needed` layers are encoded (the SFU's `LayersNeeded`); a resumed layer starts with a
  /// keyframe.
  pub fn set_needed_layers(&mut self, needed: &[u8]) {
    for encoder in &mut self.layers {
      let is_needed = needed.contains(&encoder.info.layer);
      encoder.wants_keyframe |= is_needed && !encoder.is_needed;
      encoder.is_needed = is_needed;
    }
  }

  #[cfg(feature = "effects")]
  pub fn set_effect(&mut self, effect: crate::effects::BackgroundEffect) {
    if let Some(effects) = &mut self.maybe_effects {
      effects.pipeline.set_effect(effect);
    }
  }

  /// Encodes the current picture on every needed layer; skipped while nothing plays yet.
  pub async fn capture(&mut self) -> Result<(), WebCallError> {
    if self.maybe_stream_id.is_none() || self.video.video_width() == 0 {
      return Ok(());
    }
    let frame = self.next_frame().await?;
    self.timestamp_us += 1_000_000.0 / f64::from(self.framerate.max(1));
    let is_periodic_keyframe =
      self.frames_since_keyframe >= KEYFRAME_INTERVAL_SECONDS * u32::from(self.framerate);
    self.frames_since_keyframe = if is_periodic_keyframe {
      0
    } else {
      self.frames_since_keyframe + 1
    };
    let mut result = Ok(());
    for layer in self.layers.iter_mut().filter(|layer| layer.is_needed) {
      if layer.encoder.encode_queue_size() > MAX_ENCODE_QUEUE {
        continue;
      }
      let is_keyframe = std::mem::take(&mut layer.wants_keyframe) || is_periodic_keyframe;
      let options =
        dictionary(&[("keyFrame", JsValue::from_bool(is_keyframe))]).map_err(browser("VideoEncoder"))?;
      if let Err(error) = layer.encoder.encode(&frame, &options) {
        result = Err(browser("VideoEncoder")(error));
      }
    }
    frame.close();
    result
  }

  /// The picture to encode: straight from the `<video>`, or through the effects.
  async fn next_frame(&mut self) -> Result<WebVideoFrame, WebCallError> {
    #[cfg(feature = "effects")]
    if let Some(effects) = &mut self.maybe_effects {
      return effects.frame(&self.video, self.timestamp_us).await;
    }
    WebVideoFrame::new(
      self.video.as_ref(),
      &dictionary(&[("timestamp", JsValue::from(self.timestamp_us))]).map_err(browser("VideoFrame"))?,
    )
    .map_err(browser("VideoFrame"))
  }

  pub fn stop(self) {
    for track in self.stream.get_tracks().iter() {
      track.unchecked_into::<MediaStreamTrack>().stop();
    }
    self.video.set_src_object(None);
    for layer in &self.layers {
      // Closing an encoder that already failed throws; nothing left to release then.
      let _ = layer.encoder.close();
    }
  }
}

impl LayerEncoder {
  fn new(
    info: VideoLayerInfo,
    kind: VideoTrackKind,
    framerate: u8,
    inputs: &mpsc::UnboundedSender<Input>,
  ) -> Result<Self, WebCallError> {
    let sender = inputs.clone();
    let layer = info.layer;
    let output = Closure::<dyn FnMut(EncodedVideoChunk)>::new(move |chunk: EncodedVideoChunk| {
      let bytes = Uint8Array::new_with_length(chunk.byte_length());
      let input = match chunk.copy_to(&bytes) {
        Ok(()) => Input::VideoEncoded {
          kind: kind.clone(),
          layer,
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
    let avc = dictionary(&[("format", JsValue::from_str("annexb"))]).map_err(browser("VideoEncoder"))?;
    encoder
      .configure(
        &dictionary(&[
          ("codec", JsValue::from_str(H264_CODEC)),
          ("avc", avc.into()),
          ("width", JsValue::from(info.width)),
          ("height", JsValue::from(info.height)),
          ("bitrate", JsValue::from(info.max_bitrate_bps)),
          ("framerate", JsValue::from(framerate)),
          ("latencyMode", JsValue::from_str("realtime")),
        ])
        .map_err(browser("VideoEncoder"))?,
      )
      .map_err(browser("VideoEncoder"))?;
    Ok(Self {
      info,
      encoder,
      is_needed: true,
      wants_keyframe: true,
      _output: output,
      _error: error,
    })
  }
}

impl VideoView {
  pub fn new(canvas: HtmlCanvasElement, inputs: &mpsc::UnboundedSender<Input>) -> Self {
    Self {
      canvas,
      inputs: inputs.clone(),
      maybe_decoder: None,
    }
  }

  /// Decodes a frame; deltas before the first keyframe can't be decoded and are skipped.
  pub fn decode(&mut self, frame: &ReceivedVideoFrame) -> Result<(), WebCallError> {
    if self
      .maybe_decoder
      .as_ref()
      .is_none_or(|decoder| decoder.codec != frame.codec)
    {
      self.maybe_decoder = Some(Decoder::new(&frame.codec, self.canvas.clone(), &self.inputs)?);
    }
    let Some(decoder) = &mut self.maybe_decoder else {
      return Ok(());
    };
    if decoder.is_waiting_for_keyframe && !frame.is_keyframe {
      return Ok(());
    }
    decoder.is_waiting_for_keyframe = false;
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
    decoder.decoder.decode(&chunk).map_err(browser("VideoDecoder"))
  }
}

impl Decoder {
  fn new(
    codec: &VideoCodec,
    canvas: HtmlCanvasElement,
    inputs: &mpsc::UnboundedSender<Input>,
  ) -> Result<Self, WebCallError> {
    let codec_string = match codec {
      VideoCodec::H264 => H264_CODEC,
      VideoCodec::Vp9 => VP9_CODEC,
      _ => return Err(WebCallError::Unsupported("this video codec")),
    };
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
    // H.264 in Annex B carries its parameter sets in band: no `description` needed.
    decoder
      .configure(&dictionary(&[("codec", JsValue::from_str(codec_string))]).map_err(browser("VideoDecoder"))?)
      .map_err(browser("VideoDecoder"))?;
    Ok(Self {
      codec: codec.clone(),
      decoder,
      is_waiting_for_keyframe: true,
      _output: output,
      _error: error,
    })
  }
}

impl Drop for Decoder {
  fn drop(&mut self) {
    let _ = self.decoder.close();
  }
}

/// The plan for a camera: three simulcast layers from 720p at 30 fps.
pub(super) fn camera_plan() -> TrackPlan {
  TrackPlan {
    kind: VideoTrackKind::Camera,
    framerate: CAMERA_FRAMERATE,
    bitrate_bps: CAMERA_BITRATE_BPS,
    max_layers: 3,
    #[cfg(feature = "effects")]
    maybe_effects: None,
  }
}

/// The plan for a screen share: one layer (text must stay readable), 15 fps.
pub(super) fn screen_plan() -> TrackPlan {
  TrackPlan {
    kind: VideoTrackKind::Screen,
    framerate: SCREEN_FRAMERATE,
    bitrate_bps: SCREEN_BITRATE_BPS,
    max_layers: 1,
    #[cfg(feature = "effects")]
    maybe_effects: None,
  }
}

/// Asks for the camera (`getUserMedia`); `maybe_device` is a `MediaDeviceInfo.deviceId`.
pub(super) async fn capture_camera(maybe_device: Option<&str>) -> Result<MediaStream, WebCallError> {
  let video = MediaTrackConstraints::new();
  video.set_width(&ideal(CAMERA_WIDTH)?);
  video.set_height(&ideal(CAMERA_HEIGHT)?);
  video.set_frame_rate(&ideal(u32::from(CAMERA_FRAMERATE))?);
  if let Some(device) = maybe_device {
    let exact = dictionary(&[("exact", JsValue::from_str(device))]).map_err(browser("getUserMedia"))?;
    video.set_device_id(&exact.into());
  }
  let constraints = MediaStreamConstraints::new();
  constraints.set_video(&video);
  let promise = window()?
    .navigator()
    .media_devices()
    .map_err(browser("mediaDevices"))?
    .get_user_media_with_constraints(&constraints)
    .map_err(browser("getUserMedia"))?;
  Ok(
    JsFuture::from(promise)
      .await
      .map_err(browser("getUserMedia"))?
      .unchecked_into(),
  )
}

/// Asks the user what to share (`getDisplayMedia`). Needs a recent user gesture.
pub(super) async fn capture_screen() -> Result<MediaStream, WebCallError> {
  let video = dictionary(&[
    ("frameRate", maximum(u32::from(SCREEN_FRAMERATE))?),
    ("width", maximum(SCREEN_MAX_WIDTH)?),
    ("height", maximum(SCREEN_MAX_HEIGHT)?),
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

fn ideal(value: u32) -> Result<JsValue, WebCallError> {
  constraint("ideal", value)
}

fn maximum(value: u32) -> Result<JsValue, WebCallError> {
  constraint("max", value)
}

fn constraint(name: &str, value: u32) -> Result<JsValue, WebCallError> {
  dictionary(&[(name, JsValue::from(value))])
    .map(JsValue::from)
    .map_err(browser("media constraints"))
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

/// Background effects in the browser: the picture goes through a canvas into I420, through the
/// shared effects pipeline (WebGPU) and back into a `VideoFrame` for the encoders.
#[cfg(feature = "effects")]
mod effects {
  use js_sys::{Object, Uint8Array};
  use wasm_bindgen::JsCast;
  use wasm_bindgen::prelude::*;
  use web_sys::{HtmlCanvasElement, HtmlVideoElement};

  use super::super::{WebCallError, browser, dictionary, window};
  use crate::capture_convert::{PixelFormat, RawPicture};
  use crate::effects::EffectsPipeline;
  use crate::web_api::{Canvas2d, VideoFrame as WebVideoFrame};

  pub(in super::super) struct WebEffects {
    pub pipeline: EffectsPipeline,
    canvas: HtmlCanvasElement,
    context: Canvas2d,
  }

  impl WebEffects {
    pub fn new(pipeline: EffectsPipeline) -> Result<Self, WebCallError> {
      let canvas: HtmlCanvasElement = window()?
        .document()
        .ok_or(WebCallError::Unsupported("a document"))?
        .create_element("canvas")
        .map_err(browser("canvas"))?
        .unchecked_into();
      let options = dictionary(&[("willReadFrequently", JsValue::TRUE)]).map_err(browser("canvas"))?;
      let context: Canvas2d = canvas
        .get_context_with_context_options("2d", &options)
        .map_err(browser("canvas"))?
        .ok_or(WebCallError::Unsupported("a 2D canvas"))?
        .unchecked_into();
      Ok(Self {
        pipeline,
        canvas,
        context,
      })
    }

    /// The current picture with the effect applied, as an I420 `VideoFrame`.
    pub async fn frame(
      &mut self,
      video: &HtmlVideoElement,
      timestamp_us: f64,
    ) -> Result<WebVideoFrame, WebCallError> {
      // Effects need a width that is a multiple of 8 and even heights (I420).
      let width = video.video_width() & !7;
      let height = video.video_height() & !1;
      if self.canvas.width() != width || self.canvas.height() != height {
        self.canvas.set_width(width);
        self.canvas.set_height(height);
      }
      let (w, h) = (f64::from(width), f64::from(height));
      self
        .context
        .draw_video(video, 0.0, 0.0, w, h)
        .map_err(browser("canvas"))?;
      let pixels = self
        .context
        .get_image_data(0.0, 0.0, w, h)
        .map_err(browser("canvas"))?
        .data();
      let picture = RawPicture {
        format: PixelFormat::Rgbx,
        width,
        height,
        stride: 0,
        data: &pixels,
      };
      let frame = picture
        .to_i420(timestamp_us as u64)
        .map_err(|error| WebCallError::Browser {
          context: "effects",
          message: error.to_string(),
        })?;
      let frame = self
        .pipeline
        .apply(frame)
        .await
        .map_err(|error| WebCallError::Browser {
          context: "effects",
          message: error.to_string(),
        })?;
      let init: Object = dictionary(&[
        ("format", JsValue::from_str("I420")),
        ("codedWidth", JsValue::from(width)),
        ("codedHeight", JsValue::from(height)),
        ("timestamp", JsValue::from(timestamp_us)),
      ])
      .map_err(browser("VideoFrame"))?;
      WebVideoFrame::new(&Uint8Array::from(frame.data()).into(), &init).map_err(browser("VideoFrame"))
    }
  }
}
