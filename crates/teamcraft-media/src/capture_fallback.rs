//! A capture that switches to a second source when the first delivers nothing: on Linux the camera
//! through the portal (PipeWire) first, then the same camera through V4L2 directly. The portal's
//! stream can stay paused for good, for example when the session manager doesn't link the camera
//! node into the portal's restricted remote, and then no picture ever comes (issue #683).
//!
//! The first source gets [`FallbackCapture::new`]'s `wait` for its first frame. If none comes, or
//! it ends before one, it is stopped and the second source is opened and delivers straight into
//! the consumer's queue. Once the first source delivered, it stays (a later stall is the encoder
//! thread's business, which reports it).

use std::sync::mpsc::{self as std_mpsc, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tracing::{info, warn};

use crate::video_capture::{
  CaptureError, CaptureFormat, FrameReceiver, FrameSender, VideoCapture, frame_queue,
};

/// How often the forwarding thread looks for a stop while frames are slow.
const STOP_POLL: Duration = Duration::from_millis(50);

/// Opens the second source (blocking is fine: it runs on the capture's own thread).
pub type CaptureFactory = Box<dyn FnOnce() -> Result<Box<dyn VideoCapture>, CaptureError> + Send>;

/// A first source with a second one to fall back to.
pub struct FallbackCapture {
  format: CaptureFormat,
  maybe_sources: Option<Sources>,
  maybe_running: Option<(std_mpsc::Sender<()>, JoinHandle<()>)>,
}

/// The two sources and how long the first may take.
struct Sources {
  primary: Box<dyn VideoCapture>,
  primary_name: &'static str,
  open_fallback: CaptureFactory,
  fallback_name: &'static str,
  wait: Duration,
}

/// What the first source did before its deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrimaryOutcome {
  /// It delivers: forward its frames until stopped.
  Delivers,
  /// No frame in time, or it ended before one: try the second source.
  FallBack,
  /// Stopped, or the consumer is gone.
  Stopped,
}

impl FallbackCapture {
  /// `primary` gets `wait` for its first frame before `open_fallback` replaces it. The names say
  /// in the log which path delivers ("portal", "V4L2").
  pub fn new(
    primary: Box<dyn VideoCapture>,
    primary_name: &'static str,
    open_fallback: CaptureFactory,
    fallback_name: &'static str,
    wait: Duration,
  ) -> Self {
    Self {
      format: primary.format(),
      maybe_sources: Some(Sources {
        primary,
        primary_name,
        open_fallback,
        fallback_name,
        wait,
      }),
      maybe_running: None,
    }
  }
}

impl VideoCapture for FallbackCapture {
  fn format(&self) -> CaptureFormat {
    self.format
  }

  /// Starts once, like the portal capture it wraps.
  fn start(&mut self, frames: FrameSender) -> Result<(), CaptureError> {
    let mut sources = self
      .maybe_sources
      .take()
      .ok_or_else(|| CaptureError::Device("a fallback capture can only be started once".into()))?;
    let (primary_frames, primary_rx) = frame_queue();
    sources.primary.start(primary_frames)?;
    let (stop, stop_rx) = std_mpsc::channel();
    let thread = std::thread::Builder::new()
      .name("capture-fallback".into())
      .spawn(move || sources.run(&primary_rx, frames, &stop_rx))
      .map_err(|error| CaptureError::Device(error.to_string()))?;
    self.maybe_running = Some((stop, thread));
    Ok(())
  }

  fn stop(&mut self) {
    if let Some((stop, thread)) = self.maybe_running.take() {
      // A closed channel means the thread already ended.
      let _ = stop.send(());
      if thread.join().is_err() {
        warn!("capture fallback thread panicked");
      }
    }
  }
}

impl Drop for FallbackCapture {
  fn drop(&mut self) {
    self.stop();
  }
}

impl Sources {
  fn run(mut self, primary_rx: &FrameReceiver, frames: FrameSender, stop_rx: &std_mpsc::Receiver<()>) {
    match self.await_primary(primary_rx, &frames, stop_rx) {
      PrimaryOutcome::Delivers => {
        info!(source = self.primary_name, "the camera delivers");
        forward(primary_rx, &frames, stop_rx);
        self.primary.stop();
      },
      PrimaryOutcome::Stopped => self.primary.stop(),
      PrimaryOutcome::FallBack => {
        self.primary.stop();
        warn!(
          from = self.primary_name,
          to = self.fallback_name,
          wait = ?self.wait,
          "the camera delivered no picture; trying the other way"
        );
        run_fallback(self.open_fallback, self.fallback_name, frames, stop_rx);
      },
    }
  }

  /// Waits up to `wait` for the first frame and hands it on.
  fn await_primary(
    &self,
    primary_rx: &FrameReceiver,
    frames: &FrameSender,
    stop_rx: &std_mpsc::Receiver<()>,
  ) -> PrimaryOutcome {
    let deadline = Instant::now() + self.wait;
    loop {
      if is_stopped(stop_rx) {
        return PrimaryOutcome::Stopped;
      }
      let left = deadline.saturating_duration_since(Instant::now());
      match primary_rx.recv_timeout(left.min(STOP_POLL)) {
        Ok(frame) => {
          return if frames.send(frame) {
            PrimaryOutcome::Delivers
          } else {
            PrimaryOutcome::Stopped
          };
        },
        Err(RecvTimeoutError::Timeout) if left.is_zero() => return PrimaryOutcome::FallBack,
        Err(RecvTimeoutError::Timeout) => {},
        Err(RecvTimeoutError::Disconnected) => return PrimaryOutcome::FallBack,
      }
    }
  }
}

/// Hands the first source's frames on until it ends, the consumer is gone, or a stop.
fn forward(primary_rx: &FrameReceiver, frames: &FrameSender, stop_rx: &std_mpsc::Receiver<()>) {
  loop {
    if is_stopped(stop_rx) {
      return;
    }
    match primary_rx.recv_timeout(STOP_POLL) {
      Ok(frame) => {
        if !frames.send(frame) {
          return;
        }
      },
      Err(RecvTimeoutError::Timeout) => {},
      // Dropping `frames` afterwards tells the consumer the capture ended.
      Err(RecvTimeoutError::Disconnected) => return,
    }
  }
}

/// Opens the second source into the consumer's queue and keeps it until a stop.
fn run_fallback(
  open_fallback: CaptureFactory,
  name: &'static str,
  frames: FrameSender,
  stop_rx: &std_mpsc::Receiver<()>,
) {
  let mut fallback = match open_fallback() {
    Ok(fallback) => fallback,
    Err(error) => {
      // `frames` goes with this return: the consumer sees the capture end and reports it.
      warn!(%error, source = name, "the other way to the camera failed too");
      return;
    },
  };
  if let Err(error) = fallback.start(frames) {
    warn!(%error, source = name, "the other way to the camera did not start");
    return;
  }
  info!(source = name, "the camera delivers through the fallback");
  // Either a stop or the capture being dropped (the sender is gone): stop the source.
  let _ = stop_rx.recv();
  fallback.stop();
}

fn is_stopped(stop_rx: &std_mpsc::Receiver<()>) -> bool {
  !matches!(stop_rx.try_recv(), Err(std_mpsc::TryRecvError::Empty))
}

#[cfg(test)]
mod tests {
  use std::sync::{Arc, Mutex};

  use super::*;
  use crate::video::VideoResolution;
  use crate::video_capture::test_pattern;

  const FORMAT: CaptureFormat = CaptureFormat {
    resolution: VideoResolution {
      width: 64,
      height: 32,
    },
    framerate: 30,
  };
  /// Long enough never to run out in a test that doesn't want the fallback.
  const LONG: Duration = Duration::from_secs(600);
  const RECEIVE: Duration = Duration::from_secs(10);

  #[test]
  fn a_delivering_first_source_stays_and_the_fallback_is_never_opened() {
    let (primary, primary_frames) = shared_capture();
    let opened = Arc::new(Mutex::new(false));
    let mut capture = FallbackCapture::new(
      Box::new(primary),
      "portal",
      factory_marking(&opened),
      "V4L2",
      LONG,
    );
    let (frames, frame_rx) = frame_queue();
    capture.start(frames).unwrap();
    let sender = primary_frames.lock().unwrap().clone().unwrap();
    assert!(sender.send(frame(1)));
    assert_eq!(frame_rx.recv_timeout(RECEIVE).unwrap().timestamp_us(), 1);
    assert!(sender.send(frame(2)));
    assert_eq!(frame_rx.recv_timeout(RECEIVE).unwrap().timestamp_us(), 2);
    capture.stop();
    assert!(!*opened.lock().unwrap());
    assert!(
      frame_rx.recv_timeout(RECEIVE).is_err(),
      "stopping ends the consumer's queue"
    );
  }

  #[test]
  fn a_first_source_that_ends_before_a_picture_falls_back_at_once() {
    let (fallback, fallback_frames) = shared_capture();
    let mut capture = FallbackCapture::new(
      Box::new(EndingCapture),
      "portal",
      Box::new(move || Ok(Box::new(fallback) as Box<dyn VideoCapture>)),
      "V4L2",
      LONG,
    );
    let (frames, frame_rx) = frame_queue();
    capture.start(frames).unwrap();
    let sender = wait_for_start(&fallback_frames);
    assert!(sender.send(frame(7)));
    assert_eq!(frame_rx.recv_timeout(RECEIVE).unwrap().timestamp_us(), 7);
    capture.stop();
  }

  #[test]
  fn a_silent_first_source_falls_back_after_its_wait() {
    let (primary, _primary_frames) = shared_capture();
    let (fallback, fallback_frames) = shared_capture();
    let mut capture = FallbackCapture::new(
      Box::new(primary),
      "portal",
      Box::new(move || Ok(Box::new(fallback) as Box<dyn VideoCapture>)),
      "V4L2",
      // No time at all: the first source never sends, so the outcome doesn't depend on timing.
      Duration::ZERO,
    );
    let (frames, frame_rx) = frame_queue();
    capture.start(frames).unwrap();
    let sender = wait_for_start(&fallback_frames);
    assert!(sender.send(frame(3)));
    assert_eq!(frame_rx.recv_timeout(RECEIVE).unwrap().timestamp_us(), 3);
    capture.stop();
  }

  #[test]
  fn a_fallback_that_fails_too_ends_the_capture() {
    let mut capture = FallbackCapture::new(
      Box::new(EndingCapture),
      "portal",
      Box::new(|| Err(CaptureError::NoDevice("camera"))),
      "V4L2",
      LONG,
    );
    let (frames, frame_rx) = frame_queue();
    capture.start(frames).unwrap();
    // The consumer sees the end, as the encoder thread does (and reports "capture ended").
    assert_eq!(
      frame_rx.recv_timeout(RECEIVE).err(),
      Some(RecvTimeoutError::Disconnected)
    );
  }

  #[test]
  fn it_starts_once() {
    let (primary, _frames) = shared_capture();
    let mut capture = FallbackCapture::new(
      Box::new(primary),
      "portal",
      Box::new(|| Err(CaptureError::NoDevice("camera"))),
      "V4L2",
      LONG,
    );
    capture.start(frame_queue().0).unwrap();
    assert!(capture.start(frame_queue().0).is_err());
  }

  /// Hands its queue to the test.
  struct SharedCapture(Arc<Mutex<Option<FrameSender>>>);

  impl VideoCapture for SharedCapture {
    fn format(&self) -> CaptureFormat {
      FORMAT
    }

    fn start(&mut self, frames: FrameSender) -> Result<(), CaptureError> {
      *self.0.lock().unwrap() = Some(frames);
      Ok(())
    }

    fn stop(&mut self) {
      *self.0.lock().unwrap() = None;
    }
  }

  /// Drops its queue at once, like a PipeWire stream that fails.
  struct EndingCapture;

  impl VideoCapture for EndingCapture {
    fn format(&self) -> CaptureFormat {
      FORMAT
    }

    fn start(&mut self, frames: FrameSender) -> Result<(), CaptureError> {
      drop(frames);
      Ok(())
    }

    fn stop(&mut self) {}
  }

  fn shared_capture() -> (SharedCapture, Arc<Mutex<Option<FrameSender>>>) {
    let slot = Arc::new(Mutex::new(None));
    (SharedCapture(Arc::clone(&slot)), slot)
  }

  fn factory_marking(opened: &Arc<Mutex<bool>>) -> CaptureFactory {
    let opened = Arc::clone(opened);
    Box::new(move || {
      *opened.lock().unwrap() = true;
      Err(CaptureError::NoDevice("camera"))
    })
  }

  /// The fallback starts on the capture's thread; waits until it did (bounded).
  fn wait_for_start(slot: &Arc<Mutex<Option<FrameSender>>>) -> FrameSender {
    let deadline = Instant::now() + RECEIVE;
    loop {
      if let Some(sender) = slot.lock().unwrap().clone() {
        return sender;
      }
      assert!(Instant::now() < deadline, "the fallback never started");
      std::thread::yield_now();
    }
  }

  fn frame(timestamp_us: u64) -> crate::video::VideoFrame {
    test_pattern(FORMAT.resolution, 0, timestamp_us).unwrap()
  }
}
