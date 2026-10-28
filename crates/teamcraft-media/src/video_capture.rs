//! Where video frames come from: cameras and screens behind one trait, so the call driver and
//! the effects pipeline don't care about the platform, and tests use fakes.
//!
//! Captures run on their own threads and hand I420 frames to a bounded channel; a full channel
//! means the encoder is behind, and the capture drops the frame instead of building latency.

use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::video::{VideoError, VideoFrame, VideoResolution};

/// Frames waiting for the encoder; more are dropped.
pub const CAPTURE_QUEUE_FRAMES: usize = 2;
/// The [`VideoCapture::stall_limit`] of cameras whose read never reports an end (#922): far above
/// the gap between two frames of any running camera, even in low light, and above the short pause
/// AVFoundation makes when another app reconfigures the camera; short enough that the user learns
/// within seconds that their camera went away.
pub const CAMERA_STALL_LIMIT: Duration = Duration::from_secs(5);

/// A camera, a screen or a test source.
pub trait VideoCapture: Send {
  /// Resolution and frame rate the capture delivers (after negotiation with the device).
  fn format(&self) -> CaptureFormat;
  /// Starts delivering frames into `frames` until [`VideoCapture::stop`] or drop.
  fn start(&mut self, frames: FrameSender) -> Result<(), CaptureError>;
  fn stop(&mut self);

  /// How long the capture may deliver nothing, once it delivered, before its track counts as
  /// ended. For cameras that deliver all the time but never report that they stopped (nokhwa's
  /// AVFoundation camera, unplugged or interrupted, #922). `None` (the default): no limit, for
  /// captures that end by themselves when their device goes, screens that deliver only on change
  /// and frames an app pushes when it can.
  fn stall_limit(&self) -> Option<Duration> {
    None
  }
}

/// A camera to show in a picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraInfo {
  /// Platform id, stable across restarts and re-plugging, so it can be stored as the user's
  /// choice: a device path on Linux, AVFoundation's unique id on macOS, the device's symbolic
  /// link on Windows.
  pub id: String,
  pub name: String,
  /// What sort of camera it is, as far as the platform tells (orders the default choice, #819).
  pub kind: CameraKind,
}

/// What sort of camera a [`CameraInfo`] is. The variants are in order of preference for the
/// default camera (`camera_choice` has the rules).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CameraKind {
  /// The computer's own camera (AVFoundation's built-in device types).
  BuiltIn,
  /// A camera plugged in (USB): AVFoundation's external device type.
  External,
  /// The platform doesn't tell (Linux, Windows): a real camera as far as we know.
  Unknown,
  /// An iPhone or iPad used as a camera (Continuity Camera). Using it asks on the phone, which
  /// the user may decline.
  Continuity,
  /// Desk View: the iPhone's view of the desk, not of the person.
  DeskView,
  /// A camera made by software (OBS, v4l2loopback, …): it shows whatever that software sends.
  Virtual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureFormat {
  pub resolution: VideoResolution,
  pub framerate: u8,
}

/// The capture side of a frame queue; drops frames while the consumer is busy.
#[derive(Debug, Clone)]
pub struct FrameSender {
  sender: SyncSender<VideoFrame>,
}

/// A fake for tests: frames are pushed by hand.
pub struct ManualCapture {
  format: CaptureFormat,
  maybe_frames: Option<FrameSender>,
}

/// Moving colour bars at a fixed rate, for headless clients and manual checks.
pub struct TestPatternCapture {
  format: CaptureFormat,
  maybe_running: Option<(std::sync::mpsc::Sender<()>, JoinHandle<()>)>,
}

#[derive(Debug, Error)]
pub enum CaptureError {
  #[error("no {0} available")]
  NoDevice(&'static str),
  #[error("the user or the system denied access to the {0}")]
  Denied(&'static str),
  /// The user closed the picker (screen or window) without choosing: nothing to report.
  #[error("the {0} choice was cancelled")]
  Cancelled(&'static str),
  #[error("capture: {0}")]
  Device(String),
  /// Another app holds the camera.
  #[error("the camera is in use by another app: {0}")]
  InUse(String),
  /// The camera didn't come up: an iPhone camera (Continuity Camera) declined on the phone, the
  /// phone locked or out of reach.
  #[error("the camera did not connect: {0}")]
  NotConnected(String),
  /// None of the modes the camera offers could be set.
  #[error("the camera offers no mode we can use: {0}")]
  NoUsableMode(String),
  #[error("unsupported pixel format {0}")]
  UnsupportedFormat(String),
  #[error(transparent)]
  Frame(#[from] VideoError),
}

/// The receiving end of [`frame_queue`].
pub type FrameReceiver = Receiver<VideoFrame>;

/// A bounded frame queue between a capture and its consumer.
pub fn frame_queue() -> (FrameSender, FrameReceiver) {
  let (sender, receiver) = sync_channel(CAPTURE_QUEUE_FRAMES);
  (FrameSender { sender }, receiver)
}

impl FrameSender {
  /// Queues a frame; `false` once the consumer is gone (stop capturing then).
  pub fn send(&self, frame: VideoFrame) -> bool {
    match self.sender.try_send(frame) {
      Ok(()) | Err(TrySendError::Full(_)) => true,
      Err(TrySendError::Disconnected(_)) => false,
    }
  }
}

impl ManualCapture {
  pub fn new(format: CaptureFormat) -> Self {
    Self {
      format,
      maybe_frames: None,
    }
  }

  /// Delivers `frame` if started; `false` otherwise or when the consumer is gone.
  pub fn push(&self, frame: VideoFrame) -> bool {
    self
      .maybe_frames
      .as_ref()
      .is_some_and(|frames| frames.send(frame))
  }

  /// A handle that pushes frames from another thread after `start`.
  pub fn sender(&self) -> Option<FrameSender> {
    self.maybe_frames.clone()
  }
}

impl VideoCapture for ManualCapture {
  fn format(&self) -> CaptureFormat {
    self.format
  }

  fn start(&mut self, frames: FrameSender) -> Result<(), CaptureError> {
    self.maybe_frames = Some(frames);
    Ok(())
  }

  fn stop(&mut self) {
    self.maybe_frames = None;
  }
}

impl TestPatternCapture {
  pub fn new(format: CaptureFormat) -> Self {
    Self {
      format,
      maybe_running: None,
    }
  }
}

impl VideoCapture for TestPatternCapture {
  fn format(&self) -> CaptureFormat {
    self.format
  }

  fn start(&mut self, frames: FrameSender) -> Result<(), CaptureError> {
    self.stop();
    let format = self.format;
    let (stop, stop_rx) = std::sync::mpsc::channel();
    let thread = std::thread::Builder::new()
      .name("test-pattern".into())
      .spawn(move || {
        let period = Duration::from_secs(1) / u32::from(format.framerate.max(1));
        let start = Instant::now();
        for index in 0u64.. {
          let timestamp_us = u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX);
          let Ok(frame) = test_pattern(format.resolution, index, timestamp_us) else {
            return;
          };
          if !frames.send(frame) {
            return;
          }
          // Paced by the stop channel's timeout: wakes for the next frame or ends at once.
          if !matches!(
            stop_rx.recv_timeout(period),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
          ) {
            return;
          }
        }
      })
      .map_err(|error| CaptureError::Device(error.to_string()))?;
    self.maybe_running = Some((stop, thread));
    Ok(())
  }

  fn stop(&mut self) {
    if let Some((stop, thread)) = self.maybe_running.take() {
      drop(stop);
      if thread.join().is_err() {
        tracing::warn!("test pattern thread panicked");
      }
    }
  }
}

impl Drop for TestPatternCapture {
  fn drop(&mut self) {
    self.stop();
  }
}

/// Eight vertical colour bars scrolling one bar per second at 30 fps, plus a frame counter band
/// at the top (luma steps) so receivers can tell frames apart.
pub fn test_pattern(
  resolution: VideoResolution,
  index: u64,
  timestamp_us: u64,
) -> Result<VideoFrame, VideoError> {
  // (Y, U, V) of white, yellow, cyan, green, magenta, red, blue, black (BT.601).
  const BARS: [(u8, u8, u8); 8] = [
    (235, 128, 128),
    (210, 16, 146),
    (170, 166, 16),
    (145, 54, 34),
    (106, 202, 222),
    (81, 90, 240),
    (41, 240, 110),
    (16, 128, 128),
  ];
  let (width, height) = (usize::from(resolution.width), usize::from(resolution.height));
  let shift = (index as usize * width / 240) % width.max(1);
  let bar_of = |x: usize| BARS[((x + shift) * BARS.len() / width.max(1)) % BARS.len()];
  let mut data = Vec::with_capacity(width * height * 3 / 2);
  for y in 0..height {
    for x in 0..width {
      let luma = if y < height / 16 {
        // Frame counter: 8 bits of the index as dark/bright blocks.
        let bit = x * 8 / width.max(1);
        if (index >> bit) & 1 == 1 { 235 } else { 16 }
      } else {
        bar_of(x).0
      };
      data.push(luma);
    }
  }
  for plane in [1, 2] {
    for _ in 0..height / 2 {
      for x in 0..width / 2 {
        let (_, u, v) = bar_of(x * 2);
        data.push(if plane == 1 { u } else { v });
      }
    }
  }
  VideoFrame::new(resolution, timestamp_us, data)
}

#[cfg(test)]
mod tests {
  use super::*;

  const FORMAT: CaptureFormat = CaptureFormat {
    resolution: VideoResolution {
      width: 320,
      height: 240,
    },
    framerate: 30,
  };

  #[test]
  fn full_queue_drops_frames_instead_of_blocking() {
    let mut capture = ManualCapture::new(FORMAT);
    let (frames, receiver) = frame_queue();
    assert!(!capture.push(frame(0)), "not started");
    capture.start(frames).unwrap();
    for index in 0..5 {
      assert!(capture.push(frame(index)));
    }
    let received: Vec<u64> = receiver.try_iter().map(|frame| frame.timestamp_us()).collect();
    assert_eq!(received, vec![0, 1], "only the queue's capacity, oldest first");
    drop(receiver);
    assert!(!capture.push(frame(9)), "consumer gone");
  }

  #[test]
  fn test_pattern_frames_differ_and_have_the_right_size() {
    let first = test_pattern(FORMAT.resolution, 0, 0).unwrap();
    let second = test_pattern(FORMAT.resolution, 1, 33_333).unwrap();
    assert_eq!(first.data().len(), 320 * 240 * 3 / 2);
    assert_ne!(first.data(), second.data());
  }

  fn frame(index: u64) -> VideoFrame {
    test_pattern(FORMAT.resolution, index, index).unwrap()
  }
}
