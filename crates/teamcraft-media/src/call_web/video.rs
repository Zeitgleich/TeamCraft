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
//! A camera's encoders follow the engine's `LayerBitrates` (#939): on a limited uplink its top
//! sent layer is encoded at a share of its bitrate before it stops, as on the desktop. The running
//! encoder is configured again with only the bitrate changed, and no keyframe is asked for. Chrome
//! (measured with 152 on macOS: OpenH264, VideoToolbox and libaom) changes the running encoder's
//! rate within a few frames, without a keyframe and with at most one frame's pause; the spec leaves
//! it to the browser, and one that rebuilds its encoder for it marks the next chunk a keyframe
//! itself, so viewers decode it either way. An encoder that refuses the new configuration is
//! replaced at the new bitrate like a failed one.
//!
//! A camera's encoders are governed (#1762, [`crate::camera_governor`]): a layer whose encoder
//! sends far more than its bitrate is configured lower and then gets fewer frames (Chrome's
//! H.264 encoder skips none on its own, so on hard content a 320×180 layer sent ten times its
//! bitrate), and encoders that don't keep up lower the camera's frame rate, then leave its top
//! layers out. A screen share follows its own policy (#1733, [`crate::share_policy`]) on the same
//! load controller: its top layer's frame rate first, then its middle layer, its codec and size.
//!
//! A watched track's canvas reports its shown size ([`SizeWatch`]), from which the call picks the
//! layer to receive ([`crate::video::layer_for_view`]).
//!
//! A codec error stays with its track or tile, never ends the call (issue #919): WebCodecs closes
//! a failed encoder or decoder for good, so a failed encoder is replaced by a new one that starts
//! with a keyframe, and a failed decoder is dropped and a keyframe asked for, from which a new one
//! starts. Only encoders that keep failing stop their track.

use std::collections::VecDeque;

use futures::channel::mpsc;
use js_sys::{Promise, Uint8Array};
use teamcraft_proto::{
  MediaStreamId, ScreenContentMode, VideoCodec, VideoLayerInfo, VideoPublicationId, VideoTrackKind,
};
use tracing::{debug, info, warn};
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
use crate::call_engine::OwnPublication;
use crate::call_video::ReceivedVideoFrame;
use crate::camera_governor::{
  CameraGovernorConfig, CameraLoad, FrameTally, LayerLimit, LayerRateGuard, LoadStep, OverloadReason,
};
use crate::share_policy::{
  EncoderKind, ShareCodecs, ShareEncoders, ShareGovernor, ShareLimits, SharePlan, UplinkTop, share_framerate,
};
use crate::video::{
  CAMERA_FRAMERATE, LayerSizer, ScreenProfile, VideoResolution, camera_layers, fit_within, within_layer_limit,
};
use crate::web_api::{
  Canvas2d, EncodedVideoChunk, VideoDecoder, VideoEncoder, VideoFrame as WebVideoFrame, count_picture_drawn,
};

/// WebCodecs codec string: H.264 Constrained Baseline, level 4.0 (up to 1080p). Decoders take
/// larger pictures under it; encoders don't ([`h264_encoder_codec`]).
const H264_CODEC: &str = "avc1.42e028";
/// Constrained Baseline, level 5.1: up to 4K, for a screen share's top layer above 1080p.
const H264_CODEC_LEVEL_5_1: &str = "avc1.42e033";
/// The largest picture of level 4.0, in macroblocks of 16×16 pixels (H.264 table A-1, MaxFS).
const LEVEL_4_0_MAX_MACROBLOCKS: u32 = 8192;
/// WebCodecs codec string: VP9 profile 0, level 5.1, 8 bit: decoders take any picture a share
/// sends under it (#1733).
const VP9_CODEC: &str = "vp09.00.51.08";
/// VP9 level 4.1 (up to 2048×1088, #1733), 5.0 (4K at 30 fps) and 5.1 encoder strings.
const VP9_CODEC_LEVEL_4_1: &str = "vp09.00.41.08";
const VP9_CODEC_LEVEL_5_0: &str = "vp09.00.50.08";
/// Level 4.1's largest picture and level 5.0's samples per second (VP9 spec, annex A).
const VP9_LEVEL_4_1_MAX_PICTURE: u32 = 2_228_224;
const VP9_LEVEL_5_0_MAX_RATE: u64 = 311_951_360;
/// WebCodecs codec string: AV1 Main profile, level 5.0, 8 bit (#565): up to 4K at 30 fps and
/// 1440p at 60 fps, every picture a screen share sends. Decoders take it for any smaller picture.
const AV1_CODEC: &str = "av01.0.12M.08";
/// AV1 level 5.1, for a picture or rate above level 5.0's (not sent today; a larger source).
const AV1_CODEC_LEVEL_5_1: &str = "av01.0.13M.08";
/// Level 5.0's largest picture and its samples per second (AV1 spec, annex A.3).
const AV1_LEVEL_5_0_MAX_PICTURE: u32 = 8_912_896;
const AV1_LEVEL_5_0_MAX_RATE: u64 = 267_386_880;
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
  /// What the encoders are built with, to build them anew in another codec (#565).
  encoding: Encoding,
  /// The codecs the track may go out in, best first (`ScreenProfile::codecs`; cameras H.264).
  codecs: Vec<VideoCodec>,
  /// Follows the source's size with the layers' (a shared window resized).
  sizer: LayerSizer,
  framerate: u8,
  frames_since_keyframe: u32,
  timestamp_us: f64,
  /// Pictures in a row that couldn't be taken; only the first of a run is a warning.
  failed_pictures_in_a_row: u32,
  /// Pictures taken from the source for encoding (stats).
  frames_captured: u64,
  #[cfg(feature = "effects")]
  maybe_effects: Option<effects::WebEffects>,
  /// When the next picture is due, at the track's frame rate.
  cadence: Cadence,
  /// A camera's encoders against the frames they get (#1762); `None` for a screen share.
  maybe_load: Option<CameraLoad>,
  /// A screen share's policy and its encoders' load (#1733); `None` for a camera.
  maybe_share: Option<ShareGovernor>,
  /// The most layers the SFU takes (#978).
  max_layers: u8,
  /// The largest picture sent.
  limit: VideoResolution,
  /// The share's layers changed beyond what its encoders follow in place: it goes out anew.
  maybe_replan: Option<Replan>,
  /// When the share's top layer was given each of its last frames, by timestamp: its encode time
  /// is the wait for the chunk (#1733).
  top_encode_starts: VecDeque<(i64, Instant)>,
  _ended: Closure<dyn FnMut()>,
}

/// New layers for an own track, to be published anew ([`TrackSender::take_replan`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Replan {
  pub codec: VideoCodec,
  pub layers: Vec<VideoLayerInfo>,
}

/// Why an own camera sends less than its plan (#1762), for the stats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CameraLimits {
  pub planned: LoadStep,
  pub load: LoadStep,
  pub maybe_reason: Option<OverloadReason>,
  /// Layers whose encoder overshot: the layer, the bitrate it is encoded at and its frame rate.
  pub layers: Vec<(u8, u32, u8)>,
}

/// Which SFU stream an own track is sent on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Publication {
  pub kind: VideoTrackKind,
  /// The id the track is published under, and the stream the SFU confirmed for it (#929).
  pub own: OwnPublication,
}

/// The encoders of one own track, one per simulcast layer. A failed encoder is replaced by a new
/// one that starts with a keyframe; a layer whose encoders keep failing gives up.
pub(super) struct LayerEncoders<E> {
  layers: Vec<Layer<E>>,
  open: EncoderOpener<E>,
  next_generation: u64,
  /// Frames given to a layer's encoder, and those skipped because its queue was full (the effects'
  /// timing reports both, #1336).
  given: u64,
  skipped_queue_full: u64,
  /// Set for a camera ([`LayerEncoders::govern`]): each layer's output is held to its bitrate.
  maybe_governor: Option<CameraGovernorConfig>,
  /// The frame rate the track is captured at now.
  framerate: u8,
  /// Frames given so far: a layer that gets every n-th frame takes those it divides.
  frame_index: u64,
}

/// Opens an encoder for a layer; the number (generation) tells its failure reports from those of
/// the encoders it replaced.
type EncoderOpener<E> = Box<dyn FnMut(VideoLayerInfo, u64) -> Result<E, WebCallError>>;

struct Layer<E> {
  info: VideoLayerInfo,
  /// What the layer is encoded at: `info.max_bitrate_bps`, or less on a limited uplink (#939).
  bitrate_bps: u32,
  encoder: E,
  generation: u64,
  is_needed: bool,
  wants_keyframe: bool,
  restarts_in_a_row: u8,
  /// Its output against its bitrate, for a governed track.
  maybe_guard: Option<LayerRateGuard>,
  /// The next output is measured afresh: the layer paused or resumed, or its encoder changed.
  is_measure_stale: bool,
}

/// One layer's encoder: WebCodecs in the browser, a fake in tests.
pub(super) trait FrameEncoder {
  type Frame;

  /// Whether it takes frames: WebCodecs closes an encoder for good after an error.
  fn is_configured(&self) -> bool;
  fn queue_size(&self) -> u32;
  fn encode(&self, frame: &Self::Frame, is_keyframe: bool) -> Result<(), WebCallError>;
  /// Encodes the next frames at `bitrate_bps` and `framerate`, the running encoder configured
  /// again.
  fn configure(&mut self, bitrate_bps: u32, framerate: u8) -> Result<(), WebCallError>;
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

/// A WebCodecs encoder for one layer; closed on drop.
struct LayerEncoder {
  encoder: VideoEncoder,
  /// Its `VideoEncoderConfig`, configured again with another bitrate and frame rate (#939, #1762).
  config: js_sys::Object,
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
  /// Frames waiting to be decoded (#1762): a decoder that keeps up has taken the last one by the
  /// time the next comes.
  fn queue_size(&self) -> u32;
  fn decode(&self, frame: &ReceivedVideoFrame) -> Result<(), WebCallError>;
}

/// A WebCodecs decoder drawing into a tile's canvas; closed on drop.
pub(super) struct Decoder {
  codec: VideoCodec,
  decoder: VideoDecoder,
  _output: Closure<dyn FnMut(WebVideoFrame)>,
  _error: Closure<dyn FnMut(JsValue)>,
}

/// How a track's encoders are configured, whatever the layer.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Encoding {
  kind: VideoTrackKind,
  codec: VideoCodec,
  framerate: u8,
  latency: LatencyMode,
  /// `VideoEncoderConfig.contentHint` (#565): `"text"` turns on AV1's screen content tools.
  maybe_content_hint: Option<&'static str>,
}

/// What [`TrackSender::start`] needs besides the stream.
pub(super) struct TrackPlan {
  pub kind: VideoTrackKind,
  pub framerate: u8,
  pub shape: LayerShape,
  /// The codecs the track may go out in, best first; the call picks the first every viewer
  /// decodes ([`crate::call_engine::CallEngine::video_codec_for`]).
  pub codecs: Vec<VideoCodec>,
  /// What is sent never exceeds this size, whatever the camera delivers (4K cameras); the
  /// encoders scale. `None`: the source size.
  pub maybe_max_resolution: Option<VideoResolution>,
  #[cfg(feature = "effects")]
  pub maybe_effects: Option<CameraEffects>,
}

/// How a track's simulcast layers are cut from its picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LayerShape {
  /// A camera's layers (`video::camera_layers`: 1080p, 720p, 360p and 180p from a 1080p camera,
  /// #967, #978),
  /// encoded in the latency mode that gives them their bitrate ([`camera_latency_mode`]).
  Camera,
  /// A screen share's layers ([`crate::share_policy`], #1733): an H.264 base of at most 480p,
  /// a half-size middle one from 1440p, and the picture, the large ones in AV1 where this browser
  /// encodes it well and some viewer decodes it; stepped down under CPU pressure.
  Screen(ScreenProfile),
}

/// Frames of a share's top layer whose encode start is kept until its chunk comes back.
const SHARE_ENCODE_STARTS: usize = 64;

impl TrackSender {
  /// Starts encoding the stream's video track as `plan.kind`, to be published under `publication`
  /// in [`Self::codec`] with [`Self::layers`]. A camera's encoders encode the first of
  /// `codecs.by_all` (of `plan.codecs`); a share's layers follow its policy from `codecs` and
  /// `encoders` (#1733). At most `max_layers` layers (the top ones of the plan: what the SFU
  /// takes, #978).
  #[allow(clippy::too_many_arguments)]
  pub async fn start(
    stream: MediaStream,
    plan: TrackPlan,
    codecs: ShareCodecs,
    encoders: ShareEncoders,
    max_layers: u8,
    publication: VideoPublicationId,
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
    let now = Instant::now();
    let maybe_share = match &plan.shape {
      LayerShape::Camera => None,
      LayerShape::Screen(profile) => Some(ShareGovernor::new(
        SharePlan {
          source: resolution,
          framerate: plan.framerate,
          profile: profile.clone(),
          codecs: codecs.clone(),
          encoders,
        },
        now,
      )),
    };
    let (infos, latency, maybe_content_hint) = match (&plan.shape, &maybe_share) {
      (LayerShape::Screen(profile), Some(share)) => {
        (share.layers(), LatencyMode::Realtime, Some(content_hint(profile)))
      },
      _ => (
        camera_layers(resolution, plan.framerate, limit),
        camera_latency_mode(),
        None,
      ),
    };
    let codec = maybe_share.as_ref().map_or_else(
      || codecs.by_all.first().cloned().unwrap_or(VideoCodec::H264),
      ShareGovernor::track_codec,
    );
    let framerate = maybe_share
      .as_ref()
      .map_or(plan.framerate, |share| share.step().top_framerate);
    if let Some(hint) = maybe_content_hint {
      // #1733: the browser keeps the captured resolution and lowers the rate under load
      // ("maintain resolution") for a track marked as text, not the other way round.
      if let Err(error) = js_sys::Reflect::set(
        &track,
        &JsValue::from_str("contentHint"),
        &JsValue::from_str(hint),
      ) {
        debug!(error = %js_message(&error), "content hint not set on the shared screen");
      }
    }
    let infos = within_layer_limit(&infos, max_layers);
    let sizer = LayerSizer::new(infos.clone(), limit);
    let encoding = Encoding {
      kind: plan.kind.clone(),
      codec,
      framerate,
      latency,
      maybe_content_hint,
    };
    let is_camera = plan.shape == LayerShape::Camera;
    let layer_count = u8::try_from(infos.len()).unwrap_or(u8::MAX);
    let mut encoders = LayerEncoders::new(infos, encoding.opener(inputs))?;
    let maybe_load = is_camera.then(|| {
      let config = CameraGovernorConfig::default();
      encoders.govern(config, plan.framerate, now);
      CameraLoad::new(plan.framerate, layer_count, now, &config)
    });
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
        own: OwnPublication::new(publication),
      },
      stream,
      video,
      encoders,
      encoding,
      codecs: plan.codecs,
      sizer,
      framerate,
      frames_since_keyframe: 0,
      timestamp_us: 0.0,
      failed_pictures_in_a_row: 0,
      frames_captured: 0,
      #[cfg(feature = "effects")]
      maybe_effects: plan.maybe_effects.map(effects::WebEffects::new).transpose()?,
      cadence: Cadence::per_second(framerate, now),
      maybe_load,
      maybe_share,
      max_layers,
      limit,
      maybe_replan: None,
      top_encode_starts: VecDeque::new(),
      _ended: ended,
    })
  }

  pub fn layers(&self) -> Vec<VideoLayerInfo> {
    self.encoders.infos()
  }

  /// The codec its encoders encode.
  pub fn codec(&self) -> &VideoCodec {
    &self.encoding.codec
  }

  /// The codecs the track may go out in, best first.
  pub fn codecs(&self) -> &[VideoCodec] {
    &self.codecs
  }

  /// Encodes in `codec` from the next picture on, under the new publication `publication` (#565:
  /// a viewer joined that can't decode the codec, or the last one that couldn't left). Every
  /// layer starts with a keyframe; the old stream is the caller's to withdraw.
  pub fn switch_codec(
    &mut self,
    codec: VideoCodec,
    publication: VideoPublicationId,
    inputs: &mpsc::UnboundedSender<Input>,
  ) -> Result<(), WebCallError> {
    let previous = std::mem::replace(&mut self.encoding.codec, codec);
    if let Err(error) = self.encoders.reopen(self.encoding.opener(inputs)) {
      // Back to the codec the track is published in; a layer whose encoder doesn't open again is
      // restarted with the next picture, as after a failure.
      self.encoding.codec = previous;
      if let Err(restore) = self.encoders.reopen(self.encoding.opener(inputs)) {
        debug!(%restore, "a video encoder didn't open again in the track's codec");
      }
      return Err(error);
    }
    self.publication.own = OwnPublication::new(publication);
    self.frames_since_keyframe = 0;
    Ok(())
  }

  /// The size the camera or screen delivers now; `None` before it plays.
  #[cfg(feature = "call-view")]
  pub fn source_size(&self) -> Option<VideoResolution> {
    let side = |pixels: u32| u16::try_from(pixels).unwrap_or(u16::MAX);
    let (width, height) = (self.video.video_width(), self.video.video_height());
    (width > 0 && height > 0).then(|| VideoResolution {
      width: side(width),
      height: side(height),
    })
  }

  /// Pictures taken from the source for encoding so far.
  #[cfg(feature = "call-view")]
  pub fn frames_captured(&self) -> u64 {
    self.frames_captured
  }

  /// Frames given to the layers' encoders so far, and those skipped because a queue was full.
  #[cfg(feature = "call-view")]
  pub fn encoder_frame_counts(&self) -> (u64, u64) {
    self.encoders.frame_counts()
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

  /// Encodes the layers at `bitrates` (layer, bit/s) from now on: the engine's `LayerBitrates`
  /// (#939). Only an encoder that refuses it is replaced, and the error is a layer that gave up.
  pub fn set_layer_bitrates(&mut self, bitrates: &[(u8, u32)]) -> Result<(), EncoderGaveUp> {
    self.encoders.set_bitrates(bitrates)
  }

  #[cfg(feature = "effects")]
  pub fn set_effect(&mut self, effect: crate::effects::BackgroundEffect) {
    if let Some(effects) = &mut self.maybe_effects {
      effects.set_effect(effect);
    }
  }

  /// Encodes the current picture on every needed layer; skipped while nothing plays yet or the
  /// track has no stream to go out on. A picture that can't be taken is skipped; only encoders
  /// that keep failing end the track.
  pub async fn capture(&mut self) -> Result<(), EncoderGaveUp> {
    if self.publication.own.maybe_stream_id.is_none() || self.video.video_width() == 0 {
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
    self.frames_captured += 1;
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
    if self.maybe_share.is_some() {
      // Timestamps are whole microseconds; the cast keeps them.
      self
        .top_encode_starts
        .push_back((self.timestamp_us as i64, Instant::now()));
      if self.top_encode_starts.len() > SHARE_ENCODE_STARTS {
        self.top_encode_starts.pop_front();
      }
    }
    #[cfg(feature = "effects")]
    if let Some(effects) = &mut self.maybe_effects {
      let (given, skipped) = self.encoders.frame_counts();
      effects.observe_encoders(given, skipped);
    }
    let tally = result?;
    self.govern_load(tally, Instant::now())
  }

  /// A layer's encoder produced a picture of `bytes` for the frame of `timestamp_us`: it works, a
  /// governed layer's output is held to its bitrate, and a share's top layer tells how long it
  /// took against the frame interval (#1733).
  pub fn on_encoded(
    &mut self,
    layer: u8,
    bytes: usize,
    timestamp_us: f64,
    now: Instant,
  ) -> Result<(), EncoderGaveUp> {
    let is_top = self
      .encoders
      .layers
      .last()
      .is_some_and(|top| top.info.layer == layer);
    if is_top && let Some(share) = &mut self.maybe_share {
      // The cast keeps the whole microseconds the frame was stamped with.
      let key = timestamp_us as i64;
      if let Some(index) = self.top_encode_starts.iter().position(|(stamp, _)| *stamp == key) {
        let (_, given_at) = self.top_encode_starts[index];
        self.top_encode_starts.drain(..=index);
        share.on_frame(FrameTally {
          encode_time: now.saturating_duration_since(given_at),
          frame_interval: std::time::Duration::from_secs(1) / u32::from(share.step().top_framerate.max(1)),
          ..FrameTally::default()
        });
      }
    }
    self.encoders.on_encoded(layer, bytes, now)
  }

  /// The share's layers changed beyond what its encoders follow in place (the CPU ladder stopped
  /// its middle layer or changed its codec, the viewers' codecs changed): what it is to be
  /// published as anew, once ([`Self::rebuild`]).
  pub fn take_replan(&mut self) -> Option<Replan> {
    self.maybe_replan.take()
  }

  /// Encodes `replan`'s layers from the next picture on, under the new publication
  /// `publication`; every layer starts with a keyframe and the old stream is the caller's to
  /// withdraw. A layer whose encoder doesn't open keeps the old layers.
  pub fn rebuild(
    &mut self,
    replan: Replan,
    publication: VideoPublicationId,
    inputs: &mpsc::UnboundedSender<Input>,
  ) -> Result<(), WebCallError> {
    let previous = std::mem::replace(&mut self.encoding.codec, replan.codec.clone());
    let framerate = replan
      .layers
      .iter()
      .map(|info| info.max_framerate)
      .max()
      .unwrap_or(1);
    if let Err(error) = self
      .encoders
      .replace_layers(replan.layers.clone(), self.encoding.opener(inputs))
    {
      self.encoding.codec = previous;
      return Err(error);
    }
    self.encoding.framerate = framerate;
    self.sizer = LayerSizer::new(replan.layers, self.limit);
    self.publication.own = OwnPublication::new(publication);
    self.frames_since_keyframe = 0;
    self.top_encode_starts.clear();
    self.set_capture_rate(framerate, Instant::now());
    if let Some(share) = &mut self.maybe_share {
      share.measure_afresh(Instant::now());
    }
    Ok(())
  }

  /// Plans a share anew for the codecs the viewers decode now (#1733): where its layers change,
  /// [`Self::take_replan`] has them. A camera follows [`Self::switch_codec`].
  pub fn follow_share_codecs(&mut self, codecs: ShareCodecs) -> Result<(), EncoderGaveUp> {
    let Some(share) = &mut self.maybe_share else {
      return Ok(());
    };
    let plan = SharePlan {
      codecs,
      ..share.ladder().plan().clone()
    };
    share.replan(plan, Instant::now());
    self.follow_share_step()
  }

  /// How the uplink treats the share's top layer now (#1733): where it can't carry it even at its
  /// least frame rate, the top steps to a smaller size, and back once it carried it whole.
  pub fn on_uplink(&mut self, top: UplinkTop, now: Instant) -> Result<(), EncoderGaveUp> {
    let Some(share) = &mut self.maybe_share else {
      return Ok(());
    };
    if !share.on_uplink(top, now) {
      return Ok(());
    }
    info!(
      top_lines = ?share.step().maybe_top.map(|top| top.width.min(top.height)),
      "the uplink changed the screen share's top layer size"
    );
    self.follow_share_step()
  }

  /// What holds a share below its plan, for the stats; `None` for a camera.
  #[cfg(feature = "call-view")]
  pub fn share_limits(&self) -> Option<ShareLimits> {
    self.maybe_share.as_ref().map(ShareGovernor::limits)
  }

  /// The share's encoders follow its step now: in place where only rates changed, else the
  /// track goes out anew ([`Self::take_replan`]).
  fn follow_share_step(&mut self) -> Result<(), EncoderGaveUp> {
    let Some(share) = &self.maybe_share else {
      return Ok(());
    };
    let layers = within_layer_limit(&share.layers(), self.max_layers);
    let codec = share.track_codec();
    let framerate = share.step().top_framerate;
    if codec == self.encoding.codec && self.encoders.update_infos(&layers)? {
      self.maybe_replan = None;
      self.set_capture_rate(framerate, Instant::now());
    } else if self.encoders.infos() != layers || codec != self.encoding.codec {
      self.maybe_replan = Some(Replan { codec, layers });
    }
    Ok(())
  }

  /// Captures at `framerate` from `now` on.
  fn set_capture_rate(&mut self, framerate: u8, now: Instant) {
    if framerate == self.framerate {
      return;
    }
    self.framerate = framerate.max(1);
    self.cadence = Cadence::per_second(self.framerate, now);
    self.frames_since_keyframe = self
      .frames_since_keyframe
      .min(KEYFRAME_INTERVAL_SECONDS * u32::from(self.framerate));
  }

  /// The highest layer a governed camera's encoders keep up with, when they leave some out
  /// ([`crate::call_engine::CallEngine::limit_own_layers`]).
  pub fn maybe_top_layer(&self) -> Option<u8> {
    let load = self.maybe_load.as_ref()?;
    let off = load.step().top_layers_off;
    let count = u8::try_from(self.encoders.layers.len()).unwrap_or(u8::MAX);
    (off > 0).then(|| count.saturating_sub(1).saturating_sub(off))
  }

  /// What holds a governed camera below its plan; `None` for a screen share.
  #[cfg(feature = "call-view")]
  pub fn camera_limits(&self) -> Option<CameraLimits> {
    let load = self.maybe_load.as_ref()?;
    Some(CameraLimits {
      planned: load.planned(),
      load: load.step(),
      maybe_reason: load.maybe_reason(),
      layers: self.encoders.layer_limits(),
    })
  }

  /// Judges whether the encoders keep up; a new step changes the capture's frame rate (the top
  /// layers left out go through [`Self::maybe_top_layer`]).
  fn govern_load(&mut self, tally: FrameTally, now: Instant) -> Result<(), EncoderGaveUp> {
    if let Some(share) = &mut self.maybe_share {
      share.on_frame(tally);
      let Some(level) = share.evaluate(now) else {
        return Ok(());
      };
      info!(
        level,
        top_framerate = share.step().top_framerate,
        top_codec = %share.step().top_codec,
        has_middle = share.step().has_middle,
        top_lines = ?share.step().maybe_top.map(|top| top.width.min(top.height)),
        reason = ?share.limits().maybe_reason,
        "screen share encoders' load changed the share's step"
      );
      return self.follow_share_step();
    }
    let Some(load) = &mut self.maybe_load else {
      return Ok(());
    };
    load.on_frame(tally);
    let Some(config) = self.encoders.maybe_governor else {
      return Ok(());
    };
    let Some(step) = load.evaluate(now, &config) else {
      return Ok(());
    };
    info!(
      framerate = step.framerate,
      top_layers_off = step.top_layers_off,
      reason = ?load.maybe_reason(),
      "camera encoders' load changed the camera's frame rate or layers"
    );
    if step.framerate != self.framerate {
      self.framerate = step.framerate;
      self.cadence = Cadence::per_second(step.framerate, now);
      self.frames_since_keyframe = self
        .frames_since_keyframe
        .min(KEYFRAME_INTERVAL_SECONDS * u32::from(step.framerate));
      self.encoders.set_framerate(step.framerate)?;
    }
    Ok(())
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
    if let Some(share) = &mut self.maybe_share {
      // A share plans its layers for the new size (#1733): a window shrunk below 1440p loses its
      // middle layer rather than getting a base below 480p.
      let plan = SharePlan {
        source: fit_within(source, self.limit),
        ..share.ladder().plan().clone()
      };
      share.replan(plan, Instant::now());
      let layers = within_layer_limit(&share.layers(), self.max_layers);
      if self
        .encoders
        .infos()
        .iter()
        .map(|info| (info.layer, &info.maybe_codec))
        .eq(layers.iter().map(|info| (info.layer, &info.maybe_codec)))
      {
        return self.encoders.resize(&layers);
      }
      self.maybe_replan = Some(Replan {
        codec: share.track_codec(),
        layers,
      });
      return Ok(());
    }
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
          bitrate_bps: info.max_bitrate_bps,
          encoder: open(info.clone(), generation)?,
          info,
          generation,
          is_needed: true,
          wants_keyframe: true,
          restarts_in_a_row: 0,
          maybe_guard: None,
          is_measure_stale: true,
        })
      })
      .collect::<Result<Vec<_>, WebCallError>>()?;
    // The track is captured at its fastest layer's rate; slower layers take every n-th frame.
    let framerate = layers
      .iter()
      .map(|layer| layer.info.max_framerate)
      .max()
      .unwrap_or(1);
    Ok(Self {
      next_generation: layers.len() as u64,
      layers,
      open: Box::new(open),
      given: 0,
      skipped_queue_full: 0,
      maybe_governor: None,
      framerate,
      frame_index: 0,
    })
  }

  /// Holds each layer's output to its bitrate from now on (a camera, #1762), the track captured
  /// at `framerate`.
  pub fn govern(&mut self, config: CameraGovernorConfig, framerate: u8, now: Instant) {
    self.maybe_governor = Some(config);
    self.framerate = framerate.max(1);
    for layer in &mut self.layers {
      layer.maybe_guard = Some(LayerRateGuard::new(now));
    }
  }

  /// The layers as published, each with its whole bitrate.
  pub fn infos(&self) -> Vec<VideoLayerInfo> {
    self.layers.iter().map(|layer| layer.info.clone()).collect()
  }

  /// Encodes the layers at `bitrates` (layer, bit/s) from now on (#939). A layer whose bitrate
  /// changes keeps its encoder, configured again, and no keyframe: the picture goes on. One that
  /// refuses the configuration is replaced at the new bitrate (starting with a keyframe); a closed
  /// one is replaced with the next frame at it. A bitrate of 0 or of an unknown layer is ignored.
  pub fn set_bitrates(&mut self, bitrates: &[(u8, u32)]) -> Result<(), EncoderGaveUp> {
    for index in 0..self.layers.len() {
      let layer = &mut self.layers[index];
      let Some(&(_, bitrate_bps)) = bitrates.iter().find(|(id, _)| *id == layer.info.layer) else {
        continue;
      };
      if bitrate_bps == layer.bitrate_bps || bitrate_bps == 0 {
        continue;
      }
      layer.bitrate_bps = bitrate_bps;
      self.reconfigure(index)?;
    }
    Ok(())
  }

  /// The camera is captured at `framerate` from now on (its encoders didn't keep up, or keep up
  /// again): every layer's encoder is configured at its share of it.
  pub fn set_framerate(&mut self, framerate: u8) -> Result<(), EncoderGaveUp> {
    let framerate = framerate.max(1);
    if framerate == self.framerate {
      return Ok(());
    }
    self.framerate = framerate;
    for index in 0..self.layers.len() {
      self.layers[index].is_measure_stale = true;
      self.reconfigure(index)?;
    }
    Ok(())
  }

  /// The layers whose encoder overshot and runs limited: the layer, its bitrate and frame rate.
  pub fn layer_limits(&self) -> Vec<(u8, u32, u8)> {
    let Some(config) = &self.maybe_governor else {
      return Vec::new();
    };
    self
      .layers
      .iter()
      .filter_map(|layer| {
        let limit = layer.limit(self.framerate, config);
        limit.is_limited().then(|| {
          (
            layer.info.layer,
            limit.bitrate_bps(layer.bitrate_bps, config),
            limit.framerate(self.framerate),
          )
        })
      })
      .collect()
  }

  /// Configures a layer's running encoder at its bitrate and frame rate now; one that refuses is
  /// replaced at them (starting with a keyframe), a closed one is replaced with the next frame.
  fn reconfigure(&mut self, index: usize) -> Result<(), EncoderGaveUp> {
    let built = self.built(index);
    let layer = &mut self.layers[index];
    if !layer.encoder.is_configured() {
      return Ok(());
    }
    if let Err(error) = layer
      .encoder
      .configure(built.max_bitrate_bps, built.max_framerate)
    {
      warn!(%error, layer = layer.info.layer, bitrate_bps = built.max_bitrate_bps, "a video encoder takes no new configuration; replacing it");
      self.restart(index)?;
    }
    Ok(())
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
      layer.is_measure_stale |= is_needed != layer.is_needed;
      layer.is_needed = is_needed;
    }
  }

  /// Encodes `frame` on every needed layer whose encoder has room. A failed encoder is replaced
  /// (the new one starts with the next frame, a keyframe); the error is only a layer that gave up.
  ///
  /// A governed layer whose encoder overshot gets only every n-th frame; a periodic keyframe it
  /// doesn't get waits for its next frame. The tally says how the encoders took the frame.
  pub fn encode(
    &mut self,
    frame: &E::Frame,
    is_periodic_keyframe: bool,
  ) -> Result<FrameTally, EncoderGaveUp> {
    let mut tally = FrameTally::default();
    let frame_index = self.frame_index;
    self.frame_index = self.frame_index.wrapping_add(1);
    for index in 0..self.layers.len() {
      let divisor = self.limit(index).frame_divisor.max(1) * self.rate_divisor(index);
      let layer = &mut self.layers[index];
      if !layer.is_needed {
        continue;
      }
      if !frame_index.is_multiple_of(u64::from(divisor)) {
        layer.wants_keyframe |= is_periodic_keyframe;
        continue;
      }
      // Closed after an error whose report hasn't arrived yet (that report is stale then).
      if !layer.encoder.is_configured() {
        debug!(layer = layer.info.layer, "video encoder closed after an error");
        self.restart(index)?;
        continue;
      }
      let queued = layer.encoder.queue_size();
      if queued > MAX_ENCODE_QUEUE {
        self.skipped_queue_full += 1;
        tally.skipped += 1;
        continue;
      }
      self.given += 1;
      tally.given += 1;
      tally.queued += queued;
      let is_keyframe = std::mem::take(&mut layer.wants_keyframe) || is_periodic_keyframe;
      if let Err(error) = layer.encoder.encode(frame, is_keyframe) {
        warn!(%error, layer = layer.info.layer, "encoding a video frame failed");
        self.restart(index)?;
      }
    }
    Ok(tally)
  }

  /// Frames given to the layers' encoders so far, and those skipped because a queue was full.
  pub fn frame_counts(&self) -> (u64, u64) {
    (self.given, self.skipped_queue_full)
  }

  /// Every layer gets a new encoder from `open` (another codec, #565), at its size, which starts
  /// with a keyframe; `open` replaces the encoders from then on. Fails when one doesn't open.
  pub fn reopen(
    &mut self,
    open: impl FnMut(VideoLayerInfo, u64) -> Result<E, WebCallError> + 'static,
  ) -> Result<(), WebCallError> {
    self.open = Box::new(open);
    for layer in &mut self.layers {
      let generation = self.next_generation;
      self.next_generation += 1;
      layer.encoder = (self.open)(
        layer.built(self.framerate, self.maybe_governor.as_ref()),
        generation,
      )?;
      layer.generation = generation;
      layer.wants_keyframe = true;
      layer.restarts_in_a_row = 0;
      layer.is_measure_stale = true;
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
      // A bitrate lowered for the uplink stays lowered.
      if layer.bitrate_bps == layer.info.max_bitrate_bps {
        layer.bitrate_bps = info.max_bitrate_bps;
      }
      layer.info = info.clone();
      let generation = self.next_generation;
      self.next_generation += 1;
      match (self.open)(
        layer.built(self.framerate, self.maybe_governor.as_ref()),
        generation,
      ) {
        Ok(encoder) => {
          layer.encoder = encoder;
          layer.generation = generation;
          layer.wants_keyframe = true;
          layer.is_measure_stale = true;
        },
        Err(error) => {
          warn!(%error, layer = info.layer, "opening a video encoder at the new size failed");
          self.restart(index)?;
        },
      }
    }
    Ok(())
  }

  /// A layer's encoder produced a picture of `bytes`: it works, so a later failure may restart it
  /// again. A governed layer's output is judged against its bitrate; a new limit configures its
  /// encoder (one that refuses is replaced, and the error is a layer that gave up).
  pub fn on_encoded(&mut self, layer: u8, bytes: usize, now: Instant) -> Result<(), EncoderGaveUp> {
    let Some(index) = self.layers.iter().position(|current| current.info.layer == layer) else {
      return Ok(());
    };
    let framerate = self.framerate;
    let layer = &mut self.layers[index];
    layer.restarts_in_a_row = 0;
    let (Some(config), Some(guard)) = (&self.maybe_governor, &mut layer.maybe_guard) else {
      return Ok(());
    };
    if std::mem::take(&mut layer.is_measure_stale) {
      guard.measure_afresh(now);
    }
    guard.on_output(bytes, now, config);
    let Some(limit) = guard.evaluate(layer.bitrate_bps, framerate, now, config) else {
      return Ok(());
    };
    info!(
      layer = layer.info.layer,
      bitrate_bps = limit.bitrate_bps(layer.bitrate_bps, config),
      framerate = limit.framerate(framerate),
      "a camera layer's output against its bitrate changed its limit"
    );
    self.reconfigure(index)
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
      match (self.open)(
        layer.built(self.framerate, self.maybe_governor.as_ref()),
        generation,
      ) {
        Ok(encoder) => {
          debug!(layer = layer.info.layer, "started a new video encoder");
          layer.encoder = encoder;
          layer.generation = generation;
          layer.wants_keyframe = true;
          layer.is_measure_stale = true;
          return Ok(());
        },
        Err(error) => warn!(%error, layer = layer.info.layer, "starting a new video encoder failed"),
      }
    }
  }

  /// The layers' frame rates, sizes and bitrates follow `infos` in place where each layer keeps
  /// its id, size and codec (a share's frame rate stepped, #1733): the running encoders are
  /// configured again, without a keyframe. `false`, and nothing changed, where the layers differ
  /// otherwise: the track is built anew ([`Self::replace_layers`]).
  pub fn update_infos(&mut self, infos: &[VideoLayerInfo]) -> Result<bool, EncoderGaveUp> {
    let is_same_shape = infos.len() == self.layers.len()
      && self.layers.iter().zip(infos).all(|(layer, info)| {
        layer.info.layer == info.layer
          && (layer.info.width, layer.info.height) == (info.width, info.height)
          && layer.info.maybe_codec == info.maybe_codec
      });
    if !is_same_shape {
      return Ok(false);
    }
    self.framerate = infos.iter().map(|info| info.max_framerate).max().unwrap_or(1);
    for (index, info) in infos.iter().enumerate() {
      let layer = &mut self.layers[index];
      if layer.info == *info {
        continue;
      }
      // A bitrate lowered for the uplink stays lowered.
      if layer.bitrate_bps == layer.info.max_bitrate_bps {
        layer.bitrate_bps = info.max_bitrate_bps;
      }
      layer.info = info.clone();
      self.reconfigure(index)?;
    }
    Ok(true)
  }

  /// New layers `infos` with new encoders from `open`, each starting with a keyframe (a share's
  /// middle layer stopped, or a layer's codec changed, #1733). Fails when one doesn't open.
  pub fn replace_layers(
    &mut self,
    infos: Vec<VideoLayerInfo>,
    open: impl FnMut(VideoLayerInfo, u64) -> Result<E, WebCallError> + 'static,
  ) -> Result<(), WebCallError> {
    let mut open: EncoderOpener<E> = Box::new(open);
    // Generations go on from the replaced encoders', so their late failure reports are stale.
    let mut layers = Vec::with_capacity(infos.len());
    let mut next_generation = self.next_generation;
    for info in infos {
      let generation = next_generation;
      next_generation += 1;
      layers.push(Layer {
        bitrate_bps: info.max_bitrate_bps,
        encoder: open(info.clone(), generation)?,
        info,
        generation,
        is_needed: true,
        wants_keyframe: true,
        restarts_in_a_row: 0,
        maybe_guard: None,
        is_measure_stale: true,
      });
    }
    self.framerate = layers
      .iter()
      .map(|layer| layer.info.max_framerate)
      .max()
      .unwrap_or(1);
    self.layers = layers;
    self.open = open;
    self.next_generation = next_generation;
    self.maybe_governor = None;
    Ok(())
  }

  /// Every how many-th captured frame layer `index` takes for its own frame rate: a share's base
  /// layer at 15 fps of a capture at 30 takes every second (#1733). A governed layer's own rate,
  /// not the one its limit leaves it: the limit's frame divisor applies on top, and counted twice
  /// a camera layer held to every second frame got every fourth.
  fn rate_divisor(&self, index: usize) -> u8 {
    let layer_rate = match self.maybe_governor {
      Some(_) => self.layers[index].info.max_framerate,
      None => self.built(index).max_framerate,
    }
    .max(1);
    let capture = self.framerate.max(1);
    // Rounded: 30 fps for a layer at 18 takes every frame (the encoder drops what it must).
    ((u16::from(capture) + u16::from(layer_rate) / 2) / u16::from(layer_rate))
      .clamp(1, u16::from(u8::MAX))
      .try_into()
      .unwrap_or(1)
  }

  /// What layer `index` is held to now.
  fn limit(&self, index: usize) -> LayerLimit {
    match &self.maybe_governor {
      Some(config) => self.layers[index].limit(self.framerate, config),
      None => LayerLimit::NONE,
    }
  }

  fn built(&self, index: usize) -> VideoLayerInfo {
    self.layers[index].built(self.framerate, self.maybe_governor.as_ref())
  }

  fn layer_mut(&mut self, layer: u8) -> Option<&mut Layer<E>> {
    self.layers.iter_mut().find(|current| current.info.layer == layer)
  }
}

impl<E> Layer<E> {
  /// What its encoder is opened with: the layer at the bitrate it is encoded at, and for a
  /// governed track at its limit and the track's frame rate now.
  fn built(&self, framerate: u8, maybe_governor: Option<&CameraGovernorConfig>) -> VideoLayerInfo {
    let Some(config) = maybe_governor else {
      // An ungoverned track (a screen share) on a limited uplink keeps its resolution and lowers
      // its frame rate with its bitrate (#1733).
      return VideoLayerInfo {
        max_bitrate_bps: self.bitrate_bps,
        max_framerate: share_framerate(&self.info, self.bitrate_bps),
        ..self.info.clone()
      };
    };
    let limit = self.limit(framerate, config);
    VideoLayerInfo {
      max_bitrate_bps: limit.bitrate_bps(self.bitrate_bps, config),
      max_framerate: limit.framerate(framerate).min(self.info.max_framerate),
      ..self.info.clone()
    }
  }

  fn limit(&self, framerate: u8, config: &CameraGovernorConfig) -> LayerLimit {
    self
      .maybe_guard
      .as_ref()
      .map_or(LayerLimit::NONE, |guard| guard.limit(framerate, config))
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

impl Encoding {
  /// Opens a layer's encoder with these settings.
  fn opener(
    &self,
    inputs: &mpsc::UnboundedSender<Input>,
  ) -> impl FnMut(VideoLayerInfo, u64) -> Result<LayerEncoder, WebCallError> + 'static {
    let (encoding, inputs) = (self.clone(), inputs.clone());
    move |info, generation| LayerEncoder::new(info, &encoding, generation, &inputs)
  }

  /// `VideoEncoderConfig` of a layer of `info`'s size.
  ///
  /// `bitrateMode` stays unset, the browsers' `"variable"` (#1005, #1205): `"constant"` caps no
  /// peak. Chrome's hardware H.264 encoder on a Mac and Firefox on a Mac sent the very same bytes
  /// in every mode, Chrome's software encoder on Linux within 0.6 % (docs/performance.md, "Short
  /// bitrate peaks of the encoders").
  fn config(&self, info: &VideoLayerInfo) -> Result<js_sys::Object, JsValue> {
    let mut entries = vec![
      ("width", JsValue::from(info.width)),
      ("height", JsValue::from(info.height)),
      ("bitrate", JsValue::from(info.max_bitrate_bps)),
      ("framerate", JsValue::from(self.layer_framerate(info))),
      ("latencyMode", JsValue::from_str(self.latency.as_str())),
    ];
    // `contentHint` (#565, #1733): "text" keeps a detail share's resolution and turns on AV1's
    // screen content tools; it applies to H.264 as well where the browser knows it.
    if let Some(hint) = self.maybe_content_hint {
      entries.push(("contentHint", JsValue::from_str(hint)));
    }
    match info.codec(&self.codec) {
      VideoCodec::Av1 => {
        entries.push((
          "codec",
          JsValue::from_str(av1_encoder_codec(info.width, info.height, self.framerate)),
        ));
      },
      VideoCodec::Vp9 => {
        entries.push((
          "codec",
          JsValue::from_str(vp9_encoder_codec(info.width, info.height, info.max_framerate)),
        ));
      },
      // H.264 as before #565, also for codecs nobody picks (`CallEngine::video_codec_for`).
      _ => {
        entries.push((
          "codec",
          JsValue::from_str(h264_encoder_codec(info.width, info.height)),
        ));
        entries.push((
          "avc",
          dictionary(&[("format", JsValue::from_str("annexb"))])?.into(),
        ));
      },
    }
    dictionary(&entries)
  }
}

impl Encoding {
  /// Every layer is encoded at its own frame rate: a camera's lower while governed (#1762), a
  /// share's base layer at what text needs and its top at what the CPU and the uplink allow
  /// (#1733).
  fn layer_framerate(&self, info: &VideoLayerInfo) -> u8 {
    info.max_framerate.max(1)
  }
}

impl LayerEncoder {
  fn new(
    info: VideoLayerInfo,
    encoding: &Encoding,
    generation: u64,
    inputs: &mpsc::UnboundedSender<Input>,
  ) -> Result<Self, WebCallError> {
    let sender = inputs.clone();
    let layer = info.layer;
    let kind = encoding.kind.clone();
    let output_kind = kind.clone();
    // #1733: a share's layers may differ in codec; each chunk carries its layer's.
    let codec = info.codec(&encoding.codec).clone();
    let output = Closure::<dyn FnMut(EncodedVideoChunk)>::new(move |chunk: EncodedVideoChunk| {
      let bytes = Uint8Array::new_with_length(chunk.byte_length());
      let input = match chunk.copy_to(&bytes) {
        Ok(()) => Input::VideoEncoded {
          kind: output_kind.clone(),
          codec: codec.clone(),
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
    let config = encoding.config(&info).map_err(browser("VideoEncoder"))?;
    encoder.configure(&config).map_err(browser("VideoEncoder"))?;
    Ok(Self {
      encoder,
      config,
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

  /// `configure()` again with only the bitrate and frame rate changed: the spec queues it behind the frames
  /// already given, and Chrome then changes the running encoder's rate rather than build a new
  /// one; either way the chunks say which are keyframes. An error makes the caller replace the
  /// encoder.
  fn configure(&mut self, bitrate_bps: u32, framerate: u8) -> Result<(), WebCallError> {
    let set = |key: &str, value: JsValue| {
      js_sys::Reflect::set(&self.config, &JsValue::from_str(key), &value).map_err(browser("VideoEncoder"))
    };
    set("bitrate", JsValue::from(bitrate_bps))?;
    set("framerate", JsValue::from(framerate.max(1)))?;
    self
      .encoder
      .configure(&self.config)
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

  /// Frames waiting in the decoder; none without one.
  pub fn queue_size(&self) -> u32 {
    self
      .maybe_decoder
      .as_ref()
      .map_or(0, |open| open.decoder.queue_size())
  }

  /// Closes the decoder of a tile nobody sees (#1762): no work for a picture nobody watches. The
  /// next keyframe opens a new one.
  pub fn close(&mut self) {
    self.maybe_decoder = None;
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
      VideoCodec::Av1 => AV1_CODEC,
      VideoCodec::Unknown(_) => return Err(WebCallError::Unsupported("this video codec")),
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
    // H.264 in Annex B carries its parameter sets in band, AV1 its sequence header in every
    // keyframe: no `description` needed.
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

  fn queue_size(&self) -> u32 {
    self.decoder.decode_queue_size()
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

/// The plan for a camera sent within `limit` (`video::camera_limit`): its layers at 30 fps, from
/// 1080p where the camera delivers it (#967).
pub(super) fn camera_plan(limit: VideoResolution) -> TrackPlan {
  TrackPlan {
    kind: VideoTrackKind::Camera,
    framerate: CAMERA_FRAMERATE,
    shape: LayerShape::Camera,
    codecs: vec![VideoCodec::H264],
    maybe_max_resolution: Some(limit),
    #[cfg(feature = "effects")]
    maybe_effects: None,
  }
}

/// How a WebCodecs encoder weighs latency against quality (`VideoEncoderConfig.latencyMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LatencyMode {
  Realtime,
  Quality,
}

impl LatencyMode {
  fn as_str(self) -> &'static str {
    match self {
      Self::Realtime => "realtime",
      Self::Quality => "quality",
    }
  }
}

/// The latency mode of a camera's encoders: `quality` on Apple devices, `realtime` elsewhere.
/// Chrome's hardware encoder on a Mac (VideoToolbox) in `realtime` mode stops far below the
/// bitrate on a calm camera picture, whatever the bitrate and `bitrateMode` (#967, Chrome 156, M4
/// Max, a calm 30 fps picture: 360p 36 of 375 kbit/s, 720p 503 of 1500, 1080p 1940 of 3000, and
/// 533 kbit/s when 720p was given 3 Mbit/s); in `quality` mode it sends 424, 1700 and 3053 kbit/s,
/// and its pictures came out as soon (5 to 9 ms after the frame, against 6 to 11 ms). With H.264
/// Constrained Baseline there are no B-frames to wait for. Other platforms keep `realtime`: their
/// software encoder sends its bitrate either way, and a hardware encoder elsewhere may buffer
/// frames outside it (untested).
fn camera_latency_mode() -> LatencyMode {
  let is_apple = window()
    .ok()
    .and_then(|window| window.navigator().user_agent().ok())
    .is_some_and(|agent| agent.contains("Macintosh"));
  if is_apple {
    LatencyMode::Quality
  } else {
    LatencyMode::Realtime
  }
}

/// The codec string an H.264 encoder of `width`×`height` is configured with: level 4.0 up to
/// 1080p, level 5.1 above. Chrome refuses to encode a picture larger than the level allows
/// ("exceeds the maximum coded area"), so a 1440p screen share's top layer failed, and the
/// others saw only its half-size layer (issue #941).
fn h264_encoder_codec(width: u16, height: u16) -> &'static str {
  let macroblocks = u32::from(width).div_ceil(16) * u32::from(height).div_ceil(16);
  if macroblocks <= LEVEL_4_0_MAX_MACROBLOCKS {
    H264_CODEC
  } else {
    H264_CODEC_LEVEL_5_1
  }
}

/// The video codecs a browser encodes and decodes (#565).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodecSupport {
  pub encoders: Vec<VideoCodec>,
  pub decoders: Vec<VideoCodec>,
  /// How it encodes AV1 and VP9 (#1733): a screen share's top layer goes out in them at full size
  /// only from a hardware encoder.
  pub share_encoders: ShareEncoders,
}

/// What this browser encodes and decodes: H.264 always (every client sends and takes it), AV1
/// where WebCodecs says it encodes a standard screen share's top layer (1440p at 30 fps, in real
/// time), VP9 where it does so in hardware (#1733), and each where it decodes it. A browser that
/// doesn't answer gets H.264 only.
pub(super) async fn codec_support() -> CodecSupport {
  let mut support = CodecSupport {
    encoders: vec![VideoCodec::H264],
    decoders: vec![VideoCodec::H264],
    share_encoders: ShareEncoders {
      av1: EncoderKind::Unavailable,
      vp9: EncoderKind::Unavailable,
    },
  };
  let layer = VideoLayerInfo {
    layer: 0,
    width: 2560,
    height: 1440,
    max_framerate: 30,
    max_bitrate_bps: 6_000_000,
    maybe_codec: None,
  };
  support.share_encoders.vp9 = encoder_kind(VideoCodec::Vp9, &layer).await;
  if support.share_encoders.vp9 == EncoderKind::Hardware {
    support.encoders.insert(0, VideoCodec::Vp9);
  }
  support.share_encoders.av1 = encoder_kind(VideoCodec::Av1, &layer).await;
  if support.share_encoders.av1 != EncoderKind::Unavailable {
    support.encoders.insert(0, VideoCodec::Av1);
  }
  for (codec, name) in [(VideoCodec::Vp9, VP9_CODEC), (VideoCodec::Av1, AV1_CODEC)] {
    if let Ok(config) = dictionary(&[("codec", JsValue::from_str(name))])
      && is_supported(VideoDecoder::is_config_supported(&config)).await
    {
      support.decoders.insert(0, codec);
    }
  }
  support
}

/// How the browser encodes `codec` at `layer` for a screen share (#1733): not at all where
/// WebCodecs refuses the config; in hardware where it takes it with `hardwareAcceleration:
/// "prefer-hardware"` and `MediaCapabilities.encodingInfo` doesn't call it power inefficient;
/// else in software. A browser that can't tell gets the software plan, which a hardware encoder
/// only loses resolution or rate with.
async fn encoder_kind(codec: VideoCodec, layer: &VideoLayerInfo) -> EncoderKind {
  let probe = Encoding {
    kind: VideoTrackKind::Screen,
    codec: codec.clone(),
    framerate: layer.max_framerate,
    latency: LatencyMode::Realtime,
    maybe_content_hint: Some("text"),
  };
  let Ok(config) = probe.config(layer) else {
    return EncoderKind::Unavailable;
  };
  if !is_supported(VideoEncoder::is_config_supported(&config)).await {
    return EncoderKind::Unavailable;
  }
  let hardware = js_sys::Object::assign(&js_sys::Object::new(), &config);
  let is_hardware_config = js_sys::Reflect::set(
    &hardware,
    &JsValue::from_str("hardwareAcceleration"),
    &JsValue::from_str("prefer-hardware"),
  )
  .is_ok()
    && is_supported(VideoEncoder::is_config_supported(&hardware)).await;
  if !is_hardware_config {
    return EncoderKind::Software;
  }
  match is_power_efficient(&codec, layer).await {
    Some(false) => EncoderKind::Software,
    Some(true) | None => EncoderKind::Hardware,
  }
}

/// `navigator.mediaCapabilities.encodingInfo(...)`'s `powerEfficient` for `codec` at `layer`'s
/// size; `None` where the browser has no such API or doesn't answer.
async fn is_power_efficient(codec: &VideoCodec, layer: &VideoLayerInfo) -> Option<bool> {
  let content_type = match codec {
    VideoCodec::Av1 => "video/AV1",
    VideoCodec::Vp9 => "video/VP9",
    VideoCodec::H264 | VideoCodec::Unknown(_) => "video/H264",
  };
  let navigator = window().ok()?.navigator();
  let capabilities = js_sys::Reflect::get(&navigator, &JsValue::from_str("mediaCapabilities")).ok()?;
  let encoding_info: js_sys::Function =
    js_sys::Reflect::get(&capabilities, &JsValue::from_str("encodingInfo"))
      .ok()?
      .dyn_into()
      .ok()?;
  let video = dictionary(&[
    ("contentType", JsValue::from_str(content_type)),
    ("width", JsValue::from(layer.width)),
    ("height", JsValue::from(layer.height)),
    ("bitrate", JsValue::from(layer.max_bitrate_bps)),
    ("framerate", JsValue::from(layer.max_framerate)),
  ])
  .ok()?;
  let query = dictionary(&[("type", JsValue::from_str("webrtc")), ("video", video.into())]).ok()?;
  let promise: Promise = encoding_info.call1(&capabilities, &query).ok()?.dyn_into().ok()?;
  let answer = JsFuture::from(promise).await.ok()?;
  let is_supported = js_sys::Reflect::get(&answer, &JsValue::from_str("supported"))
    .ok()?
    .as_bool()?;
  if !is_supported {
    return None;
  }
  js_sys::Reflect::get(&answer, &JsValue::from_str("powerEfficient"))
    .ok()?
    .as_bool()
}

/// Whether an `isConfigSupported` answer says yes; a throw (no WebCodecs) or a rejection says no.
async fn is_supported(answer: Result<Promise, JsValue>) -> bool {
  let Ok(promise) = answer else {
    return false;
  };
  JsFuture::from(promise)
    .await
    .ok()
    .and_then(|support| js_sys::Reflect::get(&support, &JsValue::from_str("supported")).ok())
    .and_then(|is_supported| is_supported.as_bool())
    .unwrap_or(false)
}

/// The codec string an AV1 encoder of `width`×`height` at `framerate` is configured with: level
/// 5.0 up to 4K at 30 fps, 5.1 above.
fn av1_encoder_codec(width: u16, height: u16, framerate: u8) -> &'static str {
  let picture = u32::from(width) * u32::from(height);
  let rate = u64::from(picture) * u64::from(framerate);
  if picture <= AV1_LEVEL_5_0_MAX_PICTURE && rate <= AV1_LEVEL_5_0_MAX_RATE {
    AV1_CODEC
  } else {
    AV1_CODEC_LEVEL_5_1
  }
}

/// The codec string a VP9 encoder of `width`×`height` at `framerate` is configured with (#1733).
fn vp9_encoder_codec(width: u16, height: u16, framerate: u8) -> &'static str {
  let picture = u32::from(width) * u32::from(height);
  let rate = u64::from(picture) * u64::from(framerate);
  if picture <= VP9_LEVEL_4_1_MAX_PICTURE {
    VP9_CODEC_LEVEL_4_1
  } else if rate <= VP9_LEVEL_5_0_MAX_RATE {
    VP9_CODEC_LEVEL_5_0
  } else {
    VP9_CODEC
  }
}

/// `VideoEncoderConfig.contentHint` of a screen share's encoders (#565): `"text"` for detail
/// (AV1's palette and intra block copy: libaom's screen content mode in Chrome), `"motion"`.
fn content_hint(profile: &ScreenProfile) -> &'static str {
  match profile.mode {
    ScreenContentMode::Motion => "motion",
    ScreenContentMode::Detail | ScreenContentMode::Unknown => "text",
  }
}

/// The plan for a screen share: the profile's largest picture and rate (1440p30 for detail,
/// 1080p60 for motion; 4K30 and 1440p60 for a 4K share) plus smaller layers for weak links; a
/// smaller source is sent as it is.
pub(super) fn screen_plan(profile: &ScreenProfile) -> TrackPlan {
  let (limit, framerate) = profile.limits();
  TrackPlan {
    kind: VideoTrackKind::Screen,
    framerate,
    shape: LayerShape::Screen(profile.clone()),
    codecs: profile.codecs(),
    maybe_max_resolution: Some(limit),
    #[cfg(feature = "effects")]
    maybe_effects: None,
  }
}

/// Asks for the camera (`getUserMedia`); `maybe_device` is a `MediaDeviceInfo.deviceId`. Every
/// constraint is `ideal`, so no device is turned away for its size or rate (`OverconstrainedError`);
/// if the browser still fails (the chosen device is gone, or refuses the size), it is asked once
/// more for any camera with no constraints.
pub(super) async fn capture_camera(
  maybe_device: Option<&str>,
  limit: VideoResolution,
) -> Result<MediaStream, WebCallError> {
  let video = MediaTrackConstraints::new();
  video.set_width(&ideal(u32::from(limit.width))?);
  video.set_height(&ideal(u32::from(limit.height))?);
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

/// Asks the user what to share (`getDisplayMedia`), with its sound if `has_sound`. Needs a recent
/// user gesture.
pub(super) async fn capture_screen(
  profile: &ScreenProfile,
  has_sound: bool,
) -> Result<MediaStream, WebCallError> {
  picked_screen(request_screen(profile, has_sound)?).await
}

/// Opens the screen picker now (synchronously, so a click's user activation still counts) and
/// returns `getDisplayMedia`'s promise. With `has_sound` the picker also offers the tab's or the
/// system's sound ([`super::screen_sound`]); a browser that throws at the sound request at once is
/// asked again, still in the click, for the picture alone.
pub(super) fn request_screen(profile: &ScreenProfile, has_sound: bool) -> Result<Promise, WebCallError> {
  let maybe_sound = super::screen_sound::sound_request(has_sound);
  match display_media(profile, maybe_sound) {
    Err(error) if maybe_sound.is_some() => {
      warn!(%error, "the browser refused to capture the screen's sound; sharing the picture alone");
      display_media(profile, None)
    },
    result => result,
  }
}

fn display_media(
  profile: &ScreenProfile,
  maybe_sound: Option<super::screen_sound::SoundRequest>,
) -> Result<Promise, WebCallError> {
  let (limit, framerate) = profile.limits();
  let video = dictionary(&[
    ("frameRate", maximum(u32::from(framerate))?),
    ("width", maximum(u32::from(limit.width))?),
    ("height", maximum(u32::from(limit.height))?),
  ])
  .map_err(browser("getDisplayMedia"))?;
  let constraints = DisplayMediaStreamConstraints::new();
  constraints.set_video(&video);
  if let Some(sound) = maybe_sound {
    let track: Vec<(&str, JsValue)> = sound
      .track
      .iter()
      .map(|(name, value)| (*name, JsValue::from_bool(*value)))
      .collect();
    let audio = dictionary(&track).map_err(browser("getDisplayMedia"))?;
    constraints.set_audio(&audio);
    for (name, value) in sound.options {
      js_sys::Reflect::set(&constraints, &JsValue::from_str(name), &JsValue::from_str(value))
        .map_err(browser("getDisplayMedia"))?;
    }
  }
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
pub(super) async fn play_hidden(stream: &MediaStream) -> Result<HtmlVideoElement, WebCallError> {
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

/// The camera's background effects (#439): the segmentation worker, a compositor (WebGL2, or WebGPU
/// in Safari; `page_calls::browser_effects_compositing`), the effect, and optionally a
/// canvas that shows each picture as it is sent, for the self view (`captureStream()` of it plays
/// in a `<video>`).
#[cfg(feature = "effects")]
pub struct CameraEffects {
  compositor: effects::BrowserCompositor,
  worker: std::rc::Rc<super::SegmentationWorker>,
  effect: crate::effects::BackgroundEffect,
  maybe_view: Option<HtmlCanvasElement>,
  is_required: bool,
}

#[cfg(feature = "effects")]
impl CameraEffects {
  /// Composites with WebGPU (Safari): the picture is read from a canvas, composited
  /// and turned into I420 on the GPU, and read back for the encoders.
  pub async fn webgpu(
    worker: std::rc::Rc<super::SegmentationWorker>,
    effect: crate::effects::BackgroundEffect,
  ) -> Result<Self, crate::effects::EffectsError> {
    let compositor = crate::effects::Compositor::new(crate::effects::EffectsConfig::default()).await?;
    Ok(Self::with(
      effects::BrowserCompositor::WebGpu(Box::new(compositor)),
      worker,
      effect,
    ))
  }

  /// Composites with WebGL2 (Chrome, Edge, Firefox, browsers without WebGPU): the picture stays on
  /// the GPU and the encoders take the canvas it is drawn into.
  pub fn webgl(
    worker: std::rc::Rc<super::SegmentationWorker>,
    effect: crate::effects::BackgroundEffect,
  ) -> Result<Self, crate::effects::EffectsError> {
    let compositor = super::GlCompositor::new(crate::effects::EffectsConfig::default())?;
    Ok(Self::with(
      effects::BrowserCompositor::WebGl(Box::new(compositor)),
      worker,
      effect,
    ))
  }

  fn with(
    compositor: effects::BrowserCompositor,
    worker: std::rc::Rc<super::SegmentationWorker>,
    effect: crate::effects::BackgroundEffect,
  ) -> Self {
    Self {
      compositor,
      worker,
      effect,
      maybe_view: None,
      is_required: false,
    }
  }

  /// The organization requires the background (admin.md §3.5): a picture the effects fail on is
  /// not sent at all (the video stalls), instead of without the effect.
  pub fn required(mut self) -> Self {
    self.is_required = true;
    self
  }

  /// Draws every sent picture into `canvas` too, at the picture's size.
  pub fn with_view(mut self, canvas: HtmlCanvasElement) -> Self {
    self.maybe_view = Some(canvas);
    self
  }
}

/// Background effects in the browser. The segmentation runs in its worker on a small copy of the
/// picture whenever the worker is idle and a mask is due ([`MaskCadence`](crate::effects::MaskCadence));
/// each picture is composited with the newest mask the worker answered with. With WebGPU the
/// picture goes from a canvas (RGBA) through the compositor, which writes it as I420, back into a
/// `VideoFrame`; with WebGL2 it stays on the GPU and the `VideoFrame` is the compositor's canvas.
/// Without an effect the picture goes to the encoders as it is, without either detour.
#[cfg(feature = "effects")]
pub(super) mod effects {
  use std::rc::Rc;
  use std::time::Duration;

  use js_sys::{Object, Uint8Array};
  use wasm_bindgen::JsCast;
  use wasm_bindgen::prelude::*;
  use web_sys::{HtmlCanvasElement, HtmlVideoElement};

  use web_time::Instant;

  use super::super::effects_timing::{EffectsTiming, Stage};
  use super::super::{GlCompositor, SegmentationWorker, WebCallError, browser, dictionary, window};
  use super::CameraEffects;
  use crate::capture_convert::rgbx_to_i420;
  use crate::effects::{
    BackgroundEffect, Compositor, EffectsError, MODEL_INPUT_HEIGHT, MODEL_INPUT_WIDTH, MaskCadence,
    SegmentationMask,
  };
  use crate::web_api::{Canvas2d, VideoFrame as WebVideoFrame};

  /// The segmentation's average time per frame in its worker (a frame lasts 33 ms at 30 fps): a
  /// slower segmenter runs every second or third frame instead, so it leaves the device's CPU to
  /// the page and the encoders.
  const SEGMENTATION_BUDGET: Duration = Duration::from_millis(8);

  pub(in super::super) enum BrowserCompositor {
    WebGpu(Box<Compositor>),
    WebGl(Box<GlCompositor>),
  }

  impl BrowserCompositor {
    /// As the timing names it.
    fn name(&self) -> &'static str {
      match self {
        BrowserCompositor::WebGpu(_) => "webgpu",
        BrowserCompositor::WebGl(_) => "webgl2",
      }
    }
  }

  pub(in super::super) struct WebEffects {
    compositor: BrowserCompositor,
    /// WebGPU: the canvas the camera's picture is read from.
    maybe_reader: Option<(HtmlCanvasElement, Canvas2d)>,
    worker: Rc<SegmentationWorker>,
    effect: BackgroundEffect,
    cadence: MaskCadence,
    /// The newest mask; before the first one, nobody (only the background shows).
    maybe_mask: Option<SegmentationMask>,
    nobody: SegmentationMask,
    /// The self view's canvas and its context.
    maybe_view: Option<(HtmlCanvasElement, Canvas2d)>,
    is_required: bool,
    /// Pictures in a row the effects failed on (sent without the effect where that is allowed).
    failures_in_a_row: u32,
    /// Local timing of the steps (#1336): off unless asked for.
    maybe_timing: Option<EffectsTiming>,
  }

  impl WebEffects {
    pub fn new(effects: CameraEffects) -> Result<Self, WebCallError> {
      let maybe_reader = match effects.compositor {
        BrowserCompositor::WebGpu(_) => {
          let canvas = new_canvas()?;
          let options = dictionary(&[("willReadFrequently", JsValue::TRUE)]).map_err(browser("canvas"))?;
          let context = context_2d(&canvas, &options)?;
          Some((canvas, context))
        },
        BrowserCompositor::WebGl(_) => None,
      };
      let maybe_view = match effects.maybe_view {
        Some(view) => {
          let context = context_2d(&view, &Object::new())?;
          Some((view, context))
        },
        None => None,
      };
      let size = MODEL_INPUT_WIDTH as usize * MODEL_INPUT_HEIGHT as usize;
      let nobody = SegmentationMask::new(MODEL_INPUT_WIDTH, MODEL_INPUT_HEIGHT, vec![0.0; size])
        .map_err(effects_error)?;
      // A mask the worker made for an earlier camera would show where the person was then.
      effects.worker.forget_mask();
      let maybe_timing = EffectsTiming::from_storage(effects.compositor.name());
      Ok(Self {
        compositor: effects.compositor,
        maybe_reader,
        worker: effects.worker,
        effect: effects.effect,
        cadence: MaskCadence::within(SEGMENTATION_BUDGET),
        maybe_mask: None,
        nobody,
        maybe_view,
        is_required: effects.is_required,
        failures_in_a_row: 0,
        maybe_timing,
      })
    }

    /// Times every picture's steps from now on, for one summary ([`WebEffects::timing`]).
    pub fn time_for_bench(&mut self) {
      self.maybe_timing = Some(EffectsTiming::for_bench(self.compositor.name()));
    }

    /// The timing so far, where it is on.
    pub fn timing(&self) -> Option<&EffectsTiming> {
      self.maybe_timing.as_ref()
    }

    /// The encoders' totals of frames given and skipped for a full queue, for the timing.
    pub fn observe_encoders(&mut self, encoded: u64, skipped: u64) {
      if let Some(timing) = &mut self.maybe_timing {
        timing.observe_encoders(encoded, skipped);
      }
    }

    fn record(&mut self, stage: Stage, started: Option<Instant>) {
      if let (Some(timing), Some(started)) = (&mut self.maybe_timing, started) {
        timing.record(stage, started);
      }
    }

    pub fn set_effect(&mut self, effect: BackgroundEffect) {
      if effect != self.effect {
        // A mask from before a pause would show where the person was then.
        self.maybe_mask = None;
        self.worker.forget_mask();
      }
      self.effect = effect;
    }

    /// The current picture with the effect applied (as it is without one), as a `VideoFrame`;
    /// the self view shows it.
    pub async fn frame(
      &mut self,
      video: &HtmlVideoElement,
      timestamp_us: f64,
    ) -> Result<WebVideoFrame, WebCallError> {
      let frame = if self.effect == BackgroundEffect::None {
        plain_frame(video, timestamp_us)?
      } else {
        let started = self.maybe_timing.as_ref().map(|_| Instant::now());
        match self.apply(video, timestamp_us).await {
          Ok(frame) => {
            self.failures_in_a_row = 0;
            self.record(Stage::Total, started);
            if let Some(timing) = &mut self.maybe_timing {
              timing.picture_done();
            }
            frame
          },
          // A required background: the room must not show (the caller skips the picture).
          Err(error) if self.is_required => return Err(error),
          // A GPU hiccup, a lost device or a failed worker: the video goes on without the effect.
          Err(error) => {
            self.failures_in_a_row = self.failures_in_a_row.saturating_add(1);
            if self.failures_in_a_row == 1 {
              tracing::warn!(%error, "background effect skipped");
            }
            plain_frame(video, timestamp_us)?
          },
        }
      };
      self.show(&frame, video);
      Ok(frame)
    }

    /// Draws the sent picture into the self view; a picture it misses is no failure.
    fn show(&self, frame: &WebVideoFrame, video: &HtmlVideoElement) {
      let Some((view, context)) = &self.maybe_view else {
        return;
      };
      let (width, height) = (video.video_width() & !7, video.video_height() & !1);
      if view.width() != width || view.height() != height {
        view.set_width(width);
        view.set_height(height);
      }
      if let Err(error) = context.draw_video_frame(frame, 0.0, 0.0) {
        tracing::debug!(?error, "the self view missed a picture");
      }
    }

    /// The current picture composited with the newest mask; hands the worker the next picture to
    /// segment when one is due and it is idle.
    async fn apply(
      &mut self,
      video: &HtmlVideoElement,
      timestamp_us: f64,
    ) -> Result<WebVideoFrame, WebCallError> {
      if let Some(answer) = self.worker.take_mask() {
        self.cadence.observe(answer.cost);
        if let Some(timing) = &mut self.maybe_timing {
          timing.record_segmentation(answer.cost);
        }
        self.maybe_mask = Some(answer.mask);
      }
      if let Some(failure) = self.worker.maybe_failure() {
        return Err(effects_error(EffectsError::Segmentation(failure)));
      }
      let is_due = self.maybe_mask.is_none() || self.cadence.is_due();
      let wants_picture = is_due && !self.worker.is_busy();
      let Self {
        compositor,
        maybe_reader,
        worker,
        effect,
        maybe_mask,
        nobody,
        maybe_timing,
        ..
      } = self;
      let mask = maybe_mask.as_ref().unwrap_or(nobody);
      // Each step's start, where the timing is on.
      let is_timed = maybe_timing.is_some();
      let start = || is_timed.then(Instant::now);
      let mut record = |stage, started: Option<Instant>| {
        if let (Some(timing), Some(started)) = (maybe_timing.as_mut(), started) {
          timing.record(stage, started);
        }
      };
      match compositor {
        BrowserCompositor::WebGpu(compositor) => {
          let Some((canvas, context)) = maybe_reader else {
            return Err(WebCallError::Unsupported("a canvas to read the camera from"));
          };
          // The compositor needs a width that is a multiple of 8 and even heights (I420).
          let width = video.video_width() & !7;
          let height = video.video_height() & !1;
          if canvas.width() != width || canvas.height() != height {
            canvas.set_width(width);
            canvas.set_height(height);
          }
          let (w, h) = (f64::from(width), f64::from(height));
          let started = start();
          context
            .draw_video(video, 0.0, 0.0, w, h)
            .map_err(browser("canvas"))?;
          let pixels = context
            .get_image_data(0.0, 0.0, w, h)
            .map_err(browser("canvas"))?
            .data();
          record(Stage::Picture, started);
          if wants_picture {
            let started = start();
            let small = shrink(&pixels, width, height);
            worker
              .segment(MODEL_INPUT_WIDTH, MODEL_INPUT_HEIGHT, &small)
              .map_err(effects_error)?;
            record(Stage::ModelInput, started);
          }
          let timestamp = timestamp_us as u64;
          let started = start();
          // The GPU turns the RGBA picture into I420 as it composites (#1295); only a picture too
          // large for its buffers is converted on the CPU first.
          let frame = if compositor.takes_rgba(width, height) {
            compositor
              .apply_rgba(width, height, &pixels, timestamp, mask, effect)
              .await
          } else {
            let frame =
              rgbx_to_i420(&pixels, width, height, timestamp).map_err(|error| WebCallError::Browser {
                context: "effects",
                message: error.to_string(),
              })?;
            compositor.apply(&frame, mask, effect).await
          }
          .map_err(effects_error)?;
          record(Stage::Composite, started);
          let started = start();
          let init: Object = dictionary(&[
            ("format", JsValue::from_str("I420")),
            ("codedWidth", JsValue::from(width)),
            ("codedHeight", JsValue::from(height)),
            ("timestamp", JsValue::from(timestamp_us)),
          ])
          .map_err(browser("VideoFrame"))?;
          let frame = WebVideoFrame::new(&Uint8Array::from(frame.data()).into(), &init)
            .map_err(browser("VideoFrame"))?;
          record(Stage::Frame, started);
          Ok(frame)
        },
        BrowserCompositor::WebGl(compositor) => {
          let started = start();
          let size = compositor.take_picture(video).map_err(effects_error)?;
          record(Stage::Picture, started);
          if wants_picture {
            let started = start();
            let small = compositor.model_input().map_err(effects_error)?;
            worker
              .segment(MODEL_INPUT_WIDTH, MODEL_INPUT_HEIGHT, &small)
              .map_err(effects_error)?;
            record(Stage::ModelInput, started);
          }
          let started = start();
          compositor.composite(size, mask, effect).map_err(effects_error)?;
          record(Stage::Composite, started);
          let started = start();
          let frame = WebVideoFrame::new(
            compositor.canvas().as_ref(),
            &dictionary(&[("timestamp", JsValue::from(timestamp_us))]).map_err(browser("VideoFrame"))?,
          )
          .map_err(browser("VideoFrame"))?;
          record(Stage::Frame, started);
          Ok(frame)
        },
      }
    }
  }

  /// The RGBA picture at the model's input size (nearest sample: the model is far smaller).
  pub(super) fn shrink(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
    let (width, height) = (width as usize, height as usize);
    let (model_width, model_height) = (MODEL_INPUT_WIDTH as usize, MODEL_INPUT_HEIGHT as usize);
    let mut small = Vec::with_capacity(model_width * model_height * 4);
    if width == 0 || height == 0 || pixels.len() < width * height * 4 {
      small.resize(model_width * model_height * 4, 0);
      return small;
    }
    for row in 0..model_height {
      let y = (row * height + height / 2) / model_height;
      for column in 0..model_width {
        let x = (column * width + width / 2) / model_width;
        let at = (y * width + x) * 4;
        small.extend_from_slice(&pixels[at..at + 4]);
      }
    }
    small
  }

  fn plain_frame(video: &HtmlVideoElement, timestamp_us: f64) -> Result<WebVideoFrame, WebCallError> {
    WebVideoFrame::new(
      video.as_ref(),
      &dictionary(&[("timestamp", JsValue::from(timestamp_us))]).map_err(browser("VideoFrame"))?,
    )
    .map_err(browser("VideoFrame"))
  }

  fn new_canvas() -> Result<HtmlCanvasElement, WebCallError> {
    Ok(
      window()?
        .document()
        .ok_or(WebCallError::Unsupported("a document"))?
        .create_element("canvas")
        .map_err(browser("canvas"))?
        .unchecked_into(),
    )
  }

  fn context_2d(canvas: &HtmlCanvasElement, options: &Object) -> Result<Canvas2d, WebCallError> {
    Ok(
      canvas
        .get_context_with_context_options("2d", options)
        .map_err(browser("canvas"))?
        .ok_or(WebCallError::Unsupported("a 2D canvas"))?
        .unchecked_into(),
    )
  }

  fn effects_error(error: EffectsError) -> WebCallError {
    WebCallError::Browser {
      context: "effects",
      message: error.to_string(),
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

  /// Issue #941: Chrome encodes a 1440p share only with a level that allows it.
  #[test]
  fn the_h264_level_follows_the_picture_size() {
    assert_eq!(h264_encoder_codec(1920, 1080), "avc1.42e028");
    assert_eq!(h264_encoder_codec(1280, 720), "avc1.42e028");
    assert_eq!(h264_encoder_codec(2216, 1440), "avc1.42e033");
    assert_eq!(h264_encoder_codec(2560, 1440), "avc1.42e033");
  }

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
      encoders.on_encoded(0, 100, Instant::now()).unwrap();
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

    /// A share published anew in another codec (#565): every layer gets an encoder from the new
    /// opener, starting with a keyframe; the old ones' late failure reports are stale.
    #[test]
    fn reopened_encoders_come_from_the_new_opener_and_start_with_keyframes() {
      let (bench, mut encoders) = encoders(2);
      encoders.encode(&(), false).unwrap();
      let reopened = Rc::clone(&bench);
      encoders
        .reopen(move |info, generation| {
          // The new codec's encoders, told apart by a generation far above the old ones'.
          reopened.borrow_mut().open(info.layer, generation + 100)?;
          Ok(FakeEncoder {
            layer: info.layer,
            generation: generation + 100,
            bench: Rc::clone(&reopened),
          })
        })
        .unwrap();
      assert_eq!(bench.borrow().opened[2..], [(0, 102), (1, 103)]);
      encoders.encode(&(), false).unwrap();
      assert_eq!(bench.borrow().coded[2..], [(0, 102, true), (1, 103, true)]);
      encoders.on_failed(0, 0).unwrap();
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

  /// Issue #939: a camera's layers follow the engine's `LayerBitrates` like the desktop's.
  mod layer_bitrates {
    use super::*;

    #[test]
    fn a_new_bitrate_configures_the_running_encoder_again_without_a_keyframe() {
      let (bench, mut encoders) = encoders(2);
      encoders.encode(&(), false).unwrap();
      encoders.set_bitrates(&[(0, 150_000), (1, 210_000)]).unwrap();
      // Layer 0 keeps its bitrate and is left alone; layer 1 keeps its encoder.
      assert_eq!(bench.borrow().opened, vec![(0, 0), (1, 1)]);
      assert_eq!(
        bench.borrow().rates,
        vec![(0, 150_000), (1, 300_000), (1, 210_000)]
      );
      encoders.encode(&(), false).unwrap();
      assert_eq!(bench.borrow().coded[2..], [(0, 0, false), (1, 1, false)]);
      // Back to its whole bitrate, the same way; the layers as published don't change.
      encoders.set_bitrates(&[(0, 150_000), (1, 300_000)]).unwrap();
      assert_eq!(bench.borrow().rates[2..], [(1, 210_000), (1, 300_000)]);
      assert_eq!(encoders.infos(), vec![layer_info(0), layer_info(1)]);
    }

    #[test]
    fn unchanged_zero_and_unknown_bitrates_change_nothing() {
      let (bench, mut encoders) = encoders(2);
      encoders
        .set_bitrates(&[(0, 150_000), (1, 0), (7, 90_000)])
        .unwrap();
      encoders.set_bitrates(&[]).unwrap();
      assert_eq!(bench.borrow().rates.len(), 2);
      assert_eq!(bench.borrow().opened.len(), 2);
    }

    #[test]
    fn an_encoder_that_refuses_the_bitrate_is_replaced_at_it_with_a_keyframe() {
      let (bench, mut encoders) = encoders(2);
      encoders.encode(&(), false).unwrap();
      bench.borrow_mut().refusing.insert(1);
      encoders.set_bitrates(&[(1, 210_000)]).unwrap();
      assert_eq!(bench.borrow().opened, vec![(0, 0), (1, 1), (1, 2)]);
      assert_eq!(bench.borrow().rates[2..], [(2, 210_000)]);
      encoders.encode(&(), false).unwrap();
      assert_eq!(bench.borrow().coded[2..], [(0, 0, false), (1, 2, true)]);
    }

    #[test]
    fn encoders_that_keep_refusing_give_up() {
      let (bench, mut encoders) = encoders(1);
      for generation in 0..=u64::from(MAX_ENCODER_RESTARTS) {
        bench.borrow_mut().refusing.insert(generation);
      }
      for step in 0..u32::from(MAX_ENCODER_RESTARTS) {
        encoders.set_bitrates(&[(0, 100_000 - step)]).unwrap();
      }
      assert_eq!(
        encoders.set_bitrates(&[(0, 50_000)]),
        Err(EncoderGaveUp { layer: 0 })
      );
    }

    #[test]
    fn a_closed_encoder_is_replaced_at_the_new_bitrate_with_the_next_frame() {
      let (bench, mut encoders) = encoders(1);
      bench.borrow_mut().closed.insert(0);
      encoders.set_bitrates(&[(0, 75_000)]).unwrap();
      // Nothing configures a closed encoder; the next frame replaces it.
      assert_eq!(bench.borrow().rates.len(), 1);
      encoders.encode(&(), false).unwrap();
      assert_eq!(bench.borrow().opened, vec![(0, 0), (0, 1)]);
      assert_eq!(bench.borrow().rates[1..], [(1, 75_000)]);
    }

    /// The desktop's rule (`SimulcastEncoder::resize`): a bitrate lowered for the uplink stays
    /// lowered when the source changes size; a whole one becomes the new size's.
    #[test]
    fn a_lowered_bitrate_outlives_a_resize_and_a_failure() {
      let (bench, mut encoders) = encoders(2);
      encoders.set_bitrates(&[(1, 210_000)]).unwrap();
      let resized: Vec<VideoLayerInfo> = encoders
        .infos()
        .into_iter()
        .map(|info| VideoLayerInfo {
          max_bitrate_bps: info.max_bitrate_bps * 2,
          ..info
        })
        .collect();
      encoders.resize(&resized).unwrap();
      assert_eq!(encoders.infos(), resized);
      assert_eq!(bench.borrow().rates[3..], [(2, 300_000), (3, 210_000)]);
      encoders.on_failed(1, 3).unwrap();
      assert_eq!(bench.borrow().rates[5..], [(4, 210_000)]);
    }

    #[test]
    fn a_paused_layer_resumes_at_the_bitrate_it_got_meanwhile() {
      let (bench, mut encoders) = encoders(2);
      encoders.set_needed_layers(&[0]);
      encoders.set_bitrates(&[(1, 180_000)]).unwrap();
      assert_eq!(bench.borrow().rates[2..], [(1, 180_000)]);
      encoders.set_needed_layers(&[0, 1]);
      encoders.encode(&(), false).unwrap();
      assert_eq!(bench.borrow().coded, vec![(0, 0, true), (1, 1, true)]);
      assert_eq!(bench.borrow().opened.len(), 2);
    }
  }

  /// Issue #1762: a camera's layers are held to their bitrate, and its frame rate follows the
  /// encoders' load.
  /// #1733: a share's layers at their own frame rates, a limited uplink lowering the top's
  /// rate instead of its size, and the ladder's steps applied in place or by new encoders.
  mod share_layers {
    use super::*;

    #[test]
    fn a_slower_layer_takes_every_n_th_frame() {
      let (bench, mut encoders) = encoders(2);
      let mut infos = encoders.infos();
      infos[0].max_framerate = 15;
      assert!(encoders.update_infos(&infos).unwrap());
      for _ in 0..4 {
        encoders.encode(&(), false).unwrap();
      }
      let coded = &bench.borrow().coded;
      assert_eq!(coded.iter().filter(|(layer, ..)| *layer == 0).count(), 2);
      assert_eq!(coded.iter().filter(|(layer, ..)| *layer == 1).count(), 4);
    }

    /// The uplink gives the top layer a fifth of its bitrate: it keeps its size and gets a
    /// fifth of the frames, at a matching frame rate.
    #[test]
    fn a_lower_bitrate_lowers_the_frame_rate_not_the_size() {
      let (bench, mut encoders) = encoders(2);
      encoders.set_bitrates(&[(1, 60_000)]).unwrap();
      assert_eq!(bench.borrow().framerates.last(), Some(&(1, 6)));
      for _ in 0..10 {
        encoders.encode(&(), false).unwrap();
      }
      let top = bench
        .borrow()
        .coded
        .iter()
        .filter(|(layer, ..)| *layer == 1)
        .count();
      assert_eq!(top, 2);
      assert_eq!(encoders.infos()[1].width, 640);
    }

    /// A rate step keeps the encoders; a new codec or a layer less doesn't fit in place.
    #[test]
    fn rate_steps_apply_in_place_other_steps_need_new_layers() {
      let (bench, mut encoders) = encoders(2);
      let mut slower = encoders.infos();
      slower[1].max_framerate = 10;
      assert!(encoders.update_infos(&slower).unwrap());
      assert_eq!(bench.borrow().opened.len(), 2, "no new encoder");
      assert_eq!(bench.borrow().framerates.last(), Some(&(1, 10)));
      let mut av1 = slower.clone();
      av1[1].maybe_codec = Some(VideoCodec::Av1);
      assert!(!encoders.update_infos(&av1).unwrap());
      assert!(!encoders.update_infos(&slower[..1]).unwrap());
      assert_eq!(encoders.infos(), slower, "nothing changed");
    }

    #[test]
    fn replaced_layers_get_new_encoders_that_start_with_keyframes() {
      let (bench, mut encoders) = encoders(3);
      encoders.encode(&(), false).unwrap();
      let shared = Rc::clone(&bench);
      let mut fewer = encoders.infos();
      fewer.remove(1);
      fewer[1].layer = 1;
      encoders
        .replace_layers(fewer.clone(), move |info, generation| {
          shared.borrow_mut().open(info.layer, generation)?;
          Ok(FakeEncoder {
            layer: info.layer,
            generation,
            bench: Rc::clone(&shared),
          })
        })
        .unwrap();
      assert_eq!(encoders.infos(), fewer);
      assert_eq!(bench.borrow().opened[3..], [(0, 3), (1, 4)]);
      encoders.encode(&(), false).unwrap();
      assert_eq!(bench.borrow().coded[3..], [(0, 3, true), (1, 4, true)]);
    }
  }

  mod governed {
    use std::time::Duration;

    use super::*;

    #[test]
    fn an_overshooting_layer_is_configured_lower_then_gets_fewer_frames() {
      let (bench, mut encoders) = encoders(2);
      let start = Instant::now();
      let config = CameraGovernorConfig::default();
      encoders.govern(config, 30, start);
      // Layer 0 sends 5 kB per picture (1.2 Mbit/s for 150 kbit/s); layer 1 keeps to its bitrate.
      let mut now = start;
      run(&mut encoders, &mut now, 64, &[(0, 5_000), (1, 1_000)]);
      // The bitrate step: the running encoder configured at half its bitrate, at 30 fps.
      assert_eq!(bench.borrow().rates[2..], [(0, 75_000)]);
      assert_eq!(bench.borrow().framerates, vec![(0, 30)]);
      run(&mut encoders, &mut now, 64, &[(0, 5_000), (1, 1_000)]);
      // Still too much: every second frame, the encoder told so.
      assert_eq!(bench.borrow().framerates[1..], [(0, 15)]);
      assert_eq!(encoders.layer_limits(), vec![(0, 75_000, 15)]);
      let coded_before = bench.borrow().coded.len();
      for _ in 0..4 {
        encoders.encode(&(), false).unwrap();
      }
      let layer_0 = bench.borrow().coded[coded_before..]
        .iter()
        .filter(|(layer, ..)| *layer == 0)
        .count();
      assert_eq!(layer_0, 2);
      // Layer 1, within its bitrate, was never configured again.
      assert!(
        bench
          .borrow()
          .framerates
          .iter()
          .all(|(generation, _)| *generation == 0)
      );
    }

    /// A periodic keyframe that falls on a frame a limited layer doesn't get comes with its next.
    #[test]
    fn a_limited_layer_takes_a_periodic_keyframe_on_its_next_frame() {
      let (bench, mut encoders) = encoders(1);
      let start = Instant::now();
      encoders.govern(CameraGovernorConfig::default(), 30, start);
      let mut now = start;
      run(&mut encoders, &mut now, 150, &[(0, 5_000)]);
      assert_eq!(encoders.layer_limits(), vec![(0, 75_000, 15)]);
      // Line the frames up so that the periodic keyframe falls on a skipped one.
      if encoders.frame_index % 2 == 0 {
        encoders.encode(&(), false).unwrap();
      }
      let coded_before = bench.borrow().coded.len();
      encoders.encode(&(), true).unwrap();
      encoders.encode(&(), false).unwrap();
      assert_eq!(bench.borrow().coded[coded_before..], [(0, 0, true)]);
    }

    #[test]
    fn a_lower_capture_frame_rate_configures_every_layer() {
      let (bench, mut encoders) = encoders(2);
      encoders.govern(CameraGovernorConfig::default(), 30, Instant::now());
      encoders.set_framerate(20).unwrap();
      assert_eq!(bench.borrow().framerates, vec![(0, 20), (1, 20)]);
      // The bitrates stay.
      assert_eq!(bench.borrow().rates[2..], [(0, 150_000), (1, 300_000)]);
      encoders.set_framerate(20).unwrap();
      assert_eq!(bench.borrow().framerates.len(), 2);
    }

    #[test]
    fn the_tally_counts_frames_given_skipped_and_queued() {
      let (bench, mut encoders) = encoders(2);
      bench.borrow_mut().queue = 1;
      let tally = encoders.encode(&(), false).unwrap();
      assert_eq!((tally.given, tally.skipped, tally.queued), (2, 0, 2));
      bench.borrow_mut().queue = MAX_ENCODE_QUEUE + 1;
      let tally = encoders.encode(&(), false).unwrap();
      assert_eq!((tally.given, tally.skipped, tally.queued), (0, 2, 0));
    }

    /// Ungoverned encoders (a screen share) are configured at their bitrate alone.
    #[test]
    fn ungoverned_layers_have_no_limits() {
      let (bench, mut encoders) = encoders(1);
      let mut now = Instant::now();
      run(&mut encoders, &mut now, 30 * 6, &[(0, 50_000)]);
      assert!(encoders.layer_limits().is_empty());
      assert_eq!(bench.borrow().rates, vec![(0, 150_000)]);
    }

    /// Gives `frames` frames at 30 fps, each layer of `outputs` giving a picture of its bytes.
    fn run(
      encoders: &mut LayerEncoders<FakeEncoder>,
      now: &mut Instant,
      frames: u32,
      outputs: &[(u8, usize)],
    ) {
      for _ in 0..frames {
        *now += Duration::from_millis(33);
        let before = encoders.frame_index;
        encoders.encode(&(), false).unwrap();
        for &(layer, bytes) in outputs {
          let divisor = encoders
            .layers
            .iter()
            .position(|current| current.info.layer == layer)
            .map_or(1, |index| encoders.limit(index).frame_divisor);
          if before.is_multiple_of(u64::from(divisor)) {
            encoders.on_encoded(layer, bytes, *now).unwrap();
          }
        }
      }
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
    /// Encoders' bitrates by generation: each opened one's and each configured again.
    rates: Vec<(u64, u32)>,
    /// Generations whose `configure` throws.
    refusing: HashSet<u64>,
    /// Encoders' frame rates by generation, each time they are configured again.
    framerates: Vec<(u64, u8)>,
    /// Frames waiting in every encoder.
    queue: u32,
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
        rates: Vec::new(),
        refusing: HashSet::new(),
        framerates: Vec::new(),
        queue: 0,
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
      self.bench.borrow().queue
    }

    fn encode(&self, _frame: &(), is_keyframe: bool) -> Result<(), WebCallError> {
      self
        .bench
        .borrow_mut()
        .code(self.layer, self.generation, is_keyframe)
    }

    fn configure(&mut self, bitrate_bps: u32, framerate: u8) -> Result<(), WebCallError> {
      let mut bench = self.bench.borrow_mut();
      if bench.refusing.contains(&self.generation) {
        return Err(failure("configure"));
      }
      bench.rates.push((self.generation, bitrate_bps));
      bench.framerates.push((self.generation, framerate));
      Ok(())
    }
  }

  impl FrameDecoder for FakeDecoder {
    fn codec(&self) -> &VideoCodec {
      &self.codec
    }

    fn is_configured(&self) -> bool {
      !self.bench.borrow().closed.contains(&self.generation)
    }

    fn queue_size(&self) -> u32 {
      0
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
      let mut bench = shared.borrow_mut();
      bench.open(info.layer, generation)?;
      bench.rates.push((generation, info.max_bitrate_bps));
      drop(bench);
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
      maybe_codec: None,
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
