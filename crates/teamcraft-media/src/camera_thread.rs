//! A camera read on a thread of its own, for camera APIs whose read blocks until a frame comes
//! (nokhwa's AVFoundation and Media Foundation cameras). Such a read can block for good: a
//! camera that opens but never delivers a frame. Stopping then gives up after a bounded wait, so
//! the encoder thread and the call behind it go on (issue #683).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tracing::warn;

use crate::video::VideoFrame;
use crate::video_capture::{CaptureError, FrameSender};
use crate::worker_thread::{Joined, WorkerThread};

/// How long stopping waits for the camera thread to end. A running camera delivers a frame every
/// few tens of milliseconds, and the thread looks for the stop after each.
const CAMERA_STOP_WAIT: Duration = Duration::from_millis(500);

/// Reads frames from an opened camera.
pub(crate) trait CameraReader {
  /// The next frame (blocking until it comes), with its time since `start`; `Ok(None)` for a
  /// frame that couldn't be used, `Err` when the camera stopped.
  fn read(&mut self, start: Instant) -> Result<Option<VideoFrame>, CaptureError>;

  /// Closes the camera; called on the camera thread once reading ends.
  fn close(&mut self);
}

/// A running camera thread.
pub(crate) struct CameraThread {
  stop: Arc<AtomicBool>,
  thread: WorkerThread,
}

impl CameraThread {
  /// Opens the camera with `open` on a new thread (some camera handles can't move between
  /// threads) and sends its frames into `frames` until stopped. Returns once the camera opened,
  /// or with why it didn't.
  pub(crate) fn start<R, O>(name: &'static str, open: O, frames: FrameSender) -> Result<Self, CaptureError>
  where
    R: CameraReader,
    O: FnOnce() -> Result<R, CaptureError> + Send + 'static,
  {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let (opened, opened_rx) = std::sync::mpsc::channel();
    let thread = WorkerThread::spawn(name, move || {
      let mut reader = match open() {
        Ok(reader) => {
          let _ = opened.send(Ok(()));
          reader
        },
        Err(error) => {
          let _ = opened.send(Err(error));
          return;
        },
      };
      read_until_stopped(&mut reader, &frames, &thread_stop);
      reader.close();
    })
    .map_err(|error| CaptureError::Device(error.to_string()))?;
    let running = Self { stop, thread };
    let result = opened_rx
      .recv()
      .unwrap_or_else(|_| Err(CaptureError::Device("camera thread ended".into())));
    match result {
      Ok(()) => Ok(running),
      Err(error) => {
        running.stop();
        Err(error)
      },
    }
  }

  /// Stops reading; gives up waiting after [`CAMERA_STOP_WAIT`] (a read that never returns).
  pub(crate) fn stop(self) {
    self.stop.store(true, Ordering::Relaxed);
    if self.thread.join_within(CAMERA_STOP_WAIT) == Joined::Detached {
      warn!("the camera delivers nothing and doesn't stop; it is left behind");
    }
  }
}

fn read_until_stopped(reader: &mut impl CameraReader, frames: &FrameSender, stop: &AtomicBool) {
  let start = Instant::now();
  while !stop.load(Ordering::Relaxed) {
    match reader.read(start) {
      Ok(Some(frame)) => {
        if !frames.send(frame) {
          return;
        }
      },
      Ok(None) => {},
      Err(error) => {
        warn!(%error, "camera stopped");
        return;
      },
    }
  }
}

#[cfg(test)]
mod tests {
  use std::sync::mpsc;

  use super::*;
  use crate::video::VideoResolution;
  use crate::video_capture::{frame_queue, test_pattern};

  const SIZE: VideoResolution = VideoResolution {
    width: 64,
    height: 32,
  };

  #[test]
  fn frames_flow_until_stopped() {
    let (frames, frame_rx) = frame_queue();
    let camera = CameraThread::start("test-camera", || Ok(Delivering(0)), frames).unwrap();
    assert!(frame_rx.recv_timeout(Duration::from_secs(5)).is_ok());
    camera.stop();
  }

  #[test]
  fn a_camera_that_never_delivers_still_stops_within_the_bound() {
    let (frames, _frame_rx) = frame_queue();
    let (reading, is_reading) = mpsc::channel();
    let (_never_sent, blocked) = mpsc::channel::<()>();
    let camera = CameraThread::start(
      "test-silent-camera",
      move || Ok(Silent { reading, blocked }),
      frames,
    )
    .unwrap();
    // Only once the camera thread is inside its read, which never returns: a stop before that
    // ends the thread at once and proves nothing.
    is_reading
      .recv_timeout(Duration::from_secs(10))
      .expect("the camera thread reads");
    // Stopped on a thread of its own, so a stop that waits for good fails the test instead of
    // hanging it.
    let (stopped, has_stopped) = mpsc::channel();
    std::thread::spawn(move || {
      let start = Instant::now();
      camera.stop();
      let _ = stopped.send(start.elapsed());
    });
    let waited = has_stopped
      .recv_timeout(CAMERA_STOP_WAIT * 10)
      .expect("stopping a camera that never delivers came back");
    assert!(
      waited >= CAMERA_STOP_WAIT,
      "it waited for the thread first: {waited:?}"
    );
  }

  #[test]
  fn a_camera_that_doesnt_open_says_why() {
    let (frames, _frame_rx) = frame_queue();
    let result = CameraThread::start(
      "test-busy-camera",
      || Err::<Delivering, _>(CaptureError::Device("busy".into())),
      frames,
    );
    assert!(matches!(result, Err(CaptureError::Device(message)) if message == "busy"));
  }

  /// A camera delivering test frames.
  struct Delivering(u64);

  impl CameraReader for Delivering {
    fn read(&mut self, _start: Instant) -> Result<Option<VideoFrame>, CaptureError> {
      self.0 += 1;
      Ok(Some(test_pattern(SIZE, self.0, self.0 * 33_333)?))
    }

    fn close(&mut self) {}
  }

  /// A camera whose read never returns, as AVFoundation's without a frame. It says when it
  /// starts reading.
  struct Silent {
    reading: mpsc::Sender<()>,
    blocked: mpsc::Receiver<()>,
  }

  impl CameraReader for Silent {
    fn read(&mut self, _start: Instant) -> Result<Option<VideoFrame>, CaptureError> {
      // The test may have stopped waiting for this.
      let _ = self.reading.send(());
      let _ = self.blocked.recv();
      Ok(None)
    }

    fn close(&mut self) {}
  }
}
