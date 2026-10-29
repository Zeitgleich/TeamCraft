//! The threads between capture and engine: [`VideoSender`] runs capture → (effects) → simulcast
//! encoding, [`VideoReceiver`] decodes one received track for the app. Encoding and decoding take
//! milliseconds per frame, so they stay off the call's async task.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self as std_mpsc, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use teamcraft_proto::{VideoLayerInfo, VideoTrackKind};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::call_video::ReceivedVideoFrame;
use crate::picture_check::{PictureContent, picture_content};
use crate::video::{
  EncodedVideoFrame, LayerSizer, VideoFrame, VideoResolution, fit_within, within_layer_limit,
};
use crate::video_capture::{CaptureError, FrameReceiver, VideoCapture, frame_queue};
use crate::video_codec::{
  FallbackEncoder, OpenH264Decoder, OpenH264Encoder, SimulcastEncoder, VideoCodecError, VideoContent,
  VideoDecoder, VideoEncoder,
};
use crate::worker_thread::{Joined, WorkerThread};

/// How often the encoder thread looks at controls while no frame arrives.
const CONTROL_POLL: Duration = Duration::from_millis(20);
/// How long a started track may take to its first encoded picture before the start counts as
/// failed (a camera that delivers nothing, a capture stuck in negotiation, no working encoder).
/// Generous: some cameras take a few seconds to wake up.
pub const FIRST_PICTURE_TIMEOUT: Duration = Duration::from_secs(10);
/// Repeated failures of one kind (a frame not encoded, not sent) are logged at most this often.
const REPEATED_WARNING_INTERVAL: Duration = Duration::from_secs(10);
/// A keyframe request still open this long after the last picture is answered with the last
/// picture again: the capture delivers nothing while a screen stands still, and the request
/// would wait for the next change (issue #683). Longer than a frame of any capture that runs.
const KEYFRAME_REFRESH_DELAY: Duration = Duration::from_millis(150);
/// The default [`VideoSendConfig::maybe_idle_refresh`] of screen shares: a still screen's
/// picture again about once a second, so a viewer never waits long for one.
pub const SCREEN_IDLE_REFRESH: Duration = Duration::from_secs(1);
/// How long an own track may show nothing but black before it is reported: longer than a camera
/// takes to adjust to the light when it starts, short enough to tell the user while they look.
pub const BLACK_PICTURES_REPORT_AFTER: Duration = Duration::from_secs(3);
/// How long stopping a track waits for its encoder thread, which stops the capture (cameras wait
/// up to half a second for theirs, `camera_thread`). Bounded: a capture stuck in its driver must
/// not leave a thread waiting for good (issue #683); the call's task doesn't wait for it at all
/// ([`VideoSender::stop_in_background`]).
const SENDER_STOP_WAIT: Duration = Duration::from_secs(1);
/// How often the encoder thread notes which encoders work, for the call's stats (#938): an
/// encoder falls back to software rarely, and the stats are read once a second.
const ENCODER_NAMES_INTERVAL: Duration = Duration::from_secs(1);
/// How long [`VideoSender::start`] waits for the first frame of a track whose layers are planned
/// from it ([`VideoSendConfig::maybe_layer_plan`]). Most cameras deliver within a second; one that
/// takes longer gets the layers planned for the size asked for, and the start check
/// ([`FIRST_PICTURE_TIMEOUT`]) still applies after it.
pub const FIRST_FRAME_WAIT: Duration = Duration::from_secs(3);

/// Makes the encoder for one layer; hardware where available (see [`software_encoders`]).
pub type EncoderFactory =
  Arc<dyn Fn(&VideoLayerInfo, VideoContent) -> Result<Box<dyn VideoEncoder>, VideoCodecError> + Send + Sync>;

/// Turns frames before encoding, e.g. background effects. Runs on the encoder thread.
pub type FrameFilter = Box<dyn FnMut(VideoFrame) -> VideoFrame + Send>;

/// Plans a track's layers for the size its source delivers ([`VideoSendConfig::maybe_layer_plan`]).
pub type LayerPlanner = Box<dyn FnOnce(VideoResolution) -> Vec<VideoLayerInfo> + Send>;

/// Gets decoded frames of a watched track, on the decoder thread.
pub type FrameSink = Box<dyn FnMut(VideoFrame) + Send>;

/// Called on the decoder thread when a frame didn't decode: the track needs a keyframe.
pub type KeyframeNeeded = Box<dyn FnMut() + Send>;

/// Drops frames that come faster than the send rate. Some slack keeps a 30 fps camera whose
/// timestamps jitter from losing every other frame.
struct FrameLimiter {
  min_interval_us: u64,
  maybe_last_us: Option<u64>,
}

impl FrameLimiter {
  /// `None` or 0: no limit.
  fn new(maybe_max_framerate: Option<u8>) -> Self {
    let min_interval_us = match maybe_max_framerate {
      Some(rate) if rate > 0 => 1_000_000 / u64::from(rate) * 9 / 10,
      _ => 0,
    };
    Self {
      min_interval_us,
      maybe_last_us: None,
    }
  }

  fn is_due(&mut self, timestamp_us: u64) -> bool {
    if let Some(last) = self.maybe_last_us
      && timestamp_us.saturating_sub(last) < self.min_interval_us
      && timestamp_us >= last
    {
      return false;
    }
    self.maybe_last_us = Some(timestamp_us);
    true
  }
}

/// One published track's capture and encoder thread.
pub struct VideoSender {
  pub kind: VideoTrackKind,
  pub layers: Vec<VideoLayerInfo>,
  controls: std_mpsc::Sender<SenderControl>,
  counters: Arc<SendCounters>,
  thread: Option<WorkerThread>,
}

/// A track being stopped: its threads end on a blocking task, the call's task goes on
/// ([`VideoSender::stop_in_background`]). Its counters stay readable, final once
/// [`StoppingSender::is_stopped`].
pub struct StoppingSender {
  pub kind: VideoTrackKind,
  counters: Arc<SendCounters>,
  task: tokio::task::JoinHandle<()>,
}

/// Resolves once a started track encoded its first picture, or with why it never will. While
/// nobody waits for it (dropped), the track gets as long as it needs.
#[derive(Debug)]
pub struct FirstPicture {
  started: oneshot::Receiver<Result<(), VideoStartError>>,
  maybe_black: Option<BlackPictures>,
}

/// Resolves when a running track has shown nothing but black for
/// [`BLACK_PICTURES_REPORT_AFTER`] (issue #683): a covered camera, a screen capture that macOS
/// blanks until the app restarts after the Screen Recording permission was granted, protected
/// video. Once per track; the track goes on (black may be what the user shares).
#[derive(Debug)]
pub struct BlackPictures(oneshot::Receiver<BlackReport>);

/// What a [`BlackPictures`] notice says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlackReport {
  /// Black frames in a row so far.
  pub frames: u64,
}

/// Why a started track sends nothing.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VideoStartError {
  #[error("no video encoder: {0}")]
  NoEncoder(String),
  #[error("the {0} delivered no picture")]
  NoFrames(&'static str),
  /// `detail`: the encoders and what they made of the last picture, e.g. `OpenH264: …` with the
  /// error, or that they gave none (#935).
  #[error("the {source_name} delivered {captured} pictures, but none could be encoded: {detail}")]
  NotEncoded {
    source_name: &'static str,
    captured: u64,
    detail: String,
  },
  #[error("the {0} stopped before its first picture")]
  CaptureEnded(&'static str),
  #[error("the video thread ended")]
  ThreadEnded,
}

/// Pictures of one own track so far; the encoder thread counts, the call reads.
#[derive(Debug, Default)]
struct SendCounters {
  captured: AtomicU64,
  encoded: AtomicU64,
  /// Captured frames that were (nearly) black ([`PictureContent::Black`]).
  black: AtomicU64,
  /// The size of the last captured frame, `width << 16 | height`; 0 before the first.
  capture_size: AtomicU32,
  /// The encoders at work ([`SimulcastEncoder::encoder_names`]), noted now and then.
  encoder_names: Mutex<String>,
  /// Waits for a frame that ended without one: lets tests know the thread idled.
  #[cfg(test)]
  idle_polls: AtomicU64,
}

/// What the encoder thread has done so far: frames taken from the capture and pictures encoded
/// (all layers together).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SendProgress {
  pub frames_captured: u64,
  pub pictures_encoded: u64,
  /// Captured frames that showed nothing but black.
  pub frames_black: u64,
}

/// What an own track's encoder thread works with now (stats for nerds, #938).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SenderDetails {
  /// The size the capture delivers, once a frame came.
  pub maybe_capture: Option<VideoResolution>,
  /// The encoders at work, e.g. `vtenc_h264` or `vtenc_h264, OpenH264` once a layer fell back;
  /// empty until the first picture.
  pub encoder_names: String,
}

/// Logs a failure that may repeat every frame: the first at once, then at most once per
/// [`REPEATED_WARNING_INTERVAL`] with how often it happened meanwhile.
#[derive(Debug, Default)]
pub(crate) struct RepeatedWarning {
  maybe_last: Option<Instant>,
  suppressed: u64,
}

/// What to publish and how to encode it.
pub struct VideoSendConfig {
  pub kind: VideoTrackKind,
  pub capture: Box<dyn VideoCapture>,
  /// Simulcast layers, lowest first (one for screen shares).
  pub layers: Vec<VideoLayerInfo>,
  /// Plans the layers anew for the size of the first frame, which [`VideoSender::start`] waits
  /// for (at most [`FIRST_FRAME_WAIT`]); `layers` stay when none came by then. A camera asked for
  /// 1080p may deliver less, and its layers are then published as they are sent (#967): viewers
  /// pick by them, and the uplink gives each its bitrate.
  pub maybe_layer_plan: Option<LayerPlanner>,
  pub encoders: EncoderFactory,
  pub maybe_filter: Option<FrameFilter>,
  /// Encodes the last picture again after this long without a new frame. Screen captures only
  /// deliver when something changes ([`VideoSendConfig::default_idle_refresh`]); `None` for
  /// cameras, which deliver all the time.
  pub maybe_idle_refresh: Option<Duration>,
}

/// One watched track's decoder thread.
pub struct VideoReceiver {
  frames: std_mpsc::Sender<ReceivedVideoFrame>,
  thread: Option<JoinHandle<()>>,
}

enum SenderControl {
  Keyframe(u8),
  NeededLayers(Vec<u8>),
  Bitrates(Vec<(u8, u32)>),
}

/// Hardware encoders where the platform has them (Android: MediaCodec; Linux and macOS with the
/// `gstreamer` feature: VA-API, VideoToolbox), each falling back to OpenH264 when it can't be
/// built for a layer or fails later; OpenH264 everywhere else.
pub fn preferred_encoders() -> EncoderFactory {
  Arc::new(|layer, content| match hardware_encoder(layer, content) {
    Some(hardware) => Ok(Box::new(FallbackEncoder::new(hardware, layer, content)) as Box<dyn VideoEncoder>),
    None => Ok(Box::new(OpenH264Encoder::new(layer, content)?) as Box<dyn VideoEncoder>),
  })
}

#[cfg(target_os = "android")]
fn hardware_encoder(layer: &VideoLayerInfo, content: VideoContent) -> Option<Box<dyn VideoEncoder>> {
  crate::video_mediacodec::MediaCodecEncoder::new(layer, content)
    .map(|encoder| Box::new(encoder) as Box<dyn VideoEncoder>)
}

#[cfg(all(feature = "gstreamer", not(target_os = "android")))]
fn hardware_encoder(layer: &VideoLayerInfo, content: VideoContent) -> Option<Box<dyn VideoEncoder>> {
  crate::video_gstreamer::GstreamerEncoder::new(layer, content)
    .map(|encoder| Box::new(encoder) as Box<dyn VideoEncoder>)
}

#[cfg(not(any(feature = "gstreamer", target_os = "android")))]
fn hardware_encoder(_layer: &VideoLayerInfo, _content: VideoContent) -> Option<Box<dyn VideoEncoder>> {
  None
}

/// The software encoders (OpenH264), for platforms without hardware encoding or as a fallback.
pub fn software_encoders() -> EncoderFactory {
  Arc::new(|layer, content| Ok(Box::new(OpenH264Encoder::new(layer, content)?) as Box<dyn VideoEncoder>))
}

impl VideoSendConfig {
  /// The idle refresh a track of `kind` gets unless the app sets another: screen shares
  /// [`SCREEN_IDLE_REFRESH`], cameras none.
  pub fn default_idle_refresh(kind: &VideoTrackKind) -> Option<Duration> {
    (*kind == VideoTrackKind::Screen).then_some(SCREEN_IDLE_REFRESH)
  }

  /// The same track with at most `max_layers` layers, the top ones (#978,
  /// [`crate::video::within_layer_limit`]), also where they are planned from the first frame:
  /// what the SFU takes ([`crate::call_engine::CallEngine::simulcast_layer_limit`]).
  pub fn within_layer_limit(mut self, max_layers: u8) -> Self {
    self.layers = within_layer_limit(&self.layers, max_layers);
    if let Some(plan) = self.maybe_layer_plan.take() {
      self.maybe_layer_plan = Some(Box::new(move |source| {
        within_layer_limit(&plan(source), max_layers)
      }));
    }
    self
  }
}

impl VideoSender {
  /// Starts capturing and encoding; frames go to `encoded` tagged with the track kind. The
  /// [`FirstPicture`] tells whether pictures come; dropping it lets the track take as long as it
  /// needs.
  pub fn start(
    config: VideoSendConfig,
    encoded: mpsc::UnboundedSender<(VideoTrackKind, EncodedVideoFrame)>,
  ) -> Result<(Self, FirstPicture), CaptureError> {
    let VideoSendConfig {
      kind,
      mut capture,
      layers,
      maybe_layer_plan,
      encoders,
      maybe_filter,
      maybe_idle_refresh,
    } = config;
    let content = match kind {
      VideoTrackKind::Screen => VideoContent::Screen,
      _ => VideoContent::Camera,
    };
    let (frames, frame_rx) = frame_queue();
    let stall = StallWatch::new(capture.stall_limit());
    capture.start(frames)?;
    let (layers, maybe_first_frame) = match maybe_layer_plan {
      Some(plan) => match frame_rx.recv_timeout(FIRST_FRAME_WAIT) {
        Ok(frame) => (plan(frame.resolution()), Some(frame)),
        // No frame yet (the start check follows), or the capture ended (the loop finds out).
        Err(_) => (layers, None),
      },
      None => (layers, None),
    };
    let (controls, control_rx) = std_mpsc::channel();
    let (first_picture, first_picture_rx) = oneshot::channel();
    let (black_notice, black_rx) = oneshot::channel();
    let counters = Arc::new(SendCounters::default());
    let encoder_loop = EncoderLoop {
      kind: kind.clone(),
      layers: layers.clone(),
      sizer: LayerSizer::new(
        layers.clone(),
        largest_resolution(&layers).unwrap_or(VideoResolution { width: 0, height: 0 }),
      ),
      content,
      encoders,
      maybe_filter,
      refresh: Refresh {
        maybe_idle_interval: maybe_idle_refresh,
      },
      frame_rx,
      maybe_pending_frame: maybe_first_frame,
      control_rx,
      encoded,
      counters: Arc::clone(&counters),
      maybe_first_picture: Some(first_picture),
      maybe_no_picture: None,
      maybe_names_noted_at: None,
      black: BlackWatch::new(black_notice),
      stall,
    };
    let thread = WorkerThread::spawn("video-encoder", move || {
      encoder_loop.run();
      capture.stop();
    })
    .map_err(|error| CaptureError::Device(error.to_string()))?;
    let sender = Self {
      kind,
      layers,
      controls,
      counters,
      thread: Some(thread),
    };
    let first_picture = FirstPicture {
      started: first_picture_rx,
      maybe_black: Some(BlackPictures(black_rx)),
    };
    Ok((sender, first_picture))
  }

  pub fn request_keyframe(&self, layer: u8) {
    // A stopped thread needs no keyframe.
    let _ = self.controls.send(SenderControl::Keyframe(layer));
  }

  /// Asks for a keyframe on every layer: the track's first frames may have been dropped (not yet
  /// published, the call not yet secured).
  pub fn request_keyframes(&self) {
    for layer in &self.layers {
      self.request_keyframe(layer.layer);
    }
  }

  pub fn set_needed_layers(&self, layers: Vec<u8>) {
    let _ = self.controls.send(SenderControl::NeededLayers(layers));
  }

  /// Encodes the layers at `bitrates` (layer, bit/s) from now on; the engine's
  /// `LayerBitrates`.
  pub fn set_layer_bitrates(&self, bitrates: Vec<(u8, u32)>) {
    let _ = self.controls.send(SenderControl::Bitrates(bitrates));
  }

  pub fn progress(&self) -> SendProgress {
    self.counters.progress()
  }

  pub fn details(&self) -> SenderDetails {
    self.counters.details()
  }
}

impl VideoSender {
  /// The capture ended without being stopped (the share was ended from the system's indicator,
  /// the shared window closed), no encoder could be built or the first picture never came: the
  /// track sends nothing any more.
  pub fn has_ended(&self) -> bool {
    self.thread.as_ref().is_some_and(WorkerThread::is_finished)
  }
}

/// The encoder thread of one track.
struct EncoderLoop {
  kind: VideoTrackKind,
  /// The layers as planned at the start: their top is the largest picture sent.
  layers: Vec<VideoLayerInfo>,
  /// Keeps the encoders in the shape of the pictures (a shared window resized, #931).
  sizer: LayerSizer,
  content: VideoContent,
  encoders: EncoderFactory,
  maybe_filter: Option<FrameFilter>,
  refresh: Refresh,
  frame_rx: FrameReceiver,
  /// A frame taken from the capture before the loop ran (the one the layers were planned for).
  maybe_pending_frame: Option<VideoFrame>,
  control_rx: std_mpsc::Receiver<SenderControl>,
  encoded: mpsc::UnboundedSender<(VideoTrackKind, EncodedVideoFrame)>,
  counters: Arc<SendCounters>,
  maybe_first_picture: Option<oneshot::Sender<Result<(), VideoStartError>>>,
  /// Why the last frame gave no picture, while the first picture is pending: the start check's
  /// failure names the encoders and their error (#935).
  maybe_no_picture: Option<String>,
  /// When the encoders at work were last noted for the stats ([`ENCODER_NAMES_INTERVAL`]).
  maybe_names_noted_at: Option<Instant>,
  black: BlackWatch,
  stall: StallWatch,
}

/// Notices a track that shows nothing but black, from the content of its captured pictures.
#[derive(Debug)]
struct BlackWatch {
  maybe_black_since: Option<Instant>,
  black_in_a_row: u64,
  is_reported: bool,
  maybe_notice: Option<oneshot::Sender<BlackReport>>,
}

/// Notices a capture that stopped delivering: nothing for its [`VideoCapture::stall_limit`] since
/// its last frame (#922). Before the first frame [`FIRST_PICTURE_TIMEOUT`] applies instead. On
/// macOS `Instant` stands still while the computer sleeps, so a camera gets the whole limit to come
/// back after a wake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StallWatch {
  maybe_limit: Option<Duration>,
  maybe_last_frame_at: Option<Instant>,
}

/// A change [`BlackWatch`] noticed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlackChange {
  /// Only black for [`BLACK_PICTURES_REPORT_AFTER`].
  Black(BlackReport),
  /// A picture with content after a report.
  Recovered,
}

/// Whether the encoder thread goes on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoopStep {
  Continue,
  Stop,
}

/// When the encoder thread encodes its last picture again without a new frame from the capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Refresh {
  maybe_idle_interval: Option<Duration>,
}

/// The picture encoded last (scaled and filtered), for [`Refresh`].
struct LastPicture {
  frame: VideoFrame,
  encoded_at: Instant,
}

impl EncoderLoop {
  fn run(mut self) {
    let encoders = Arc::clone(&self.encoders);
    let content = self.content;
    let mut simulcast = match SimulcastEncoder::new(&self.layers, |layer| encoders(layer, content)) {
      Ok(simulcast) => simulcast,
      Err(error) => {
        warn!(%error, kind = ?self.kind, "no video encoder; the track sends nothing");
        self.report_start(Err(VideoStartError::NoEncoder(error.to_string())));
        return;
      },
    };
    let send_resolution = largest_resolution(&self.layers);
    let mut limiter = FrameLimiter::new(self.layers.iter().map(|layer| layer.max_framerate).max());
    let mut encode_warning = RepeatedWarning::default();
    let started_at = Instant::now();
    let mut maybe_last: Option<LastPicture> = None;
    loop {
      if self.apply_controls(&mut simulcast) == LoopStep::Stop {
        return;
      }
      if self.check_first_picture(Instant::now(), started_at) == LoopStep::Stop {
        return;
      }
      if self.check_stall(Instant::now()) == LoopStep::Stop {
        return;
      }
      // A still screen delivers no frames: its black is noticed with time passing.
      if let Some(change) = self.black.poll(Instant::now()) {
        self.report_black(change);
      }
      if let Some(frame) = self.due_refresh(&simulcast, maybe_last.as_ref(), Instant::now()) {
        match self.encode_and_send(&mut simulcast, frame, &mut encode_warning) {
          Ok(last) => maybe_last = Some(last),
          Err(LoopStep::Stop) => return,
          // Not encoded: the next try after another wait, not at once.
          Err(LoopStep::Continue) => {
            if let Some(last) = &mut maybe_last {
              last.encoded_at = Instant::now();
            }
          },
        }
        continue;
      }
      let received = match self.maybe_pending_frame.take() {
        Some(frame) => Ok(frame),
        None => self.frame_rx.recv_timeout(CONTROL_POLL),
      };
      let frame = match received {
        Ok(frame) => frame,
        Err(RecvTimeoutError::Timeout) => {
          #[cfg(test)]
          self.counters.idle_polls.fetch_add(1, Ordering::Relaxed);
          continue;
        },
        Err(RecvTimeoutError::Disconnected) => {
          warn!(kind = ?self.kind, "the video capture ended");
          self.report_start(Err(VideoStartError::CaptureEnded(self.source_name())));
          return;
        },
      };
      self.stall.on_frame(Instant::now());
      // A request that came while waiting (a viewer asked for a keyframe) applies to this frame.
      if self.apply_controls(&mut simulcast) == LoopStep::Stop {
        return;
      }
      self.counters.captured.fetch_add(1, Ordering::Relaxed);
      self.counters.note_capture(frame.resolution());
      self.check_content(&frame);
      // A camera may deliver 60 fps; the call sends at most its layers' rate.
      if !limiter.is_due(frame.timestamp_us()) {
        continue;
      }
      // Shrink before the filter (background effects) and the encoders: they work per pixel.
      let target = send_resolution.map_or(frame.resolution(), |limit| fit_within(frame.resolution(), limit));
      let frame = if target == frame.resolution() {
        frame
      } else {
        match frame.scaled(target) {
          Ok(scaled) => scaled,
          Err(error) => {
            self.note_no_picture(|| format!("scaling to {}x{}: {error}", target.width, target.height));
            encode_warning.warn(|suppressed| warn!(%error, suppressed, "video frame not scaled"));
            continue;
          },
        }
      };
      let frame = match &mut self.maybe_filter {
        Some(filter) => filter(frame),
        None => frame,
      };
      // A refreshed picture may have run ahead of the capture's clock; time doesn't go back.
      let frame = match &maybe_last {
        Some(last) if frame.timestamp_us() <= last.frame.timestamp_us() => {
          let timestamp_us = last.frame.timestamp_us().saturating_add(1_000);
          frame.with_timestamp_us(timestamp_us)
        },
        _ => frame,
      };
      match self.encode_and_send(&mut simulcast, frame, &mut encode_warning) {
        Ok(last) => maybe_last = Some(last),
        Err(LoopStep::Stop) => return,
        Err(LoopStep::Continue) => {},
      }
    }
  }

  /// The last picture again with a later timestamp, when [`Refresh`] says so (and someone
  /// receives the track).
  fn due_refresh(
    &self,
    simulcast: &SimulcastEncoder,
    maybe_last: Option<&LastPicture>,
    now: Instant,
  ) -> Option<VideoFrame> {
    let last = maybe_last?;
    let is_due = simulcast.has_needed_layers()
      && self
        .refresh
        .is_due(now, last.encoded_at, simulcast.wants_keyframe());
    if !is_due {
      return None;
    }
    let elapsed_us =
      u64::try_from(now.saturating_duration_since(last.encoded_at).as_micros()).unwrap_or(u64::MAX);
    let timestamp_us = last.frame.timestamp_us().saturating_add(elapsed_us);
    Some(last.frame.clone().with_timestamp_us(timestamp_us))
  }

  /// Encodes `frame` for every needed layer and hands the pictures on. The frame comes back as
  /// the last picture; `Err` when it wasn't encoded ([`LoopStep::Continue`]) or nobody takes the
  /// pictures any more ([`LoopStep::Stop`]).
  fn encode_and_send(
    &mut self,
    simulcast: &mut SimulcastEncoder,
    frame: VideoFrame,
    encode_warning: &mut RepeatedWarning,
  ) -> Result<LastPicture, LoopStep> {
    self.follow_source_size(simulcast, frame.resolution());
    if self.maybe_first_picture.is_some() && !simulcast.has_needed_layers() {
      return self.encode_unsent(simulcast, frame, encode_warning);
    }
    let encoded_frames = match simulcast.encode(&frame) {
      Ok(encoded_frames) => encoded_frames,
      Err(error) => {
        self.note_no_picture(|| format!("{}: {error}", simulcast.encoder_names()));
        let kind = &self.kind;
        encode_warning.warn(|suppressed| warn!(%error, suppressed, ?kind, "video frame not encoded"));
        return Err(LoopStep::Continue);
      },
    };
    let has_pictures = !encoded_frames.is_empty();
    let now = Instant::now();
    let is_names_due = self
      .maybe_names_noted_at
      .is_none_or(|noted_at| now.saturating_duration_since(noted_at) >= ENCODER_NAMES_INTERVAL);
    if has_pictures && is_names_due {
      self.counters.note_encoder_names(simulcast.encoder_names());
      self.maybe_names_noted_at = Some(now);
    }
    for encoded_frame in encoded_frames {
      self.counters.encoded.fetch_add(1, Ordering::Relaxed);
      if self.encoded.send((self.kind.clone(), encoded_frame)).is_err() {
        return Err(LoopStep::Stop);
      }
    }
    // After the hand-off: once the start is reported, the first picture is on its way.
    if has_pictures {
      self.report_start(Ok(()));
    } else {
      self.note_no_picture(|| format!("{} gave no picture", simulcast.encoder_names()));
    }
    Ok(LastPicture {
      frame,
      encoded_at: Instant::now(),
    })
  }

  /// A frame while the first picture is pending and no layer is needed (nobody receives the track
  /// yet, or the uplink holds every layer back): encoded on the lowest layer and dropped, so the
  /// start check learns whether the encoders work. Frames held back on purpose are no failing
  /// encoder (#935: such a camera was turned off with "none could be encoded").
  fn encode_unsent(
    &mut self,
    simulcast: &mut SimulcastEncoder,
    frame: VideoFrame,
    encode_warning: &mut RepeatedWarning,
  ) -> Result<LastPicture, LoopStep> {
    match simulcast.encode_unsent(&frame) {
      Ok(true) => {
        self.counters.encoded.fetch_add(1, Ordering::Relaxed);
        debug!(kind = ?self.kind, "no layer is needed yet; the encoders work");
        self.report_start(Ok(()));
      },
      Ok(false) => self.note_no_picture(|| format!("{} gave no picture", simulcast.encoder_names())),
      Err(error) => {
        self.note_no_picture(|| format!("{}: {error}", simulcast.encoder_names()));
        let kind = &self.kind;
        encode_warning.warn(|suppressed| warn!(%error, suppressed, ?kind, "video frame not encoded"));
        return Err(LoopStep::Continue);
      },
    }
    Ok(LastPicture {
      frame,
      encoded_at: Instant::now(),
    })
  }

  /// Remembers why the frame just taken gave no picture while the first picture is pending: the
  /// start check's failure says it.
  fn note_no_picture(&mut self, why: impl FnOnce() -> String) {
    if self.maybe_first_picture.is_some() {
      self.maybe_no_picture = Some(why());
    }
  }

  /// Rebuilds the encoders at the pictures' size once it changed for good (a shared window was
  /// resized), so the picture keeps its shape instead of being stretched into the old size.
  /// Checked on repeated pictures too: a window that stopped changing may deliver no new frame.
  fn follow_source_size(&mut self, simulcast: &mut SimulcastEncoder, size: VideoResolution) {
    let Some(layers) = self.sizer.observe(size, Instant::now()) else {
      return;
    };
    let (encoders, content, kind) = (Arc::clone(&self.encoders), self.content, &self.kind);
    match simulcast.resize(&layers, |layer| encoders(layer, content)) {
      Ok(()) => info!(
        ?kind,
        width = size.width,
        height = size.height,
        "the video source changed size; the encoders follow"
      ),
      Err(error) => warn!(%error, ?kind, "encoders not rebuilt for the new size; the picture is scaled"),
    }
  }

  /// Applies every queued control; [`LoopStep::Stop`] once the sender is gone (the track was
  /// stopped). The only place that takes controls from the queue: a keyframe request taken
  /// anywhere else to look for the stop got lost (issue #683), and with it the picture of a
  /// still screen for whoever asked.
  fn apply_controls(&self, simulcast: &mut SimulcastEncoder) -> LoopStep {
    loop {
      match self.control_rx.try_recv() {
        Ok(SenderControl::Keyframe(layer)) => {
          if let Err(error) = simulcast.request_keyframe(layer) {
            warn!(%error, "keyframe request");
          }
        },
        Ok(SenderControl::NeededLayers(layers)) => simulcast.set_needed_layers(&layers),
        Ok(SenderControl::Bitrates(bitrates)) => {
          let (encoders, content) = (Arc::clone(&self.encoders), self.content);
          match simulcast.set_bitrates(&bitrates, |layer| encoders(layer, content)) {
            Ok(()) => debug!(?bitrates, kind = ?self.kind, "layer bitrates follow the uplink"),
            Err(error) => warn!(%error, ?bitrates, "encoders not rebuilt for new layer bitrates"),
          }
        },
        Err(std_mpsc::TryRecvError::Empty) => return LoopStep::Continue,
        Err(std_mpsc::TryRecvError::Disconnected) => return LoopStep::Stop,
      }
    }
  }

  /// Ends the track when somebody waits for its first picture and it didn't come in time.
  fn check_first_picture(&mut self, now: Instant, started_at: Instant) -> LoopStep {
    let Some(first_picture) = &self.maybe_first_picture else {
      return LoopStep::Continue;
    };
    // Nobody waits (a caller that doesn't check): the track gets as long as it needs.
    if first_picture.is_closed() {
      self.maybe_first_picture = None;
      return LoopStep::Continue;
    }
    if now.saturating_duration_since(started_at) < FIRST_PICTURE_TIMEOUT {
      return LoopStep::Continue;
    }
    let error = first_picture_failure(
      self.source_name(),
      self.counters.progress(),
      self.maybe_no_picture.take(),
    );
    warn!(%error, kind = ?self.kind, "video start failed; the track is stopped");
    self.report_start(Err(error));
    LoopStep::Stop
  }

  /// Ends the track when its capture stopped delivering ([`StallWatch`]), as if the capture had
  /// ended: the call then reports the track as ended (#922).
  fn check_stall(&mut self, now: Instant) -> LoopStep {
    let Some(silent_for) = self.stall.stalled_for(now) else {
      return LoopStep::Continue;
    };
    let source_name = self.source_name();
    warn!(kind = ?self.kind, ?silent_for, "the {source_name} stopped delivering; the track ends");
    self.report_start(Err(VideoStartError::CaptureEnded(source_name)));
    LoopStep::Stop
  }

  /// Counts a black frame and reports when the track turned black or recovered. A few
  /// microseconds per frame (`picture_check` samples a grid).
  fn check_content(&mut self, frame: &VideoFrame) {
    let content = picture_content(frame);
    if content == PictureContent::Black {
      self.counters.black.fetch_add(1, Ordering::Relaxed);
    }
    if let Some(change) = self.black.observe(content, Instant::now()) {
      self.report_black(change);
    }
  }

  fn report_black(&mut self, change: BlackChange) {
    let kind = &self.kind;
    let source_name = self.source_name();
    match change {
      BlackChange::Black(report) => {
        warn!(
          ?kind,
          frames = report.frames,
          hint = black_hint(kind),
          "the {source_name} shows nothing but black"
        );
        self.black.send_notice(report);
      },
      BlackChange::Recovered => info!(?kind, "the {source_name} shows pictures again"),
    }
  }

  fn report_start(&mut self, result: Result<(), VideoStartError>) {
    self.maybe_no_picture = None;
    if let Some(first_picture) = self.maybe_first_picture.take() {
      if result.is_ok() {
        info!(kind = ?self.kind, "the video track sends its first picture");
      }
      // Nobody waits any more; the track goes on either way.
      let _ = first_picture.send(result);
    }
  }

  fn source_name(&self) -> &'static str {
    source_name(&self.kind)
  }
}

/// The largest of `layers`' sizes: what the track sends at most.
fn largest_resolution(layers: &[VideoLayerInfo]) -> Option<VideoResolution> {
  layers
    .iter()
    .map(|layer| VideoResolution {
      width: layer.width,
      height: layer.height,
    })
    .max_by_key(|resolution| u32::from(resolution.width) * u32::from(resolution.height))
}

/// Why no first picture came: nothing captured, or nothing of it encoded (`maybe_no_picture`:
/// what the encoders made of the last frame).
fn first_picture_failure(
  source_name: &'static str,
  progress: SendProgress,
  maybe_no_picture: Option<String>,
) -> VideoStartError {
  if progress.frames_captured == 0 {
    VideoStartError::NoFrames(source_name)
  } else {
    VideoStartError::NotEncoded {
      source_name,
      captured: progress.frames_captured,
      detail: maybe_no_picture.unwrap_or_else(|| "no frame reached the encoders".into()),
    }
  }
}

fn source_name(kind: &VideoTrackKind) -> &'static str {
  match kind {
    VideoTrackKind::Screen => "screen capture",
    _ => "camera",
  }
}

/// What usually makes a track of `kind` black, for the log.
fn black_hint(kind: &VideoTrackKind) -> &'static str {
  match kind {
    VideoTrackKind::Screen if cfg!(target_os = "macos") => {
      "macOS blanks screen captures until the app restarts after the Screen Recording permission \
       was granted; protected video captures black as well"
    },
    VideoTrackKind::Screen => "a minimized window or protected video captures black",
    _ => "the camera may be covered, closed with a privacy shutter or still adjusting to the light",
  }
}

impl BlackWatch {
  fn new(notice: oneshot::Sender<BlackReport>) -> Self {
    Self {
      maybe_black_since: None,
      black_in_a_row: 0,
      is_reported: false,
      maybe_notice: Some(notice),
    }
  }

  /// Takes the content of a picture captured at `now`.
  fn observe(&mut self, content: PictureContent, now: Instant) -> Option<BlackChange> {
    if content != PictureContent::Black {
      let was_black = self.maybe_black_since.take().is_some();
      self.black_in_a_row = 0;
      return (was_black && self.is_reported).then_some(BlackChange::Recovered);
    }
    self.black_in_a_row += 1;
    self.maybe_black_since.get_or_insert(now);
    self.poll(now)
  }

  /// [`BlackChange::Black`] once the pictures have been black for
  /// [`BLACK_PICTURES_REPORT_AFTER`] at `now` (once per track).
  fn poll(&mut self, now: Instant) -> Option<BlackChange> {
    let since = self.maybe_black_since?;
    if self.is_reported || now.saturating_duration_since(since) < BLACK_PICTURES_REPORT_AFTER {
      return None;
    }
    self.is_reported = true;
    Some(BlackChange::Black(BlackReport {
      frames: self.black_in_a_row,
    }))
  }

  fn send_notice(&mut self, report: BlackReport) {
    if let Some(notice) = self.maybe_notice.take() {
      // Nobody listens (a caller that doesn't check): the log says it.
      let _ = notice.send(report);
    }
  }
}

impl Refresh {
  /// Whether the last picture, encoded at `last_encoded_at`, is due again at `now`: a keyframe
  /// request has waited [`KEYFRAME_REFRESH_DELAY`] for a new frame, or the track idled for its
  /// interval.
  fn is_due(&self, now: Instant, last_encoded_at: Instant, wants_keyframe: bool) -> bool {
    let idle = now.saturating_duration_since(last_encoded_at);
    (wants_keyframe && idle >= KEYFRAME_REFRESH_DELAY)
      || self.maybe_idle_interval.is_some_and(|interval| idle >= interval)
  }
}

impl StallWatch {
  fn new(maybe_limit: Option<Duration>) -> Self {
    Self {
      maybe_limit,
      maybe_last_frame_at: None,
    }
  }

  fn on_frame(&mut self, now: Instant) {
    self.maybe_last_frame_at = Some(now);
  }

  /// How long the capture has delivered nothing, once that reached its limit.
  fn stalled_for(&self, now: Instant) -> Option<Duration> {
    let (limit, last_frame_at) = (self.maybe_limit?, self.maybe_last_frame_at?);
    let silent_for = now.saturating_duration_since(last_frame_at);
    (silent_for >= limit).then_some(silent_for)
  }
}

impl FirstPicture {
  /// The track's later notice of showing nothing but black; once (`None` after).
  pub fn take_black_pictures(&mut self) -> Option<BlackPictures> {
    self.maybe_black.take()
  }

  /// Waits for the first encoded picture. Bounded: a capture thread that hangs counts as failed
  /// shortly after [`FIRST_PICTURE_TIMEOUT`].
  pub async fn wait(self) -> Result<(), VideoStartError> {
    let backstop = FIRST_PICTURE_TIMEOUT + Duration::from_secs(5);
    match tokio::time::timeout(backstop, self.started).await {
      Ok(Ok(result)) => result,
      Ok(Err(_)) => Err(VideoStartError::ThreadEnded),
      Err(_) => Err(VideoStartError::NoFrames("video source")),
    }
  }
}

impl BlackPictures {
  /// The report once the track has shown nothing but black long enough; `None` when the track
  /// ended without.
  pub async fn wait(self) -> Option<BlackReport> {
    self.0.await.ok()
  }
}

impl SendCounters {
  fn progress(&self) -> SendProgress {
    SendProgress {
      frames_captured: self.captured.load(Ordering::Relaxed),
      pictures_encoded: self.encoded.load(Ordering::Relaxed),
      frames_black: self.black.load(Ordering::Relaxed),
    }
  }

  fn details(&self) -> SenderDetails {
    let size = self.capture_size.load(Ordering::Relaxed);
    // Unpacked as packed by `note_capture`: each half is one side.
    let maybe_capture = (size != 0).then_some(VideoResolution {
      width: (size >> 16) as u16,
      height: (size & 0xffff) as u16,
    });
    let encoder_names = self
      .encoder_names
      .lock()
      .map(|names| names.clone())
      .unwrap_or_default();
    SenderDetails {
      maybe_capture,
      encoder_names,
    }
  }

  fn note_capture(&self, size: VideoResolution) {
    let packed = u32::from(size.width) << 16 | u32::from(size.height);
    self.capture_size.store(packed, Ordering::Relaxed);
  }

  fn note_encoder_names(&self, names: String) {
    if let Ok(mut noted) = self.encoder_names.lock() {
      *noted = names;
    }
  }
}

impl RepeatedWarning {
  /// Calls `log` with the number of occurrences not logged since the last time, when due.
  pub(crate) fn warn(&mut self, log: impl FnOnce(u64)) {
    self.warn_at(Instant::now(), log);
  }

  fn warn_at(&mut self, now: Instant, log: impl FnOnce(u64)) {
    let is_due = self
      .maybe_last
      .is_none_or(|last| now.saturating_duration_since(last) >= REPEATED_WARNING_INTERVAL);
    if is_due {
      log(self.suppressed);
      self.suppressed = 0;
      self.maybe_last = Some(now);
    } else {
      self.suppressed += 1;
      debug!(suppressed = self.suppressed, "repeated video failure");
    }
  }
}

impl VideoSender {
  /// Stops capturing and encoding, and returns what the track did. Waits for the threads (up to
  /// [`SENDER_STOP_WAIT`]); the call's task uses [`VideoSender::stop_in_background`].
  pub fn finish(mut self) -> SendProgress {
    self.shut_down();
    self.counters.progress()
  }

  /// Stops capturing and encoding without waiting: the encoder thread is told to stop at once,
  /// and waited for (with it the capture, up to [`SENDER_STOP_WAIT`]) on a blocking task. A
  /// camera may take that long to let go, and the call's audio must not wait for it (#922).
  /// Needs a Tokio runtime.
  pub fn stop_in_background(mut self) -> StoppingSender {
    self.signal_stop();
    let kind = self.kind.clone();
    let counters = Arc::clone(&self.counters);
    let task = tokio::task::spawn_blocking(move || self.shut_down());
    StoppingSender { kind, counters, task }
  }

  /// Tells the encoder thread to stop: it sees its controls closed.
  fn signal_stop(&mut self) {
    let (closed, _) = std_mpsc::channel();
    drop(std::mem::replace(&mut self.controls, closed));
  }

  /// Stops the encoder thread and waits for it (and with it the capture), at most
  /// [`SENDER_STOP_WAIT`]: a capture that hangs in its driver is left behind.
  fn shut_down(&mut self) {
    self.signal_stop();
    if let Some(thread) = self.thread.take()
      && thread.join_within(SENDER_STOP_WAIT) == Joined::Detached
    {
      warn!(kind = ?self.kind, "the video capture hangs; the track is left behind");
    }
  }
}

impl StoppingSender {
  /// What the track did; final once [`StoppingSender::is_stopped`].
  pub fn progress(&self) -> SendProgress {
    self.counters.progress()
  }

  /// The threads ended, or were left behind after [`SENDER_STOP_WAIT`].
  pub fn is_stopped(&self) -> bool {
    self.task.is_finished()
  }

  /// Waits until [`StoppingSender::is_stopped`] and returns what the track did.
  pub async fn stopped(self) -> SendProgress {
    // A panic while joining leaves the counters as they are.
    let _ = self.task.await;
    self.counters.progress()
  }
}

impl Drop for VideoSender {
  fn drop(&mut self) {
    self.shut_down();
  }
}

impl VideoReceiver {
  /// Decodes frames pushed with [`VideoReceiver::push`] into `sink`; a frame that doesn't decode
  /// calls `keyframe_needed` and the decoder waits for the next keyframe.
  pub fn start(mut sink: FrameSink, mut keyframe_needed: KeyframeNeeded) -> Result<Self, VideoCodecError> {
    let (frames, frame_rx) = std_mpsc::channel::<ReceivedVideoFrame>();
    let thread = std::thread::Builder::new()
      .name("video-decoder".into())
      .spawn(move || {
        let mut decoder = match OpenH264Decoder::new() {
          Ok(decoder) => decoder,
          Err(error) => {
            warn!(%error, "no video decoder");
            return;
          },
        };
        let mut is_waiting_for_keyframe = true;
        for frame in frame_rx {
          if is_waiting_for_keyframe && !frame.is_keyframe {
            continue;
          }
          is_waiting_for_keyframe = false;
          // RTP video clock (90 kHz) to microseconds.
          let timestamp_us = u64::from(frame.timestamp.0) * 1_000_000 / 90_000;
          match decoder.decode(&frame.data, timestamp_us) {
            Ok(Some(decoded)) => sink(decoded),
            Ok(None) => {},
            Err(error) => {
              warn!(%error, "video frame not decoded; waiting for a keyframe");
              is_waiting_for_keyframe = true;
              keyframe_needed();
            },
          }
        }
      })
      .map_err(|error| VideoCodecError::Codec(error.to_string()))?;
    Ok(Self {
      frames,
      thread: Some(thread),
    })
  }

  pub fn push(&self, frame: ReceivedVideoFrame) {
    // The decoder thread only ends when this receiver is dropped.
    let _ = self.frames.send(frame);
  }
}

impl Drop for VideoReceiver {
  fn drop(&mut self) {
    let (closed, _) = std_mpsc::channel();
    drop(std::mem::replace(&mut self.frames, closed));
    if let Some(thread) = self.thread.take()
      && thread.join().is_err()
    {
      warn!("video decoder thread panicked");
    }
  }
}

#[cfg(test)]
mod tests {
  #[test]
  fn the_limiter_drops_a_60_fps_camera_to_30_and_keeps_a_jittery_30() {
    let mut limiter = FrameLimiter::new(Some(30));
    let sixty: Vec<bool> = (0..6).map(|i| limiter.is_due(i * 16_667)).collect();
    assert_eq!(sixty, [true, false, true, false, true, false]);
    let mut limiter = FrameLimiter::new(Some(30));
    let jittery = [0, 31_000, 66_000, 98_000].map(|t| limiter.is_due(t));
    assert_eq!(jittery, [true; 4]);
    assert!(FrameLimiter::new(None).is_due(5));
  }

  use std::sync::Mutex;

  use teamcraft_proto::{DeviceId, Id, MediaStreamId, Snowflake, VideoCodec};

  use super::*;
  use crate::video::{VideoResolution, simulcast_layers};
  use crate::video_capture::{CaptureFormat, ManualCapture, test_pattern};

  const VGA: VideoResolution = VideoResolution {
    width: 640,
    height: 360,
  };

  #[tokio::test]
  #[cfg_attr(
    not(feature = "openh264-source"),
    ignore = "needs OpenH264: --features openh264-source"
  )]
  async fn captured_frames_come_out_encoded_and_decode_again() {
    let capture = ManualCapture::new(CaptureFormat {
      resolution: VGA,
      framerate: 30,
    });
    let (encoded, mut encoded_rx) = mpsc::unbounded_channel();
    let pushed = Arc::new(Mutex::new(None));
    let config = VideoSendConfig {
      maybe_layer_plan: None,
      kind: VideoTrackKind::Camera,
      capture: Box::new(SharedCapture(capture, Arc::clone(&pushed))),
      layers: simulcast_layers(VGA, 30, 800_000, 2),
      encoders: software_encoders(),
      // Proves the filter runs before encoding: the whole picture turns grey.
      maybe_filter: Some(Box::new(|frame| {
        let resolution = frame.resolution();
        VideoFrame::new(resolution, frame.timestamp_us(), vec![128; frame.data().len()]).unwrap()
      })),
      maybe_idle_refresh: None,
    };
    let (sender, _first_picture) = VideoSender::start(config, encoded).unwrap();
    let frames = pushed.lock().unwrap().clone().unwrap();
    let decoded = Arc::new(Mutex::new(Vec::new()));
    let sink_frames = Arc::clone(&decoded);
    let receiver = VideoReceiver::start(
      Box::new(move |frame| sink_frames.lock().unwrap().push(frame)),
      Box::new(|| {}),
    )
    .unwrap();

    let mut received = Vec::new();
    for index in 0..10u64 {
      // Wait for room: the queue holds two frames and the encoder takes its time.
      assert!(frames.send(test_pattern(VGA, index, index * 33_333).unwrap()));
      while let Ok(Some((kind, frame))) =
        tokio::time::timeout(Duration::from_millis(200), encoded_rx.recv()).await
      {
        assert_eq!(kind, VideoTrackKind::Camera);
        received.push(frame.layer.value());
        if frame.layer.value() == 1 {
          receiver.push(received_frame(frame));
        }
        if received.len() >= 2 * (index as usize + 1) {
          break;
        }
      }
    }
    sender.request_keyframe(0);
    drop(sender);
    drop(receiver);
    assert!(received.contains(&0) && received.contains(&1), "{received:?}");
    let decoded = decoded.lock().unwrap();
    assert!(!decoded.is_empty());
    let (y, _, _) = decoded[0].planes();
    let average = y.iter().map(|value| u64::from(*value)).sum::<u64>() / y.len() as u64;
    assert!((120..=136).contains(&average), "filtered grey, got {average}");
  }

  #[tokio::test]
  #[cfg_attr(
    not(feature = "openh264-source"),
    ignore = "needs OpenH264: --features openh264-source"
  )]
  async fn a_window_of_another_shape_is_sent_in_its_shape_not_stretched() {
    let capture = ManualCapture::new(CaptureFormat {
      resolution: VGA,
      framerate: 30,
    });
    let (encoded, mut encoded_rx) = mpsc::unbounded_channel();
    let pushed = Arc::new(Mutex::new(None));
    let config = VideoSendConfig {
      maybe_layer_plan: None,
      kind: VideoTrackKind::Screen,
      capture: Box::new(SharedCapture(capture, Arc::clone(&pushed))),
      // Planned for a landscape picture...
      layers: simulcast_layers(VGA, 30, 800_000, 1),
      encoders: software_encoders(),
      maybe_filter: None,
      maybe_idle_refresh: None,
    };
    let (sender, _first_picture) = VideoSender::start(config, encoded).unwrap();
    let frames = pushed.lock().unwrap().clone().unwrap();
    // ...but the shared window is portrait.
    let portrait = VideoResolution {
      width: 360,
      height: 640,
    };
    let mut decoder = OpenH264Decoder::new().unwrap();
    let mut decoded_sizes = Vec::new();
    for index in 0..5u64 {
      assert!(frames.send(test_pattern(portrait, index, index * 33_333).unwrap()));
      let Ok(Some((_, frame))) = tokio::time::timeout(Duration::from_secs(5), encoded_rx.recv()).await else {
        panic!("frame {index} not encoded");
      };
      if let Some(picture) = decoder.decode(&frame.data, index * 33_333).unwrap() {
        decoded_sizes.push(picture.resolution());
      }
    }
    drop(sender);
    // Within the 640×360 limit, in the window's shape.
    let expected = VideoResolution {
      width: 202,
      height: 360,
    };
    assert!(!decoded_sizes.is_empty());
    assert!(
      decoded_sizes.iter().all(|size| *size == expected),
      "{decoded_sizes:?}"
    );
  }

  #[test]
  fn a_sender_whose_capture_or_encoder_ends_reports_it() {
    let (encoded, _encoded_rx) = mpsc::unbounded_channel();
    let config = VideoSendConfig {
      maybe_layer_plan: None,
      kind: VideoTrackKind::Screen,
      capture: Box::new(ManualCapture::new(CaptureFormat {
        resolution: VGA,
        framerate: 30,
      })),
      layers: simulcast_layers(VGA, 30, 800_000, 1),
      // No encoder: the thread ends at once, as when the system ends the capture.
      encoders: Arc::new(|_, _| Err(VideoCodecError::Codec("none".into()))),
      maybe_filter: None,
      maybe_idle_refresh: None,
    };
    let (sender, _first_picture) = VideoSender::start(config, encoded).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !sender.has_ended() && std::time::Instant::now() < deadline {
      std::thread::sleep(Duration::from_millis(10));
    }
    assert!(sender.has_ended());
  }

  /// #967: a camera asked for 1080p that delivers 720p publishes the layers of 720p, and the
  /// frame they were planned for is encoded like every other.
  #[tokio::test]
  async fn layers_are_planned_for_the_first_frame_the_camera_delivers() {
    let hd = VideoResolution {
      width: 1280,
      height: 720,
    };
    let limit = crate::video::camera_limit(None);
    let (encoded, mut encoded_rx) = mpsc::unbounded_channel();
    let capture = Delivering {
      format: CaptureFormat {
        resolution: limit,
        framerate: 30,
      },
      delivers: hd,
      maybe_frames: None,
    };
    let config = VideoSendConfig {
      kind: VideoTrackKind::Camera,
      capture: Box::new(capture),
      layers: crate::video::camera_layers(limit, 30, limit),
      maybe_layer_plan: Some(Box::new(move |source| {
        crate::video::camera_layers(source, 30, limit)
      })),
      encoders: first_picture::fake_encoders(),
      maybe_filter: None,
      maybe_idle_refresh: None,
    };
    let (sender, _first_picture) = VideoSender::start(config, encoded).unwrap();
    assert_eq!(sender.layers, crate::video::camera_layers(hd, 30, limit));
    assert_eq!(sender.layers.last().map(|top| top.height), Some(720));
    let (_, first) = tokio::time::timeout(Duration::from_secs(5), encoded_rx.recv())
      .await
      .unwrap()
      .unwrap();
    assert_eq!(first.timestamp, EncodedVideoFrame::rtp_timestamp(0));
  }

  /// #978: a 1080p camera sent to an SFU that takes three layers is planned with its top three,
  /// 1080p, 720p and 360p, also when the layers are planned from the first frame.
  #[tokio::test]
  async fn a_layer_limit_keeps_the_top_layers_of_the_plan() {
    let limit = crate::video::camera_limit(None);
    let (encoded, _encoded_rx) = mpsc::unbounded_channel();
    let capture = Delivering {
      format: CaptureFormat {
        resolution: limit,
        framerate: 30,
      },
      delivers: limit,
      maybe_frames: None,
    };
    let config = VideoSendConfig {
      kind: VideoTrackKind::Camera,
      capture: Box::new(capture),
      layers: crate::video::camera_layers(limit, 30, limit),
      maybe_layer_plan: Some(Box::new(move |source| {
        crate::video::camera_layers(source, 30, limit)
      })),
      encoders: first_picture::fake_encoders(),
      maybe_filter: None,
      maybe_idle_refresh: None,
    }
    .within_layer_limit(3);
    assert_eq!(config.layers.len(), 3);
    let (sender, _first_picture) = VideoSender::start(config, encoded).unwrap();
    let sides: Vec<(u8, u16)> = sender
      .layers
      .iter()
      .map(|info| (info.layer, info.height))
      .collect();
    assert_eq!(sides, vec![(0, 360), (1, 720), (2, 1080)]);
  }

  /// Delivers one frame of `delivers` as soon as it starts, whatever it was asked for.
  struct Delivering {
    format: CaptureFormat,
    delivers: VideoResolution,
    maybe_frames: Option<crate::video_capture::FrameSender>,
  }

  impl VideoCapture for Delivering {
    fn format(&self) -> CaptureFormat {
      self.format
    }

    fn start(&mut self, frames: crate::video_capture::FrameSender) -> Result<(), CaptureError> {
      assert!(frames.send(test_pattern(self.delivers, 0, 0)?));
      self.maybe_frames = Some(frames);
      Ok(())
    }

    fn stop(&mut self) {
      self.maybe_frames = None;
    }
  }

  mod first_picture {
    use super::*;

    #[tokio::test]
    async fn the_first_encoded_picture_is_reported_and_counted() {
      let pushed = Arc::new(Mutex::new(None));
      let (sender, first_picture, _encoded_rx) = start_sender(
        Box::new(SharedCapture(manual_capture(), Arc::clone(&pushed))),
        fake_encoders(),
      );
      let frames = pushed.lock().unwrap().clone().unwrap();
      assert!(frames.send(test_pattern(VGA, 0, 0).unwrap()));
      assert_eq!(first_picture.wait().await, Ok(()));
      assert_eq!(
        sender.finish(),
        SendProgress {
          frames_captured: 1,
          pictures_encoded: 1,
          frames_black: 0,
        }
      );
    }

    #[tokio::test]
    async fn a_keyframe_request_made_while_no_frame_comes_is_not_lost() {
      let pushed = Arc::new(Mutex::new(None));
      let (sender, _first_picture, mut encoded_rx) = start_sender(
        Box::new(SharedCapture(manual_capture(), Arc::clone(&pushed))),
        fake_encoders(),
      );
      let frames = pushed.lock().unwrap().clone().unwrap();
      let mut is_keyframe = async |index: u64| {
        assert!(frames.send(test_pattern(VGA, index, index * 33_333).unwrap()));
        encoded_rx.recv().await.unwrap().1.is_keyframe
      };
      assert!(is_keyframe(0).await, "the first picture");
      assert!(!is_keyframe(1).await);
      // A still screen: no frame for a while, during which the encoder thread polls its queues.
      // It used to take the request off the queue there, looking for a stop (issue #683). Two
      // polls that ended without a frame since the request: at least one ran while it waited.
      sender.request_keyframe(0);
      let idle_polls = || sender.counters.idle_polls.load(Ordering::Relaxed);
      let since = idle_polls();
      let deadline = Instant::now() + Duration::from_secs(10);
      while idle_polls() < since + 2 {
        assert!(Instant::now() < deadline, "the encoder thread never idled");
        tokio::time::sleep(Duration::from_millis(1)).await;
      }
      assert!(is_keyframe(2).await, "the request waited for the next frame");
    }

    #[tokio::test]
    async fn a_track_without_an_encoder_reports_it() {
      let (_sender, first_picture, _encoded_rx) = start_sender(
        Box::new(manual_capture()),
        Arc::new(|_, _| Err(VideoCodecError::Codec("no OpenH264".into()))),
      );
      let result = first_picture.wait().await;
      assert!(matches!(result, Err(VideoStartError::NoEncoder(_))), "{result:?}");
    }

    #[tokio::test]
    async fn a_capture_that_ends_before_its_first_picture_reports_it() {
      let (_sender, first_picture, _encoded_rx) =
        start_sender(Box::new(EndingCapture(manual_capture())), fake_encoders());
      let result = first_picture.wait().await;
      assert_eq!(result, Err(VideoStartError::CaptureEnded("camera")));
    }

    #[test]
    fn a_track_without_a_picture_in_time_stops_while_somebody_waits() {
      let (mut encoder_loop, mut first_picture) = idle_loop();
      let start = Instant::now();
      let almost = start + FIRST_PICTURE_TIMEOUT - Duration::from_millis(1);
      assert_eq!(
        encoder_loop.check_first_picture(almost, start),
        LoopStep::Continue
      );
      assert!(first_picture.try_recv().is_err(), "nothing decided yet");
      let overdue = start + FIRST_PICTURE_TIMEOUT;
      assert_eq!(encoder_loop.check_first_picture(overdue, start), LoopStep::Stop);
      assert_eq!(
        first_picture.try_recv(),
        Ok(Err(VideoStartError::NoFrames("camera")))
      );
    }

    #[test]
    fn a_track_nobody_waits_for_gets_as_long_as_it_needs() {
      let (mut encoder_loop, first_picture) = idle_loop();
      drop(first_picture);
      let start = Instant::now();
      let late = start + FIRST_PICTURE_TIMEOUT * 3;
      assert_eq!(encoder_loop.check_first_picture(late, start), LoopStep::Continue);
    }

    /// Issue #935: a camera nobody receives yet (or whose layers the uplink holds back) proves
    /// its encoder with a picture that isn't sent, instead of failing for frames held back.
    #[tokio::test]
    async fn a_track_whose_layers_are_all_held_back_still_proves_its_encoder() {
      let pushed = Arc::new(Mutex::new(None));
      let (sender, first_picture, mut encoded_rx) = start_sender(
        Box::new(SharedCapture(manual_capture(), Arc::clone(&pushed))),
        fake_encoders(),
      );
      sender.set_needed_layers(Vec::new());
      let frames = pushed.lock().unwrap().clone().unwrap();
      assert!(frames.send(test_pattern(VGA, 0, 0).unwrap()));
      assert_eq!(first_picture.wait().await, Ok(()));
      assert!(
        encoded_rx.try_recv().is_err(),
        "nothing is sent while no layer is needed"
      );
      // Once a receiver needs the layer, it starts with a keyframe.
      sender.set_needed_layers(vec![0]);
      assert!(frames.send(test_pattern(VGA, 1, 33_333).unwrap()));
      let (_, picture) = encoded_rx.recv().await.unwrap();
      assert!(picture.is_keyframe);
      assert_eq!(sender.finish().pictures_encoded, 2);
    }

    /// Stats for nerds (#938): the track tells the size the capture delivers and the encoders at
    /// work once a picture came.
    #[tokio::test]
    async fn a_sending_track_tells_its_capture_size_and_encoders() {
      let pushed = Arc::new(Mutex::new(None));
      let (sender, first_picture, mut encoded_rx) = start_sender(
        Box::new(SharedCapture(manual_capture(), Arc::clone(&pushed))),
        fake_encoders(),
      );
      assert_eq!(sender.details(), SenderDetails::default());
      let frames = pushed.lock().unwrap().clone().unwrap();
      assert!(frames.send(test_pattern(VGA, 0, 0).unwrap()));
      assert_eq!(first_picture.wait().await, Ok(()));
      encoded_rx.recv().await.unwrap();
      assert_eq!(
        sender.details(),
        SenderDetails {
          maybe_capture: Some(VGA),
          encoder_names: "fake".to_owned(),
        }
      );
      sender.finish();
    }

    /// Issue #935: the failure the user sees names the encoders and their error.
    #[test]
    fn a_failing_encoder_is_named_with_its_error_when_the_check_gives_up() {
      let (mut encoder_loop, mut first_picture) = idle_loop();
      let layers = encoder_loop.layers.clone();
      let mut simulcast = SimulcastEncoder::new(&layers, |_| {
        Ok(Box::new(FakeEncoder::Failing) as Box<dyn VideoEncoder>)
      })
      .unwrap();
      let mut warning = RepeatedWarning::default();
      encoder_loop.counters.captured.fetch_add(1, Ordering::Relaxed);
      let frame = test_pattern(VGA, 0, 0).unwrap();
      assert!(
        encoder_loop
          .encode_and_send(&mut simulcast, frame, &mut warning)
          .is_err()
      );
      let start = Instant::now();
      let overdue = start + FIRST_PICTURE_TIMEOUT;
      assert_eq!(encoder_loop.check_first_picture(overdue, start), LoopStep::Stop);
      let error = first_picture.try_recv().unwrap().unwrap_err();
      assert_eq!(
        error.to_string(),
        "the camera delivered 1 pictures, but none could be encoded: fake: video codec: device lost"
      );
    }

    #[test]
    fn the_failure_says_whether_anything_was_captured() {
      assert_eq!(
        first_picture_failure("camera", SendProgress::default(), None),
        VideoStartError::NoFrames("camera")
      );
      let captured = SendProgress {
        frames_captured: 12,
        ..SendProgress::default()
      };
      assert_eq!(
        first_picture_failure(
          "screen capture",
          captured,
          Some("OpenH264 gave no picture".into())
        ),
        VideoStartError::NotEncoded {
          source_name: "screen capture",
          captured: 12,
          detail: "OpenH264 gave no picture".into(),
        }
      );
    }

    #[test]
    fn repeated_failures_are_logged_once_per_interval_with_a_count() {
      let mut warning = RepeatedWarning::default();
      let start = Instant::now();
      let mut logged = Vec::new();
      for offset_ms in [0, 10, 20, 30, 10_000, 10_010] {
        warning.warn_at(start + Duration::from_millis(offset_ms), |suppressed| {
          logged.push((offset_ms, suppressed));
        });
      }
      assert_eq!(logged, vec![(0, 0), (10_000, 3)]);
    }

    /// A capture that hands back its queue at once, as one whose device disappears.
    struct EndingCapture(ManualCapture);

    impl VideoCapture for EndingCapture {
      fn format(&self) -> CaptureFormat {
        self.0.format()
      }

      fn start(&mut self, frames: crate::video_capture::FrameSender) -> Result<(), CaptureError> {
        drop(frames);
        Ok(())
      }

      fn stop(&mut self) {}
    }

    /// One picture per frame and layer, or an error for every frame.
    enum FakeEncoder {
      Working,
      Failing,
    }

    impl VideoEncoder for FakeEncoder {
      fn codec(&self) -> VideoCodec {
        VideoCodec::H264
      }

      fn name(&self) -> &'static str {
        "fake"
      }

      fn encode(
        &mut self,
        _frame: &VideoFrame,
        is_keyframe_forced: bool,
      ) -> Result<Option<crate::video_codec::EncodedPicture>, VideoCodecError> {
        match self {
          Self::Working => Ok(Some(crate::video_codec::EncodedPicture {
            is_keyframe: is_keyframe_forced,
            data: vec![0, 0, 0, 1, 0x65],
          })),
          Self::Failing => Err(VideoCodecError::Codec("device lost".into())),
        }
      }
    }

    pub(super) fn fake_encoders() -> EncoderFactory {
      Arc::new(|_, _| Ok(Box::new(FakeEncoder::Working) as Box<dyn VideoEncoder>))
    }

    pub(super) fn manual_capture() -> ManualCapture {
      ManualCapture::new(CaptureFormat {
        resolution: VGA,
        framerate: 30,
      })
    }

    pub(super) fn start_sender(
      capture: Box<dyn VideoCapture>,
      encoders: EncoderFactory,
    ) -> (
      VideoSender,
      FirstPicture,
      mpsc::UnboundedReceiver<(VideoTrackKind, EncodedVideoFrame)>,
    ) {
      let (encoded, encoded_rx) = mpsc::unbounded_channel();
      let config = VideoSendConfig {
        maybe_layer_plan: None,
        kind: VideoTrackKind::Camera,
        capture,
        layers: simulcast_layers(VGA, 30, 800_000, 1),
        encoders,
        maybe_filter: None,
        maybe_idle_refresh: None,
      };
      let (sender, first_picture) = VideoSender::start(config, encoded).unwrap();
      (sender, first_picture, encoded_rx)
    }

    /// An encoder thread's state without the thread, for its decisions.
    fn idle_loop() -> (EncoderLoop, oneshot::Receiver<Result<(), VideoStartError>>) {
      let (_frames, frame_rx) = frame_queue();
      let (_controls, control_rx) = std_mpsc::channel();
      let (encoded, _encoded_rx) = mpsc::unbounded_channel();
      let (first_picture, first_picture_rx) = oneshot::channel();
      let layers = simulcast_layers(VGA, 30, 800_000, 1);
      let encoder_loop = EncoderLoop {
        kind: VideoTrackKind::Camera,
        sizer: LayerSizer::new(layers.clone(), VGA),
        layers,
        content: VideoContent::Camera,
        encoders: fake_encoders(),
        maybe_filter: None,
        refresh: Refresh {
          maybe_idle_interval: None,
        },
        frame_rx,
        maybe_pending_frame: None,
        control_rx,
        encoded,
        counters: Arc::default(),
        maybe_first_picture: Some(first_picture),
        maybe_no_picture: None,
        maybe_names_noted_at: None,
        black: BlackWatch::new(oneshot::channel().0),
        stall: StallWatch::new(None),
      };
      (encoder_loop, first_picture_rx)
    }
  }

  /// The last picture again when the capture delivers nothing (issue #683).
  mod refresh {
    use super::first_picture::{fake_encoders, manual_capture};
    use super::*;

    const RECEIVE: Duration = Duration::from_secs(5);

    #[test]
    fn a_waiting_keyframe_or_an_idle_screen_brings_the_last_picture_again() {
      let start = Instant::now();
      let camera = Refresh {
        maybe_idle_interval: None,
      };
      let screen = Refresh {
        maybe_idle_interval: Some(SCREEN_IDLE_REFRESH),
      };
      let soon = start + Duration::from_millis(100);
      assert!(!camera.is_due(soon, start, true), "a new frame may still come");
      assert!(camera.is_due(start + KEYFRAME_REFRESH_DELAY, start, true));
      assert!(
        !camera.is_due(start + Duration::from_secs(60), start, false),
        "cameras deliver all the time"
      );
      assert!(!screen.is_due(start + KEYFRAME_REFRESH_DELAY, start, false));
      assert!(screen.is_due(start + SCREEN_IDLE_REFRESH, start, false));
    }

    #[tokio::test]
    async fn a_keyframe_request_for_a_still_screen_is_answered_with_its_last_picture() {
      let (sender, frames, mut encoded_rx) = still_screen(None);
      assert!(frames.send(test_pattern(VGA, 0, 0).unwrap()));
      let first = next(&mut encoded_rx).await;
      // The screen doesn't change: no frame comes. A viewer asks for a keyframe.
      sender.request_keyframe(0);
      let again = next(&mut encoded_rx).await;
      assert!(again.is_keyframe);
      assert!(again.timestamp.0 > first.timestamp.0, "a later picture");
    }

    #[tokio::test]
    async fn an_idle_screen_repeats_its_picture() {
      let (_sender, frames, mut encoded_rx) = still_screen(Some(Duration::from_millis(30)));
      assert!(frames.send(test_pattern(VGA, 0, 0).unwrap()));
      let mut timestamps = Vec::new();
      for _ in 0..3 {
        timestamps.push(next(&mut encoded_rx).await.timestamp.0);
      }
      assert!(
        timestamps.is_sorted() && timestamps[0] < timestamps[2],
        "{timestamps:?}"
      );
    }

    /// A screen sender whose frames the test pushes, with fake encoders.
    fn still_screen(
      maybe_idle_refresh: Option<Duration>,
    ) -> (
      VideoSender,
      crate::video_capture::FrameSender,
      mpsc::UnboundedReceiver<(VideoTrackKind, EncodedVideoFrame)>,
    ) {
      let pushed = Arc::new(Mutex::new(None));
      let (encoded, encoded_rx) = mpsc::unbounded_channel();
      let config = VideoSendConfig {
        maybe_layer_plan: None,
        kind: VideoTrackKind::Screen,
        capture: Box::new(SharedCapture(manual_capture(), Arc::clone(&pushed))),
        layers: simulcast_layers(VGA, 30, 800_000, 1),
        encoders: fake_encoders(),
        maybe_filter: None,
        maybe_idle_refresh,
      };
      let (sender, _first_picture) = VideoSender::start(config, encoded).unwrap();
      let frames = pushed.lock().unwrap().clone().unwrap();
      (sender, frames, encoded_rx)
    }

    async fn next(
      encoded_rx: &mut mpsc::UnboundedReceiver<(VideoTrackKind, EncodedVideoFrame)>,
    ) -> EncodedVideoFrame {
      tokio::time::timeout(RECEIVE, encoded_rx.recv())
        .await
        .expect("a picture in time")
        .unwrap()
        .1
    }
  }

  /// A capture that stops delivering without ending, as nokhwa's AVFoundation camera when it is
  /// unplugged (#922), ends its track after its stall limit.
  mod stall {
    use super::first_picture::{fake_encoders, manual_capture, start_sender};
    use super::*;

    const LIMIT: Duration = Duration::from_millis(200);

    #[test]
    fn silence_counts_from_the_last_frame_and_only_with_a_limit() {
      let start = Instant::now();
      let mut watch = StallWatch::new(Some(LIMIT));
      assert_eq!(watch.stalled_for(start + LIMIT * 10), None, "no frame yet");
      watch.on_frame(start);
      assert_eq!(watch.stalled_for(start + LIMIT - Duration::from_millis(1)), None);
      assert_eq!(watch.stalled_for(start + LIMIT), Some(LIMIT));
      watch.on_frame(start + LIMIT);
      assert_eq!(
        watch.stalled_for(start + LIMIT * 3 / 2),
        None,
        "a frame came again"
      );

      let mut unlimited = StallWatch::new(None);
      unlimited.on_frame(start);
      assert_eq!(unlimited.stalled_for(start + LIMIT * 100), None);
    }

    #[tokio::test]
    async fn a_camera_that_stops_delivering_ends_its_track() {
      let pushed = Arc::new(Mutex::new(None));
      let (sender, first_picture, _encoded_rx) = start_sender(
        Box::new(Stalling(SharedCapture(manual_capture(), Arc::clone(&pushed)))),
        fake_encoders(),
      );
      // The capture keeps its queue open: only the silence can tell that it went away.
      let frames = pushed.lock().unwrap().clone().unwrap();
      assert!(frames.send(test_pattern(VGA, 0, 0).unwrap()));
      assert_eq!(first_picture.wait().await, Ok(()));
      let deadline = Instant::now() + LIMIT * 50;
      while !sender.has_ended() {
        assert!(Instant::now() < deadline, "the stalled track never ended");
        tokio::time::sleep(Duration::from_millis(10)).await;
      }
      assert_eq!(sender.progress().pictures_encoded, 1);
      drop(frames);
    }

    #[tokio::test]
    async fn a_capture_without_a_limit_may_pause() {
      let pushed = Arc::new(Mutex::new(None));
      let (sender, first_picture, _encoded_rx) = start_sender(
        Box::new(SharedCapture(manual_capture(), Arc::clone(&pushed))),
        fake_encoders(),
      );
      let frames = pushed.lock().unwrap().clone().unwrap();
      assert!(frames.send(test_pattern(VGA, 0, 0).unwrap()));
      assert_eq!(first_picture.wait().await, Ok(()));
      // Idle polls for longer than a limit would have been: the thread waited and went on.
      let polls_past_limit = u64::try_from(LIMIT.as_millis() / CONTROL_POLL.as_millis()).unwrap() + 2;
      let idle_polls = || sender.counters.idle_polls.load(Ordering::Relaxed);
      let since = idle_polls();
      let deadline = Instant::now() + Duration::from_secs(10);
      while idle_polls() < since + polls_past_limit {
        assert!(Instant::now() < deadline, "the encoder thread never idled");
        tokio::time::sleep(Duration::from_millis(1)).await;
      }
      assert!(!sender.has_ended());
    }

    #[tokio::test]
    async fn a_track_stopped_in_the_background_reports_what_it_did() {
      let pushed = Arc::new(Mutex::new(None));
      let (sender, first_picture, _encoded_rx) = start_sender(
        Box::new(SharedCapture(manual_capture(), Arc::clone(&pushed))),
        fake_encoders(),
      );
      let frames = pushed.lock().unwrap().clone().unwrap();
      assert!(frames.send(test_pattern(VGA, 0, 0).unwrap()));
      assert_eq!(first_picture.wait().await, Ok(()));
      let stopping = sender.stop_in_background();
      assert_eq!(stopping.kind, VideoTrackKind::Camera);
      let progress = tokio::time::timeout(Duration::from_secs(10), stopping.stopped())
        .await
        .expect("the track stopped");
      assert_eq!(progress.pictures_encoded, 1);
    }

    /// A capture with a short stall limit.
    struct Stalling(SharedCapture);

    impl VideoCapture for Stalling {
      fn format(&self) -> CaptureFormat {
        self.0.format()
      }

      fn start(&mut self, frames: crate::video_capture::FrameSender) -> Result<(), CaptureError> {
        self.0.start(frames)
      }

      fn stop(&mut self) {
        self.0.stop();
      }

      fn stall_limit(&self) -> Option<Duration> {
        Some(LIMIT)
      }
    }
  }

  /// A track that shows nothing but black is reported, once (issue #683).
  mod black_pictures {
    use super::first_picture::{fake_encoders, manual_capture};
    use super::*;

    #[test]
    fn black_for_long_enough_is_reported_once_and_content_ends_it() {
      let mut watch = BlackWatch::new(oneshot::channel().0);
      let start = Instant::now();
      assert_eq!(watch.observe(PictureContent::Black, start), None);
      let almost = start + BLACK_PICTURES_REPORT_AFTER - Duration::from_millis(1);
      assert_eq!(watch.observe(PictureContent::Black, almost), None);
      let due = start + BLACK_PICTURES_REPORT_AFTER;
      assert_eq!(
        watch.poll(due),
        Some(BlackChange::Black(BlackReport { frames: 2 }))
      );
      let later = due + Duration::from_secs(10);
      assert_eq!(watch.observe(PictureContent::Black, later), None, "reported once");
      assert_eq!(
        watch.observe(PictureContent::Detailed, later),
        Some(BlackChange::Recovered)
      );
      assert_eq!(watch.observe(PictureContent::Detailed, later), None);
    }

    #[test]
    fn a_dark_start_is_no_black_track_and_black_later_counts_from_then() {
      let mut watch = BlackWatch::new(oneshot::channel().0);
      let start = Instant::now();
      // A camera adjusting to the light: dark for a second, then a picture.
      assert_eq!(watch.observe(PictureContent::Black, start), None);
      let second = start + Duration::from_secs(1);
      assert_eq!(watch.observe(PictureContent::Detailed, second), None);
      // Covered later: the time counts from the first black picture since.
      let covered = start + Duration::from_secs(2);
      assert_eq!(watch.observe(PictureContent::Black, covered), None);
      assert_eq!(watch.poll(covered + BLACK_PICTURES_REPORT_AFTER / 2), None);
      assert!(watch.poll(covered + BLACK_PICTURES_REPORT_AFTER).is_some());
    }

    #[test]
    fn flat_pictures_and_no_pictures_are_not_black() {
      let mut watch = BlackWatch::new(oneshot::channel().0);
      let start = Instant::now();
      assert_eq!(
        watch.poll(start + Duration::from_secs(60)),
        None,
        "nothing captured"
      );
      for second in 0..10 {
        let at = start + Duration::from_secs(second);
        assert_eq!(watch.observe(PictureContent::Flat, at), None);
      }
    }

    #[tokio::test]
    async fn a_track_sending_black_is_reported_and_counted() {
      let pushed = Arc::new(Mutex::new(None));
      let (encoded, _encoded_rx) = mpsc::unbounded_channel();
      let config = VideoSendConfig {
        maybe_layer_plan: None,
        kind: VideoTrackKind::Screen,
        capture: Box::new(SharedCapture(manual_capture(), Arc::clone(&pushed))),
        layers: simulcast_layers(VGA, 30, 800_000, 1),
        encoders: fake_encoders(),
        maybe_filter: None,
        maybe_idle_refresh: None,
      };
      let (sender, mut first_picture) = VideoSender::start(config, encoded).unwrap();
      let black_pictures = first_picture.take_black_pictures().unwrap();
      assert!(first_picture.take_black_pictures().is_none(), "taken once");
      let frames = pushed.lock().unwrap().clone().unwrap();
      // A screen that stays black after its first picture (a still screen sends no more).
      assert!(frames.send(black_frame()));
      assert_eq!(first_picture.wait().await, Ok(()));
      let report = tokio::time::timeout(BLACK_PICTURES_REPORT_AFTER * 3, black_pictures.wait())
        .await
        .expect("reported in time");
      assert_eq!(report, Some(BlackReport { frames: 1 }));
      assert_eq!(sender.finish().frames_black, 1);
    }

    #[tokio::test]
    async fn the_notice_of_a_track_that_ends_without_black_resolves_empty() {
      let pushed = Arc::new(Mutex::new(None));
      let (encoded, _encoded_rx) = mpsc::unbounded_channel();
      let config = VideoSendConfig {
        maybe_layer_plan: None,
        kind: VideoTrackKind::Camera,
        capture: Box::new(SharedCapture(manual_capture(), Arc::clone(&pushed))),
        layers: simulcast_layers(VGA, 30, 800_000, 1),
        encoders: fake_encoders(),
        maybe_filter: None,
        maybe_idle_refresh: None,
      };
      let (sender, mut first_picture) = VideoSender::start(config, encoded).unwrap();
      let black_pictures = first_picture.take_black_pictures().unwrap();
      let frames = pushed.lock().unwrap().clone().unwrap();
      assert!(frames.send(test_pattern(VGA, 0, 0).unwrap()));
      assert_eq!(first_picture.wait().await, Ok(()));
      assert_eq!(sender.finish().frames_black, 0);
      assert_eq!(black_pictures.wait().await, None);
    }

    fn black_frame() -> VideoFrame {
      let pixels = usize::from(VGA.width) * usize::from(VGA.height);
      let mut data = vec![16; pixels];
      data.resize(pixels * 3 / 2, 128);
      VideoFrame::new(VGA, 0, data).unwrap()
    }
  }

  /// Lets the test push into a capture the sender owns.
  struct SharedCapture(
    ManualCapture,
    Arc<Mutex<Option<crate::video_capture::FrameSender>>>,
  );

  impl VideoCapture for SharedCapture {
    fn format(&self) -> CaptureFormat {
      self.0.format()
    }

    fn start(&mut self, frames: crate::video_capture::FrameSender) -> Result<(), CaptureError> {
      *self.1.lock().unwrap() = Some(frames.clone());
      self.0.start(frames)
    }

    fn stop(&mut self) {
      self.0.stop();
    }
  }

  fn received_frame(frame: EncodedVideoFrame) -> ReceivedVideoFrame {
    ReceivedVideoFrame {
      sender: DeviceId::from_snowflake(Snowflake::new(1).unwrap()),
      stream_id: MediaStreamId(1),
      codec: VideoCodec::H264,
      layer: frame.layer,
      timestamp: frame.timestamp,
      is_keyframe: frame.is_keyframe,
      data: frame.data,
    }
  }
}
