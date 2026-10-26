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

/// How often the encoder thread looks at controls while no frame arrives.
const CONTROL_POLL: Duration = Duration::from_millis(20);
/// How long a started track may take to its first encoded picture before the start counts as
/// failed (a camera that delivers nothing, a capture stuck in negotiation, no working encoder).
/// Generous: some cameras take a few seconds to wake up.
pub const FIRST_PICTURE_TIMEOUT: Duration = Duration::from_secs(10);
/// Repeated failures of one kind (a frame not encoded, not sent) are logged at most this often.
const REPEATED_WARNING_INTERVAL: Duration = Duration::from_secs(10);

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
  thread: Option<JoinHandle<()>>,
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
      frame_rx,
      control_rx,
      encoded,
      counters: Arc::clone(&counters),
      maybe_first_picture: Some(first_picture),
    };
    let thread = std::thread::Builder::new()
      .name("video-encoder".into())
      .spawn(move || {
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
    self.thread.as_ref().is_some_and(JoinHandle::is_finished)
  }
}

/// The encoder thread of one track.
struct EncoderLoop {
  kind: VideoTrackKind,
  layers: Vec<VideoLayerInfo>,
  content: VideoContent,
  encoders: EncoderFactory,
  maybe_filter: Option<FrameFilter>,
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
    loop {
      if self.apply_controls(&mut simulcast) == LoopStep::Stop {
        return;
      }
      if self.check_first_picture(Instant::now(), started_at) == LoopStep::Stop {
        return;
      }
      let frame = match self.frame_rx.recv_timeout(CONTROL_POLL) {
        Ok(frame) => frame,
        Err(RecvTimeoutError::Timeout) => continue,
        Err(RecvTimeoutError::Disconnected) => {
          warn!(kind = ?self.kind, "the video capture ended");
          self.report_start(Err(VideoStartError::CaptureEnded(self.source_name())));
          return;
        },
      };
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
      let encoded_frames = match simulcast.encode(&frame) {
        Ok(encoded_frames) => encoded_frames,
        Err(error) => {
          let kind = &self.kind;
          encode_warning.warn(|suppressed| warn!(%error, suppressed, ?kind, "video frame not encoded"));
          continue;
        },
      };
      if !encoded_frames.is_empty() {
        self.report_start(Ok(()));
      }
      for encoded_frame in encoded_frames {
        self.counters.encoded.fetch_add(1, Ordering::Relaxed);
        if self.encoded.send((self.kind.clone(), encoded_frame)).is_err() {
          return;
        }
      }
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

  fn shut_down(&mut self) {
    let (closed, _) = std_mpsc::channel();
    drop(std::mem::replace(&mut self.controls, closed));
    if let Some(thread) = self.thread.take()
      && thread.join().is_err()
    {
      warn!("video encoder thread panicked");
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
      // It used to take the request off the queue there, looking for a stop (issue #683).
      sender.request_keyframe(0);
      tokio::time::sleep(CONTROL_POLL * 5).await;
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

    fn fake_encoders() -> EncoderFactory {
      Arc::new(|_, _| Ok(Box::new(FakeEncoder) as Box<dyn VideoEncoder>))
    }

    fn manual_capture() -> ManualCapture {
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
        frame_rx,
        control_rx,
        encoded,
        counters: Arc::default(),
        maybe_first_picture: Some(first_picture),
      };
      (encoder_loop, first_picture_rx)
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
