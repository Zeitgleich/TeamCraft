//! The threads between capture and engine: [`VideoSender`] runs capture → (effects) → simulcast
//! encoding, [`VideoReceiver`] decodes one received track for the app. Encoding and decoding take
//! milliseconds per frame, so they stay off the call's async task.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self as std_mpsc, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use teamcraft_proto::{VideoLayerInfo, VideoTrackKind};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::call_video::ReceivedVideoFrame;
use crate::video::{EncodedVideoFrame, VideoFrame, VideoResolution, fit_within};
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
/// How long stopping a track waits for its encoder thread, which stops the capture (cameras wait
/// up to half a second for theirs, `camera_thread`). Bounded: a capture stuck in its driver must
/// not stop the call (issue #683).
const SENDER_STOP_WAIT: Duration = Duration::from_secs(1);

/// Makes the encoder for one layer; hardware where available (see [`software_encoders`]).
pub type EncoderFactory =
  Arc<dyn Fn(&VideoLayerInfo, VideoContent) -> Result<Box<dyn VideoEncoder>, VideoCodecError> + Send + Sync>;

/// Turns frames before encoding, e.g. background effects. Runs on the encoder thread.
pub type FrameFilter = Box<dyn FnMut(VideoFrame) -> VideoFrame + Send>;

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

/// Resolves once a started track encoded its first picture, or with why it never will. While
/// nobody waits for it (dropped), the track gets as long as it needs.
#[derive(Debug)]
pub struct FirstPicture(oneshot::Receiver<Result<(), VideoStartError>>);

/// Why a started track sends nothing.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VideoStartError {
  #[error("no video encoder: {0}")]
  NoEncoder(String),
  #[error("the {0} delivered no picture")]
  NoFrames(&'static str),
  #[error("the {source_name} delivered {captured} pictures, but none could be encoded")]
  NotEncoded {
    source_name: &'static str,
    captured: u64,
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
      encoders,
      maybe_filter,
      maybe_idle_refresh,
    } = config;
    let content = match kind {
      VideoTrackKind::Screen => VideoContent::Screen,
      _ => VideoContent::Camera,
    };
    let (frames, frame_rx) = frame_queue();
    capture.start(frames)?;
    let (controls, control_rx) = std_mpsc::channel();
    let (first_picture, first_picture_rx) = oneshot::channel();
    let counters = Arc::new(SendCounters::default());
    let encoder_loop = EncoderLoop {
      kind: kind.clone(),
      layers: layers.clone(),
      content,
      encoders,
      maybe_filter,
      refresh: Refresh {
        maybe_idle_interval: maybe_idle_refresh,
      },
      frame_rx,
      control_rx,
      encoded,
      counters: Arc::clone(&counters),
      maybe_first_picture: Some(first_picture),
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
    Ok((sender, FirstPicture(first_picture_rx)))
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

  pub fn progress(&self) -> SendProgress {
    self.counters.progress()
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
  layers: Vec<VideoLayerInfo>,
  content: VideoContent,
  encoders: EncoderFactory,
  maybe_filter: Option<FrameFilter>,
  refresh: Refresh,
  frame_rx: FrameReceiver,
  control_rx: std_mpsc::Receiver<SenderControl>,
  encoded: mpsc::UnboundedSender<(VideoTrackKind, EncodedVideoFrame)>,
  counters: Arc<SendCounters>,
  maybe_first_picture: Option<oneshot::Sender<Result<(), VideoStartError>>>,
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
    let send_resolution = self
      .layers
      .iter()
      .map(|layer| VideoResolution {
        width: layer.width,
        height: layer.height,
      })
      .max_by_key(|resolution| u32::from(resolution.width) * u32::from(resolution.height));
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
      let frame = match self.frame_rx.recv_timeout(CONTROL_POLL) {
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
      // A request that came while waiting (a viewer asked for a keyframe) applies to this frame.
      if self.apply_controls(&mut simulcast) == LoopStep::Stop {
        return;
      }
      self.counters.captured.fetch_add(1, Ordering::Relaxed);
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
    let encoded_frames = match simulcast.encode(&frame) {
      Ok(encoded_frames) => encoded_frames,
      Err(error) => {
        let kind = &self.kind;
        encode_warning.warn(|suppressed| warn!(%error, suppressed, ?kind, "video frame not encoded"));
        return Err(LoopStep::Continue);
      },
    };
    let has_pictures = !encoded_frames.is_empty();
    for encoded_frame in encoded_frames {
      self.counters.encoded.fetch_add(1, Ordering::Relaxed);
      if self.encoded.send((self.kind.clone(), encoded_frame)).is_err() {
        return Err(LoopStep::Stop);
      }
    }
    // After the hand-off: once the start is reported, the first picture is on its way.
    if has_pictures {
      self.report_start(Ok(()));
    }
    Ok(LastPicture {
      frame,
      encoded_at: Instant::now(),
    })
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
    let error = first_picture_failure(self.source_name(), self.counters.progress());
    warn!(%error, kind = ?self.kind, "video start failed; the track is stopped");
    self.report_start(Err(error));
    LoopStep::Stop
  }

  fn report_start(&mut self, result: Result<(), VideoStartError>) {
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

/// Why no first picture came: nothing captured, or nothing of it encoded.
fn first_picture_failure(source_name: &'static str, progress: SendProgress) -> VideoStartError {
  if progress.frames_captured == 0 {
    VideoStartError::NoFrames(source_name)
  } else {
    VideoStartError::NotEncoded {
      source_name,
      captured: progress.frames_captured,
    }
  }
}

fn source_name(kind: &VideoTrackKind) -> &'static str {
  match kind {
    VideoTrackKind::Screen => "screen capture",
    _ => "camera",
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

impl FirstPicture {
  /// Waits for the first encoded picture. Bounded: a capture thread that hangs counts as failed
  /// shortly after [`FIRST_PICTURE_TIMEOUT`].
  pub async fn wait(self) -> Result<(), VideoStartError> {
    let backstop = FIRST_PICTURE_TIMEOUT + Duration::from_secs(5);
    match tokio::time::timeout(backstop, self.0).await {
      Ok(Ok(result)) => result,
      Ok(Err(_)) => Err(VideoStartError::ThreadEnded),
      Err(_) => Err(VideoStartError::NoFrames("video source")),
    }
  }
}

impl SendCounters {
  fn progress(&self) -> SendProgress {
    SendProgress {
      frames_captured: self.captured.load(Ordering::Relaxed),
      pictures_encoded: self.encoded.load(Ordering::Relaxed),
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
  /// Stops capturing and encoding, and returns what the track did.
  pub fn finish(mut self) -> SendProgress {
    self.shut_down();
    self.counters.progress()
  }

  /// Stops the encoder thread and waits for it (and with it the capture), at most
  /// [`SENDER_STOP_WAIT`]: this runs on the call's task, whose audio must go on even when a
  /// capture hangs in its driver.
  fn shut_down(&mut self) {
    let (closed, _) = std_mpsc::channel();
    drop(std::mem::replace(&mut self.controls, closed));
    if let Some(thread) = self.thread.take()
      && thread.join_within(SENDER_STOP_WAIT) == Joined::Detached
    {
      warn!(kind = ?self.kind, "the video capture hangs; the track is left behind");
    }
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

  #[test]
  fn a_sender_whose_capture_or_encoder_ends_reports_it() {
    let (encoded, _encoded_rx) = mpsc::unbounded_channel();
    let config = VideoSendConfig {
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
          pictures_encoded: 1
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

    #[test]
    fn the_failure_says_whether_anything_was_captured() {
      assert_eq!(
        first_picture_failure("camera", SendProgress::default()),
        VideoStartError::NoFrames("camera")
      );
      let captured = SendProgress {
        frames_captured: 12,
        pictures_encoded: 0,
      };
      assert_eq!(
        first_picture_failure("screen capture", captured),
        VideoStartError::NotEncoded {
          source_name: "screen capture",
          captured: 12
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

    /// One picture per frame and layer.
    struct FakeEncoder;

    impl VideoEncoder for FakeEncoder {
      fn codec(&self) -> VideoCodec {
        VideoCodec::H264
      }

      fn encode(
        &mut self,
        _frame: &VideoFrame,
        is_keyframe_forced: bool,
      ) -> Result<Option<crate::video_codec::EncodedPicture>, VideoCodecError> {
        Ok(Some(crate::video_codec::EncodedPicture {
          is_keyframe: is_keyframe_forced,
          data: vec![0, 0, 0, 1, 0x65],
        }))
      }
    }

    pub(super) fn fake_encoders() -> EncoderFactory {
      Arc::new(|_, _| Ok(Box::new(FakeEncoder) as Box<dyn VideoEncoder>))
    }

    pub(super) fn manual_capture() -> ManualCapture {
      ManualCapture::new(CaptureFormat {
        resolution: VGA,
        framerate: 30,
      })
    }

    fn start_sender(
      capture: Box<dyn VideoCapture>,
      encoders: EncoderFactory,
    ) -> (
      VideoSender,
      FirstPicture,
      mpsc::UnboundedReceiver<(VideoTrackKind, EncodedVideoFrame)>,
    ) {
      let (encoded, encoded_rx) = mpsc::unbounded_channel();
      let config = VideoSendConfig {
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
      let encoder_loop = EncoderLoop {
        kind: VideoTrackKind::Camera,
        layers: simulcast_layers(VGA, 30, 800_000, 1),
        content: VideoContent::Camera,
        encoders: fake_encoders(),
        maybe_filter: None,
        refresh: Refresh {
          maybe_idle_interval: None,
        },
        frame_rx,
        control_rx,
        encoded,
        counters: Arc::default(),
        maybe_first_picture: Some(first_picture),
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
