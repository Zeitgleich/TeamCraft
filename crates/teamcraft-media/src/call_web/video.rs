//! Video in the browser: camera (`getUserMedia`, three simulcast layers, optional background
//! effects) and screen share (`getDisplayMedia`, one layer) → WebCodecs H.264 → the engine; other
//! devices' tracks decoded into canvases the app provides.
//!
//! H.264 Constrained Baseline in Annex B, like the native encoder, so browsers and desktops decode
//! each other. A `<video>` element plays the captured stream and frames are taken from it at a
//! fixed rate, which works in every browser with WebCodecs (Firefox has no
//! MediaStreamTrackProcessor, Safari has it only in workers). The rate is kept by the call's
//! [`Cadence`] on the audio clock's pulses, not by a page timer, so a screen shared from a hidden
//! tab keeps its frame rate (issue #923). Each simulcast layer has its own encoder configured at the
//! layer's size; WebCodecs scales the frame to it. When the source changes shape for good (a shared
//! window resized), every layer gets a new encoder at the new shape ([`LayerSizer`], #931), so the
//! picture isn't stretched.
//!
//! A watched track's canvas reports its shown size ([`SizeWatch`]), from which the call picks the
//! layer to receive ([`crate::video::layer_for_view`]).
//!
//! A codec error stays with its track or tile, never ends the call (issue #919): WebCodecs closes
//! a failed encoder or decoder for good, so a failed encoder is replaced by a new one that starts
//! with a keyframe, and a failed decoder is dropped and a keyframe asked for, from which a new one
//! starts. Only encoders that keep failing stop their track.

use futures::channel::mpsc;
use js_sys::{Promise, Uint8Array};
use teamcraft_proto::{MediaStreamId, ScreenContentMode, VideoCodec, VideoLayerInfo, VideoTrackKind};
use tracing::{debug, warn};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
  DisplayMediaStreamConstraints, HtmlCanvasElement, HtmlVideoElement, MediaStream, MediaStreamConstraints,
  MediaStreamTrack, MediaTrackConstraints,
};
use web_time::Instant;

use super::cadence::Cadence;
use super::{
  Input, MediaAccessReason, MediaInput, WebCallError, browser, dictionary, js_message, media_access, window,
};
use crate::call_video::ReceivedVideoFrame;
use crate::video::{
  LayerSizer, VideoResolution, fit_within, screen_bitrate_bps, screen_limits, simulcast_layers,
};
use crate::web_api::{
  Canvas2d, EncodedVideoChunk, VideoDecoder, VideoEncoder, VideoFrame as WebVideoFrame, count_picture_drawn,
};

/// WebCodecs codec string: H.264 Constrained Baseline, level 4.0 (up to 1080p).
const H264_CODEC: &str = "avc1.42e028";
/// WebCodecs codec string: VP9 profile 0 (older browser clients).
const VP9_CODEC: &str = "vp09.00.10.08";
const CAMERA_WIDTH: u32 = 1280;
const CAMERA_HEIGHT: u32 = 720;
const CAMERA_FRAMERATE: u8 = 30;
const CAMERA_BITRATE_BPS: u32 = 1_500_000;
/// A keyframe at least this often (seconds), so a lost frame doesn't freeze the picture for long.
const KEYFRAME_INTERVAL_SECONDS: u32 = 4;
/// Frames waiting in an encoder above this are skipped (slow machine or encoder).
const MAX_ENCODE_QUEUE: u32 = 2;
/// A layer's encoder that failed is replaced this many times in a row (without an encoded frame in
/// between); failing once more stops the track: the browser can't encode it.
const MAX_ENCODER_RESTARTS: u8 = 3;
/// WebCodecs' state of an encoder or decoder that takes frames.
const CONFIGURED: &str = "configured";

/// One own track: capture, optional effects, an encoder per layer.
pub(super) struct TrackSender {
  pub publication: Publication,
  stream: MediaStream,
  video: HtmlVideoElement,
  encoders: LayerEncoders<LayerEncoder>,
  /// Follows the source's size with the layers' (a shared window resized).
  sizer: LayerSizer,
  framerate: u8,
  frames_since_keyframe: u32,
  timestamp_us: f64,
  /// Pictures in a row that couldn't be taken; only the first of a run is a warning.
  failed_pictures_in_a_row: u32,
  #[cfg(feature = "effects")]
  maybe_effects: Option<effects::WebEffects>,
  /// When the next picture is due, at the track's frame rate.
  cadence: Cadence,
  _ended: Closure<dyn FnMut()>,
}

/// Which SFU stream an own track is sent on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Publication {
  pub kind: VideoTrackKind,
  /// `None` until the SFU published the track, and again once it dropped it (a rejoin without the
  /// slot) until the engine's new publication is confirmed.
  pub maybe_stream_id: Option<MediaStreamId>,
}

/// The encoders of one own track, one per simulcast layer. A failed encoder is replaced by a new
/// one that starts with a keyframe; a layer whose encoders keep failing gives up.
pub(super) struct LayerEncoders<E> {
  layers: Vec<Layer<E>>,
  open: EncoderOpener<E>,
  next_generation: u64,
}

/// Opens an encoder for a layer; the number (generation) tells its failure reports from those of
/// the encoders it replaced.
type EncoderOpener<E> = Box<dyn FnMut(VideoLayerInfo, u64) -> Result<E, WebCallError>>;

struct Layer<E> {
  info: VideoLayerInfo,
  encoder: E,
  generation: u64,
  is_needed: bool,
  wants_keyframe: bool,
  restarts_in_a_row: u8,
}

/// One layer's encoder: WebCodecs in the browser, a fake in tests.
pub(super) trait FrameEncoder {
  type Frame;

  /// Whether it takes frames: WebCodecs closes an encoder for good after an error.
  fn is_configured(&self) -> bool;
  fn queue_size(&self) -> u32;
  fn encode(&self, frame: &Self::Frame, is_keyframe: bool) -> Result<(), WebCallError>;
}

/// A layer's encoders kept failing: the browser can't encode the track, which stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct EncoderGaveUp {
  pub layer: u8,
}

/// Reports a watched track's canvas size as [`Input::VideoResized`] when it changes (the tile is
/// laid out, enlarged, or the window resized); stops on drop.
pub(super) struct SizeWatch {
  observer: web_sys::ResizeObserver,
  _on_resize: Closure<dyn FnMut()>,
}

/// A WebCodecs H.264 encoder for one layer; closed on drop.
struct LayerEncoder {
  encoder: VideoEncoder,
  _output: Closure<dyn FnMut(EncodedVideoChunk)>,
  _error: Closure<dyn FnMut(JsValue)>,
}

/// Decodes one remote track into a canvas. A decoder is opened at a keyframe, for that frame's
/// codec; a failed one is dropped, and the next keyframe opens a new one.
pub(super) struct VideoView<D = Decoder> {
  stream_id: MediaStreamId,
  open: DecoderOpener<D>,
  maybe_decoder: Option<OpenDecoder<D>>,
  next_generation: u64,
}

/// Opens a decoder for a codec; the number (generation) tells its failure reports from those of
/// the decoders it replaced.
type DecoderOpener<D> = Box<dyn FnMut(&VideoCodec, u64) -> Result<D, WebCallError>>;

struct OpenDecoder<D> {
  generation: u64,
  decoder: D,
}

/// A tile's decoder: WebCodecs in the browser, a fake in tests.
pub(super) trait FrameDecoder {
  fn codec(&self) -> &VideoCodec;
  /// Whether it takes frames: WebCodecs closes a decoder for good after an error.
  fn is_configured(&self) -> bool;
  fn decode(&self, frame: &ReceivedVideoFrame) -> Result<(), WebCallError>;
}

/// A WebCodecs decoder drawing into a tile's canvas; closed on drop.
pub(super) struct Decoder {
  codec: VideoCodec,
  decoder: VideoDecoder,
  _output: Closure<dyn FnMut(WebVideoFrame)>,
  _error: Closure<dyn FnMut(JsValue)>,
}

/// What [`TrackSender::start`] needs besides the stream.
pub(super) struct TrackPlan {
  pub kind: VideoTrackKind,
  pub framerate: u8,
  pub bitrate_bps: u32,
  pub max_layers: u8,
  /// What is sent never exceeds this size, whatever the camera delivers (4K cameras); the
  /// encoders scale. `None`: the source size.
  pub maybe_max_resolution: Option<VideoResolution>,
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
    let limit = plan.maybe_max_resolution.unwrap_or(resolution);
    let resolution = fit_within(resolution, limit);
    let video = play_hidden(&stream).await?;
    let infos = simulcast_layers(resolution, plan.framerate, plan.bitrate_bps, plan.max_layers);
    let sizer = LayerSizer::new(infos.clone(), limit);
    let (kind, framerate, encoder_inputs) = (plan.kind.clone(), plan.framerate, inputs.clone());
    let encoders = LayerEncoders::new(infos, move |info, generation| {
      LayerEncoder::new(info, kind.clone(), framerate, generation, &encoder_inputs)
    })?;
    // "Stop sharing" in the browser's own UI (or an unplugged camera) ends the track.
    let ended_sender = inputs.clone();
    let ended_kind = plan.kind.clone();
    let ended = Closure::<dyn FnMut()>::new(move || {
      let _ = ended_sender.unbounded_send(Input::VideoEnded(ended_kind.clone()));
    });
    track.set_onended(Some(ended.as_ref().unchecked_ref()));
    Ok(Self {
      publication: Publication {
        kind: plan.kind,
        maybe_stream_id: None,
      },
      stream,
      video,
      encoders,
      sizer,
      framerate: plan.framerate,
      frames_since_keyframe: 0,
      timestamp_us: 0.0,
      failed_pictures_in_a_row: 0,
      #[cfg(feature = "effects")]
      maybe_effects: plan.maybe_effects.map(effects::WebEffects::new).transpose()?,
      cadence: Cadence::per_second(plan.framerate, Instant::now()),
      _ended: ended,
    })
  }

  pub fn layers(&self) -> Vec<VideoLayerInfo> {
    self.encoders.infos()
  }

  /// Whether the next picture is due at `now` ([`TrackSender::capture`] it then).
  pub fn is_due(&mut self, now: Instant) -> bool {
    self.cadence.due(now) > 0
  }

  pub fn request_keyframe(&mut self, layer: u8) {
    self.encoders.request_keyframe(layer);
  }

  /// Every layer's next frame is a keyframe (the track is sent on a new stream).
  pub fn request_keyframes(&mut self) {
    self.encoders.request_keyframes();
  }

  /// Only `needed` layers are encoded (the SFU's `LayersNeeded`); a resumed layer starts with a
  /// keyframe.
  pub fn set_needed_layers(&mut self, needed: &[u8]) {
    self.encoders.set_needed_layers(needed);
  }

  #[cfg(feature = "effects")]
  pub fn set_effect(&mut self, effect: crate::effects::BackgroundEffect) {
    if let Some(effects) = &mut self.maybe_effects {
      effects.pipeline.set_effect(effect);
    }
  }

  /// Encodes the current picture on every needed layer; skipped while nothing plays yet or the
  /// track has no stream to go out on. A picture that can't be taken is skipped; only encoders
  /// that keep failing end the track.
  pub async fn capture(&mut self) -> Result<(), EncoderGaveUp> {
    if self.publication.maybe_stream_id.is_none() || self.video.video_width() == 0 {
      return Ok(());
    }
    self.follow_source_size()?;
    let frame = match self.next_frame().await {
      Ok(frame) => frame,
      Err(error) => {
        self.on_picture_failed(&error);
        return Ok(());
      },
    };
    self.failed_pictures_in_a_row = 0;
    self.timestamp_us += 1_000_000.0 / f64::from(self.framerate.max(1));
    let is_periodic_keyframe =
      self.frames_since_keyframe >= KEYFRAME_INTERVAL_SECONDS * u32::from(self.framerate);
    self.frames_since_keyframe = if is_periodic_keyframe {
      0
    } else {
      self.frames_since_keyframe + 1
    };
    let result = self.encoders.encode(&frame, is_periodic_keyframe);
    frame.close();
    result
  }

  /// A layer's encoder produced a frame: it works.
  pub fn on_encoded(&mut self, layer: u8) {
    self.encoders.on_encoded(layer);
  }

  /// WebCodecs reported that an encoder failed (and closed it).
  pub fn on_encoder_failed(&mut self, layer: u8, generation: u64) -> Result<(), EncoderGaveUp> {
    self.encoders.on_failed(layer, generation)
  }

  /// New encoders at the source's size once it changed for good (a shared window resized or
  /// turned), so the picture keeps its shape instead of being stretched into the old size.
  fn follow_source_size(&mut self) -> Result<(), EncoderGaveUp> {
    let side = |pixels: u32| u16::try_from(pixels).unwrap_or(u16::MAX);
    let source = VideoResolution {
      width: side(self.video.video_width()),
      height: side(self.video.video_height()),
    };
    let Some(infos) = self.sizer.observe(source, Instant::now()) else {
      return Ok(());
    };
    debug!(kind = ?self.publication.kind, width = source.width, height = source.height, "the video source changed size; new encoders");
    self.encoders.resize(&infos)
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

  fn on_picture_failed(&mut self, error: &WebCallError) {
    self.failed_pictures_in_a_row = self.failed_pictures_in_a_row.saturating_add(1);
    let kind = &self.publication.kind;
    if self.failed_pictures_in_a_row == 1 {
      warn!(%error, ?kind, "taking a video picture failed; skipping it");
    } else {
      debug!(%error, ?kind, in_a_row = self.failed_pictures_in_a_row, "taking a video picture failed");
    }
  }

  /// Stops capturing; the encoders close as they drop.
  pub fn stop(self) {
    for track in self.stream.get_tracks().iter() {
      track.unchecked_into::<MediaStreamTrack>().stop();
    }
    self.video.set_src_object(None);
  }
}

impl<E: FrameEncoder> LayerEncoders<E> {
  /// Opens an encoder for every layer; every layer is needed and starts with a keyframe.
  pub fn new(
    infos: Vec<VideoLayerInfo>,
    mut open: impl FnMut(VideoLayerInfo, u64) -> Result<E, WebCallError> + 'static,
  ) -> Result<Self, WebCallError> {
    let layers = infos
      .into_iter()
      .zip(0..)
      .map(|(info, generation)| {
        Ok(Layer {
          info,
          encoder: open(info, generation)?,
          generation,
          is_needed: true,
          wants_keyframe: true,
          restarts_in_a_row: 0,
        })
      })
      .collect::<Result<Vec<_>, WebCallError>>()?;
    Ok(Self {
      next_generation: layers.len() as u64,
      layers,
      open: Box::new(open),
    })
  }

  pub fn infos(&self) -> Vec<VideoLayerInfo> {
    self.layers.iter().map(|layer| layer.info).collect()
  }

  pub fn request_keyframe(&mut self, layer: u8) {
    if let Some(layer) = self.layer_mut(layer) {
      layer.wants_keyframe = true;
    }
  }

  pub fn request_keyframes(&mut self) {
    for layer in &mut self.layers {
      layer.wants_keyframe = true;
    }
  }

  pub fn set_needed_layers(&mut self, needed: &[u8]) {
    for layer in &mut self.layers {
      let is_needed = needed.contains(&layer.info.layer);
      layer.wants_keyframe |= is_needed && !layer.is_needed;
      layer.is_needed = is_needed;
    }
  }

  /// Encodes `frame` on every needed layer whose encoder has room. A failed encoder is replaced
  /// (the new one starts with the next frame, a keyframe); the error is only a layer that gave up.
  pub fn encode(&mut self, frame: &E::Frame, is_periodic_keyframe: bool) -> Result<(), EncoderGaveUp> {
    for index in 0..self.layers.len() {
      let layer = &mut self.layers[index];
      if !layer.is_needed {
        continue;
      }
      // Closed after an error whose report hasn't arrived yet (that report is stale then).
      if !layer.encoder.is_configured() {
        debug!(layer = layer.info.layer, "video encoder closed after an error");
        self.restart(index)?;
        continue;
      }
      if layer.encoder.queue_size() > MAX_ENCODE_QUEUE {
        continue;
      }
      let is_keyframe = std::mem::take(&mut layer.wants_keyframe) || is_periodic_keyframe;
      if let Err(error) = layer.encoder.encode(frame, is_keyframe) {
        warn!(%error, layer = layer.info.layer, "encoding a video frame failed");
        self.restart(index)?;
      }
    }
    Ok(())
  }

  /// The same layers at a new size (`infos`; the source changed size): every layer gets a new
  /// encoder that starts with a keyframe. One that fails to open is replaced as after a failure.
  pub fn resize(&mut self, infos: &[VideoLayerInfo]) -> Result<(), EncoderGaveUp> {
    for index in 0..self.layers.len() {
      let layer = &mut self.layers[index];
      let Some(info) = infos.iter().find(|info| info.layer == layer.info.layer) else {
        continue;
      };
      layer.info = *info;
      let generation = self.next_generation;
      self.next_generation += 1;
      match (self.open)(*info, generation) {
        Ok(encoder) => {
          layer.encoder = encoder;
          layer.generation = generation;
          layer.wants_keyframe = true;
        },
        Err(error) => {
          warn!(%error, layer = info.layer, "opening a video encoder at the new size failed");
          self.restart(index)?;
        },
      }
    }
    Ok(())
  }

  /// A layer's encoder produced a frame: it works, so a later failure may restart it again.
  pub fn on_encoded(&mut self, layer: u8) {
    if let Some(layer) = self.layer_mut(layer) {
      layer.restarts_in_a_row = 0;
    }
  }

  /// WebCodecs reported an encoder's failure; the report of an encoder already replaced is stale.
  pub fn on_failed(&mut self, layer: u8, generation: u64) -> Result<(), EncoderGaveUp> {
    match self
      .layers
      .iter()
      .position(|current| current.info.layer == layer && current.generation == generation)
    {
      Some(index) => self.restart(index),
      None => Ok(()),
    }
  }

  /// Replaces a layer's failed encoder; after too many restarts in a row the layer gives up.
  fn restart(&mut self, index: usize) -> Result<(), EncoderGaveUp> {
    let layer = &mut self.layers[index];
    loop {
      if layer.restarts_in_a_row >= MAX_ENCODER_RESTARTS {
        return Err(EncoderGaveUp {
          layer: layer.info.layer,
        });
      }
      layer.restarts_in_a_row += 1;
      let generation = self.next_generation;
      self.next_generation += 1;
      match (self.open)(layer.info, generation) {
        Ok(encoder) => {
          debug!(layer = layer.info.layer, "started a new video encoder");
          layer.encoder = encoder;
          layer.generation = generation;
          layer.wants_keyframe = true;
          return Ok(());
        },
        Err(error) => warn!(%error, layer = layer.info.layer, "starting a new video encoder failed"),
      }
    }
  }

  fn layer_mut(&mut self, layer: u8) -> Option<&mut Layer<E>> {
    self.layers.iter_mut().find(|current| current.info.layer == layer)
  }
}

impl SizeWatch {
  pub fn start(
    stream_id: MediaStreamId,
    canvas: &HtmlCanvasElement,
    inputs: &mpsc::UnboundedSender<Input>,
  ) -> Result<Self, WebCallError> {
    let sender = inputs.clone();
    let observed = canvas.clone();
    let on_resize = Closure::<dyn FnMut()>::new(move || {
      let shown = shown_size(&observed);
      // The call task may be gone; the watch stops when the tile does.
      let _ = sender.unbounded_send(Input::VideoResized { stream_id, shown });
    });
    let observer =
      web_sys::ResizeObserver::new(on_resize.as_ref().unchecked_ref()).map_err(browser("ResizeObserver"))?;
    observer.observe(canvas);
    Ok(Self {
      observer,
      _on_resize: on_resize,
    })
  }
}

impl Drop for SizeWatch {
  fn drop(&mut self) {
    self.observer.disconnect();
  }
}

impl LayerEncoder {
  fn new(
    info: VideoLayerInfo,
    kind: VideoTrackKind,
    framerate: u8,
    generation: u64,
    inputs: &mpsc::UnboundedSender<Input>,
  ) -> Result<Self, WebCallError> {
    let sender = inputs.clone();
    let layer = info.layer;
    let output_kind = kind.clone();
    let output = Closure::<dyn FnMut(EncodedVideoChunk)>::new(move |chunk: EncodedVideoChunk| {
      let bytes = Uint8Array::new_with_length(chunk.byte_length());
      let input = match chunk.copy_to(&bytes) {
        Ok(()) => Input::VideoEncoded {
          kind: output_kind.clone(),
          layer,
          data: bytes.to_vec(),
          is_keyframe: chunk.chunk_type() == "key",
          timestamp_us: chunk.timestamp(),
        },
        Err(error) => Input::CodecFailed {
          context: "video encoder output",
          message: js_message(&error),
        },
      };
      let _ = sender.unbounded_send(input);
    });
    // WebCodecs closed the encoder for good: the track replaces it.
    let failure_sender = inputs.clone();
    let error = Closure::<dyn FnMut(JsValue)>::new(move |error: JsValue| {
      let _ = failure_sender.unbounded_send(Input::VideoEncodeFailed {
        kind: kind.clone(),
        layer,
        generation,
        message: js_message(&error),
      });
    });
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
      encoder,
      _output: output,
      _error: error,
    })
  }
}

impl FrameEncoder for LayerEncoder {
  type Frame = WebVideoFrame;

  fn is_configured(&self) -> bool {
    self.encoder.state() == CONFIGURED
  }

  fn queue_size(&self) -> u32 {
    self.encoder.encode_queue_size()
  }

  fn encode(&self, frame: &WebVideoFrame, is_keyframe: bool) -> Result<(), WebCallError> {
    let options =
      dictionary(&[("keyFrame", JsValue::from_bool(is_keyframe))]).map_err(browser("VideoEncoder"))?;
    self
      .encoder
      .encode(frame, &options)
      .map_err(browser("VideoEncoder"))
  }
}

impl Drop for LayerEncoder {
  fn drop(&mut self) {
    // Closing an encoder that already failed throws; nothing left to release then.
    let _ = self.encoder.close();
  }
}

impl VideoView {
  pub fn new(
    stream_id: MediaStreamId,
    canvas: HtmlCanvasElement,
    inputs: &mpsc::UnboundedSender<Input>,
  ) -> Self {
    let inputs = inputs.clone();
    Self::with_decoders(stream_id, move |codec, generation| {
      Decoder::new(stream_id, codec, generation, canvas.clone(), &inputs)
    })
  }
}

impl<D: FrameDecoder> VideoView<D> {
  pub fn with_decoders(
    stream_id: MediaStreamId,
    open: impl FnMut(&VideoCodec, u64) -> Result<D, WebCallError> + 'static,
  ) -> Self {
    Self {
      stream_id,
      open: Box::new(open),
      maybe_decoder: None,
      next_generation: 0,
    }
  }

  /// Decodes a frame; deltas without a decoder are skipped until a keyframe opens one. An error
  /// means this tile's decoder failed and is gone: the caller asks for a keyframe.
  pub fn decode(&mut self, frame: &ReceivedVideoFrame) -> Result<(), WebCallError> {
    let maybe_failure = self.drop_unusable_decoder(&frame.codec);
    if self.maybe_decoder.is_none() {
      if !frame.is_keyframe {
        return maybe_failure.map_or(Ok(()), Err);
      }
      let generation = self.next_generation;
      self.next_generation += 1;
      let decoder = (self.open)(&frame.codec, generation)?;
      self.maybe_decoder = Some(OpenDecoder { generation, decoder });
    }
    let Some(open) = &self.maybe_decoder else {
      return Ok(());
    };
    if let Err(error) = open.decoder.decode(frame) {
      self.maybe_decoder = None;
      return Err(error);
    }
    Ok(())
  }

  /// WebCodecs reported a decoder's failure. Returns whether it was the current decoder, which is
  /// dropped then (the caller asks for a keyframe); the report of a decoder already replaced is
  /// stale.
  pub fn on_decoder_failed(&mut self, generation: u64) -> bool {
    let is_current = self
      .maybe_decoder
      .as_ref()
      .is_some_and(|open| open.generation == generation);
    if is_current {
      self.maybe_decoder = None;
    }
    is_current
  }

  /// Drops a decoder WebCodecs closed after an error (returned as the failure: its report may
  /// still be on the way) or one for another codec.
  fn drop_unusable_decoder(&mut self, codec: &VideoCodec) -> Option<WebCallError> {
    let open = self.maybe_decoder.as_ref()?;
    if !open.decoder.is_configured() {
      self.maybe_decoder = None;
      return Some(WebCallError::Browser {
        context: "VideoDecoder",
        message: format!("track {} decoder closed after an error", self.stream_id.0),
      });
    }
    if open.decoder.codec() != codec {
      self.maybe_decoder = None;
    }
    None
  }
}

impl Decoder {
  fn new(
    stream_id: MediaStreamId,
    codec: &VideoCodec,
    generation: u64,
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
      match context.draw_video_frame(&frame, 0.0, 0.0) {
        // The page tells a tile that stopped getting pictures by this count (#921).
        Ok(()) => count_picture_drawn(&canvas),
        Err(error) => {
          let _ = error_sender.unbounded_send(Input::CodecFailed {
            context: "drawing a video frame",
            message: js_message(&error),
          });
        },
      }
      frame.close();
    });
    // A failed decoder is closed for good: the view drops it and asks for a keyframe, from which a
    // new one starts.
    let failure_sender = inputs.clone();
    let error = Closure::<dyn FnMut(JsValue)>::new(move |error: JsValue| {
      let _ = failure_sender.unbounded_send(Input::VideoDecodeFailed {
        stream_id,
        generation,
        message: js_message(&error),
      });
    });
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
      _output: output,
      _error: error,
    })
  }
}

impl FrameDecoder for Decoder {
  fn codec(&self) -> &VideoCodec {
    &self.codec
  }

  fn is_configured(&self) -> bool {
    self.decoder.state() == CONFIGURED
  }

  fn decode(&self, frame: &ReceivedVideoFrame) -> Result<(), WebCallError> {
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

impl Drop for Decoder {
  fn drop(&mut self) {
    let _ = self.decoder.close();
  }
}

/// How large `canvas` is shown, in device pixels (CSS size × `devicePixelRatio`); zero while it
/// isn't laid out.
pub(super) fn shown_size(canvas: &HtmlCanvasElement) -> VideoResolution {
  let ratio = window().map_or(1.0, |window| window.device_pixel_ratio());
  let side = |css: i32| (f64::from(css.max(0)) * ratio).round().min(f64::from(u16::MAX)) as u16;
  VideoResolution {
    width: side(canvas.client_width()),
    height: side(canvas.client_height()),
  }
}

/// The plan for a camera: three simulcast layers from 720p at 30 fps.
pub(super) fn camera_plan() -> TrackPlan {
  TrackPlan {
    kind: VideoTrackKind::Camera,
    framerate: CAMERA_FRAMERATE,
    bitrate_bps: CAMERA_BITRATE_BPS,
    max_layers: 3,
    maybe_max_resolution: Some(VideoResolution {
      width: CAMERA_WIDTH as u16,
      height: CAMERA_HEIGHT as u16,
    }),
    #[cfg(feature = "effects")]
    maybe_effects: None,
  }
}

/// The plan for a screen share: the mode's largest picture and rate (1440p30 for detail, 1080p60
/// for motion) plus a half-size layer for weak links; a smaller source is sent as it is.
pub(super) fn screen_plan(mode: ScreenContentMode) -> TrackPlan {
  let (limit, framerate) = screen_limits(mode);
  TrackPlan {
    kind: VideoTrackKind::Screen,
    framerate,
    bitrate_bps: screen_bitrate_bps(limit, framerate),
    max_layers: 2,
    maybe_max_resolution: Some(limit),
    #[cfg(feature = "effects")]
    maybe_effects: None,
  }
}

/// Asks for the camera (`getUserMedia`); `maybe_device` is a `MediaDeviceInfo.deviceId`. Every
/// constraint is `ideal`, so no device is turned away for its size or rate (`OverconstrainedError`);
/// if the browser still fails (the chosen device is gone, or refuses the size), it is asked once
/// more for any camera with no constraints.
pub(super) async fn capture_camera(maybe_device: Option<&str>) -> Result<MediaStream, WebCallError> {
  let video = MediaTrackConstraints::new();
  video.set_width(&ideal(CAMERA_WIDTH)?);
  video.set_height(&ideal(CAMERA_HEIGHT)?);
  video.set_frame_rate(&ideal(u32::from(CAMERA_FRAMERATE))?);
  if let Some(device) = maybe_device {
    let preferred = dictionary(&[("ideal", JsValue::from_str(device))]).map_err(browser("getUserMedia"))?;
    video.set_device_id(&preferred.into());
  }
  let constraints = MediaStreamConstraints::new();
  constraints.set_video(&video);
  match request_camera(&constraints).await {
    Err(WebCallError::MediaAccess {
      reason: MediaAccessReason::NotFound | MediaAccessReason::InUse | MediaAccessReason::Other,
      ..
    }) => {
      let any = MediaStreamConstraints::new();
      any.set_video(&JsValue::TRUE);
      request_camera(&any).await
    },
    result => result,
  }
}

async fn request_camera(constraints: &MediaStreamConstraints) -> Result<MediaStream, WebCallError> {
  let promise = window()?
    .navigator()
    .media_devices()
    .map_err(|_| WebCallError::Unsupported(super::SECURE_CONTEXT))?
    .get_user_media_with_constraints(constraints)
    .map_err(media_access(MediaInput::Camera))?;
  Ok(
    JsFuture::from(promise)
      .await
      .map_err(media_access(MediaInput::Camera))?
      .unchecked_into(),
  )
}

/// Asks the user what to share (`getDisplayMedia`). Needs a recent user gesture.
pub(super) async fn capture_screen(mode: ScreenContentMode) -> Result<MediaStream, WebCallError> {
  picked_screen(request_screen(mode)?).await
}

/// Opens the screen picker now (synchronously, so a click's user activation still counts) and
/// returns `getDisplayMedia`'s promise.
pub(super) fn request_screen(mode: ScreenContentMode) -> Result<Promise, WebCallError> {
  let (limit, framerate) = screen_limits(mode);
  let video = dictionary(&[
    ("frameRate", maximum(u32::from(framerate))?),
    ("width", maximum(u32::from(limit.width))?),
    ("height", maximum(u32::from(limit.height))?),
  ])
  .map_err(browser("getDisplayMedia"))?;
  let constraints = DisplayMediaStreamConstraints::new();
  constraints.set_video(&video);
  window()?
    .navigator()
    .media_devices()
    .map_err(|_| WebCallError::Unsupported(super::SECURE_CONTEXT))?
    .get_display_media_with_constraints(&constraints)
    .map_err(media_access(MediaInput::Screen))
}

/// The stream of a picker [`request_screen`] opened, once the user chose.
pub(super) async fn picked_screen(request: Promise) -> Result<MediaStream, WebCallError> {
  Ok(
    JsFuture::from(request)
      .await
      .map_err(media_access(MediaInput::Screen))?
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

#[cfg(test)]
mod tests {
  use std::cell::RefCell;
  use std::collections::HashSet;
  use std::rc::Rc;

  use teamcraft_proto::{DeviceId, Id, Snowflake};

  use super::*;
  use crate::datagram::SimulcastLayer;
  use crate::rtp::RtpTimestamp;

  mod layer_encoders {
    use super::*;

    #[test]
    fn a_failed_encode_replaces_the_encoder_which_starts_with_a_keyframe() {
      let (bench, mut encoders) = encoders(2);
      encoders.encode(&(), false).unwrap();
      encoders.encode(&(), false).unwrap();
      bench.borrow_mut().throwing.insert(1);
      // The call goes on: the failed layer gets a new encoder.
      encoders.encode(&(), false).unwrap();
      assert_eq!(bench.borrow().opened, vec![(0, 0), (1, 1), (1, 2)]);
      encoders.encode(&(), false).unwrap();
      assert_eq!(
        bench.borrow().coded,
        vec![
          (0, 0, true),
          (1, 1, true),
          (0, 0, false),
          (1, 1, false),
          (0, 0, false),
          (0, 0, false),
          (1, 2, true),
        ]
      );
    }

    #[test]
    fn a_closed_encoder_is_replaced_before_its_report_which_is_then_stale() {
      let (bench, mut encoders) = encoders(1);
      encoders.encode(&(), false).unwrap();
      bench.borrow_mut().closed.insert(0);
      encoders.encode(&(), false).unwrap();
      assert_eq!(bench.borrow().opened, vec![(0, 0), (0, 1)]);
      // WebCodecs' report of the closed encoder arrives late: nothing more to replace.
      encoders.on_failed(0, 0).unwrap();
      assert_eq!(bench.borrow().opened.len(), 2);
      encoders.encode(&(), false).unwrap();
      assert_eq!(bench.borrow().coded, vec![(0, 0, true), (0, 1, true)]);
    }

    #[test]
    fn a_reported_failure_of_the_current_encoder_replaces_it() {
      let (bench, mut encoders) = encoders(2);
      encoders.encode(&(), false).unwrap();
      encoders.on_failed(1, 1).unwrap();
      encoders.encode(&(), false).unwrap();
      assert_eq!(bench.borrow().opened, vec![(0, 0), (1, 1), (1, 2)]);
      assert_eq!(
        bench.borrow().coded,
        vec![(0, 0, true), (1, 1, true), (0, 0, false), (1, 2, true)]
      );
    }

    #[test]
    fn encoders_that_keep_failing_give_up() {
      let (bench, mut encoders) = encoders(1);
      for generation in 0..u64::from(MAX_ENCODER_RESTARTS) {
        encoders.on_failed(0, generation).unwrap();
      }
      assert_eq!(
        encoders.on_failed(0, u64::from(MAX_ENCODER_RESTARTS)),
        Err(EncoderGaveUp { layer: 0 })
      );
      assert_eq!(bench.borrow().opened.len(), 1 + usize::from(MAX_ENCODER_RESTARTS));
    }

    #[test]
    fn an_encoded_frame_lets_a_later_failure_restart_again() {
      let (_bench, mut encoders) = encoders(1);
      for generation in 0..u64::from(MAX_ENCODER_RESTARTS) {
        encoders.on_failed(0, generation).unwrap();
      }
      encoders.on_encoded(0);
      assert_eq!(encoders.on_failed(0, u64::from(MAX_ENCODER_RESTARTS)), Ok(()));
    }

    #[test]
    fn an_encoder_that_fails_to_open_is_tried_again_within_the_limit() {
      let (bench, mut encoders) = encoders(1);
      bench.borrow_mut().failing_opens = usize::from(MAX_ENCODER_RESTARTS) - 1;
      encoders.on_failed(0, 0).unwrap();
      assert_eq!(bench.borrow().opened, vec![(0, 0), (0, 3)]);
      assert_eq!(encoders.on_failed(0, 3), Err(EncoderGaveUp { layer: 0 }));
    }

    #[test]
    fn a_resized_source_gets_new_encoders_at_its_size_starting_with_keyframes() {
      let (bench, mut encoders) = encoders(2);
      encoders.encode(&(), false).unwrap();
      let portrait = crate::video::resized_layers(
        &encoders.infos(),
        VideoResolution {
          width: 360,
          height: 640,
        },
      );
      encoders.resize(&portrait).unwrap();
      assert_eq!(encoders.infos(), portrait);
      assert_eq!(bench.borrow().opened, vec![(0, 0), (1, 1), (0, 2), (1, 3)]);
      encoders.encode(&(), false).unwrap();
      encoders.encode(&(), false).unwrap();
      assert_eq!(
        bench.borrow().coded[2..],
        [(0, 2, true), (1, 3, true), (0, 2, false), (1, 3, false)]
      );
      // A late failure report of a replaced encoder is stale.
      encoders.on_failed(1, 1).unwrap();
      assert_eq!(bench.borrow().opened.len(), 4);
    }

    #[test]
    fn a_resize_whose_encoder_fails_to_open_is_tried_again() {
      let (bench, mut encoders) = encoders(1);
      let infos = encoders.infos();
      bench.borrow_mut().failing_opens = 1;
      encoders.resize(&infos).unwrap();
      assert_eq!(bench.borrow().opened, vec![(0, 0), (0, 2)]);
      encoders.encode(&(), false).unwrap();
      assert_eq!(bench.borrow().coded, vec![(0, 2, true)]);
    }

    #[test]
    fn a_paused_layer_is_neither_encoded_nor_replaced() {
      let (bench, mut encoders) = encoders(2);
      encoders.set_needed_layers(&[1]);
      bench.borrow_mut().closed.insert(0);
      encoders.encode(&(), false).unwrap();
      assert_eq!(bench.borrow().opened, vec![(0, 0), (1, 1)]);
      assert_eq!(bench.borrow().coded, vec![(1, 1, true)]);
      // Needed again: the closed encoder is replaced, and the new one starts with a keyframe.
      encoders.set_needed_layers(&[0, 1]);
      encoders.encode(&(), false).unwrap();
      encoders.encode(&(), false).unwrap();
      assert_eq!(bench.borrow().opened, vec![(0, 0), (1, 1), (0, 2)]);
      assert!(bench.borrow().coded.contains(&(0, 2, true)));
    }
  }

  mod video_view {
    use super::*;

    #[test]
    fn deltas_wait_for_a_keyframe_to_open_a_decoder() {
      let (bench, mut view) = view();
      view.decode(&frame(VideoCodec::H264, false)).unwrap();
      assert!(bench.borrow().opened.is_empty());
      view.decode(&frame(VideoCodec::H264, true)).unwrap();
      view.decode(&frame(VideoCodec::H264, false)).unwrap();
      assert_eq!(bench.borrow().opened, vec![(VideoCodec::H264, 0)]);
      assert_eq!(decoded(&bench), vec![(0, true), (0, false)]);
    }

    #[test]
    fn a_failed_decode_drops_the_decoder_until_the_next_keyframe() {
      let (bench, mut view) = view();
      bench.borrow_mut().throwing.insert(0);
      assert!(view.decode(&frame(VideoCodec::H264, true)).is_err());
      view.decode(&frame(VideoCodec::H264, false)).unwrap();
      assert_eq!(bench.borrow().opened.len(), 1);
      view.decode(&frame(VideoCodec::H264, true)).unwrap();
      assert_eq!(
        bench.borrow().opened,
        vec![(VideoCodec::H264, 0), (VideoCodec::H264, 1)]
      );
      assert_eq!(decoded(&bench), vec![(1, true)]);
    }

    #[test]
    fn a_decoder_closed_before_its_report_fails_the_next_delta() {
      let (bench, mut view) = view();
      view.decode(&frame(VideoCodec::H264, true)).unwrap();
      bench.borrow_mut().closed.insert(0);
      // Never decoded on the closed decoder (which would throw): it's dropped and reported.
      assert!(view.decode(&frame(VideoCodec::H264, false)).is_err());
      assert_eq!(decoded(&bench), vec![(0, true)]);
      // Its report arrives late: stale.
      assert!(!view.on_decoder_failed(0));
    }

    #[test]
    fn a_decoder_closed_before_a_keyframe_is_replaced_by_it() {
      let (bench, mut view) = view();
      view.decode(&frame(VideoCodec::H264, true)).unwrap();
      bench.borrow_mut().closed.insert(0);
      view.decode(&frame(VideoCodec::H264, true)).unwrap();
      assert_eq!(decoded(&bench), vec![(0, true), (1, true)]);
    }

    #[test]
    fn only_the_current_decoders_failure_report_counts() {
      let (bench, mut view) = view();
      view.decode(&frame(VideoCodec::H264, true)).unwrap();
      assert!(!view.on_decoder_failed(7));
      view.decode(&frame(VideoCodec::H264, false)).unwrap();
      assert!(view.on_decoder_failed(0));
      view.decode(&frame(VideoCodec::H264, false)).unwrap();
      assert_eq!(decoded(&bench), vec![(0, true), (0, false)]);
    }

    #[test]
    fn a_keyframe_of_another_codec_opens_a_new_decoder() {
      let (bench, mut view) = view();
      view.decode(&frame(VideoCodec::H264, true)).unwrap();
      view.decode(&frame(VideoCodec::Vp9, false)).unwrap();
      view.decode(&frame(VideoCodec::Vp9, true)).unwrap();
      assert_eq!(
        bench.borrow().opened,
        vec![(VideoCodec::H264, 0), (VideoCodec::Vp9, 1)]
      );
      assert_eq!(decoded(&bench), vec![(0, true), (1, true)]);
    }

    #[test]
    fn a_decoder_that_fails_to_open_is_an_error() {
      let (bench, mut view) = view();
      bench.borrow_mut().failing_opens = 1;
      assert!(view.decode(&frame(VideoCodec::H264, true)).is_err());
      // The next keyframe tries again.
      view.decode(&frame(VideoCodec::H264, true)).unwrap();
      assert_eq!(decoded(&bench), vec![(1, true)]);
    }
  }

  /// What fake codecs did, and which of them WebCodecs "closed" or makes throw. Codecs are named
  /// by layer (encoders) or codec (decoders) and their generation.
  struct Bench<C> {
    opened: Vec<(C, u64)>,
    /// Name, generation, keyframe.
    coded: Vec<(C, u64, bool)>,
    closed: HashSet<u64>,
    throwing: HashSet<u64>,
    failing_opens: usize,
  }

  type Shared<C> = Rc<RefCell<Bench<C>>>;

  struct FakeEncoder {
    layer: u8,
    generation: u64,
    bench: Shared<u8>,
  }

  struct FakeDecoder {
    codec: VideoCodec,
    generation: u64,
    bench: Shared<VideoCodec>,
  }

  impl<C> Bench<C> {
    fn shared() -> Shared<C> {
      Rc::new(RefCell::new(Self {
        opened: Vec::new(),
        coded: Vec::new(),
        closed: HashSet::new(),
        throwing: HashSet::new(),
        failing_opens: 0,
      }))
    }

    /// Opens a codec unless an open is set to fail.
    fn open(&mut self, name: C, generation: u64) -> Result<(), WebCallError> {
      if self.failing_opens > 0 {
        self.failing_opens -= 1;
        return Err(failure("open"));
      }
      self.opened.push((name, generation));
      Ok(())
    }

    /// Codes a frame unless the codec throws (and is closed by it, like in WebCodecs).
    fn code(&mut self, name: C, generation: u64, is_keyframe: bool) -> Result<(), WebCallError> {
      if self.throwing.contains(&generation) {
        self.closed.insert(generation);
        return Err(failure("code"));
      }
      self.coded.push((name, generation, is_keyframe));
      Ok(())
    }
  }

  impl FrameEncoder for FakeEncoder {
    type Frame = ();

    fn is_configured(&self) -> bool {
      !self.bench.borrow().closed.contains(&self.generation)
    }

    fn queue_size(&self) -> u32 {
      0
    }

    fn encode(&self, _frame: &(), is_keyframe: bool) -> Result<(), WebCallError> {
      self
        .bench
        .borrow_mut()
        .code(self.layer, self.generation, is_keyframe)
    }
  }

  impl FrameDecoder for FakeDecoder {
    fn codec(&self) -> &VideoCodec {
      &self.codec
    }

    fn is_configured(&self) -> bool {
      !self.bench.borrow().closed.contains(&self.generation)
    }

    fn decode(&self, frame: &ReceivedVideoFrame) -> Result<(), WebCallError> {
      self
        .bench
        .borrow_mut()
        .code(self.codec.clone(), self.generation, frame.is_keyframe)
    }
  }

  fn encoders(layer_count: u8) -> (Shared<u8>, LayerEncoders<FakeEncoder>) {
    let bench = Bench::shared();
    let shared = Rc::clone(&bench);
    let infos = (0..layer_count).map(layer_info).collect();
    let encoders = LayerEncoders::new(infos, move |info, generation| {
      shared.borrow_mut().open(info.layer, generation)?;
      Ok(FakeEncoder {
        layer: info.layer,
        generation,
        bench: Rc::clone(&shared),
      })
    })
    .unwrap();
    (bench, encoders)
  }

  fn view() -> (Shared<VideoCodec>, VideoView<FakeDecoder>) {
    let bench = Bench::shared();
    let shared = Rc::clone(&bench);
    let view = VideoView::with_decoders(MediaStreamId(60), move |codec, generation| {
      shared.borrow_mut().open(codec.clone(), generation)?;
      Ok(FakeDecoder {
        codec: codec.clone(),
        generation,
        bench: Rc::clone(&shared),
      })
    });
    (bench, view)
  }

  /// Decoder generation and keyframe of each decoded frame.
  fn decoded(bench: &Shared<VideoCodec>) -> Vec<(u64, bool)> {
    bench
      .borrow()
      .coded
      .iter()
      .map(|(_, generation, is_keyframe)| (*generation, *is_keyframe))
      .collect()
  }

  fn layer_info(layer: u8) -> VideoLayerInfo {
    VideoLayerInfo {
      layer,
      width: 320 << layer,
      height: 180 << layer,
      max_framerate: 30,
      max_bitrate_bps: 150_000 << layer,
    }
  }

  fn frame(codec: VideoCodec, is_keyframe: bool) -> ReceivedVideoFrame {
    ReceivedVideoFrame {
      sender: DeviceId::from_snowflake(Snowflake::new(2).unwrap()),
      stream_id: MediaStreamId(60),
      codec,
      layer: SimulcastLayer::new(0).unwrap(),
      timestamp: RtpTimestamp(0),
      is_keyframe,
      data: vec![0, 0, 0, 1],
    }
  }

  fn failure(context: &'static str) -> WebCallError {
    WebCallError::Browser {
      context,
      message: "InvalidStateError".into(),
    }
  }
}
