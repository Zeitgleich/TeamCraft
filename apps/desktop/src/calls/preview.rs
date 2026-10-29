//! The pre-join check on the desktop (#442): the camera as the call would send it (through the
//! same background effects) into the self view, and the microphone's level into the meter.
//! Nothing is encoded or sent. Each input runs on a thread of its own and is released when the
//! check ends: the pre-join screen closed or was hidden, or a join takes the devices over.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self as std_mpsc, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use teamcraft_client_api::calls::{CallPreview, PreviewCamera};
use teamcraft_client_api::failure::{ActionFailure, CallFailure, FailureKind};
use teamcraft_media::audio_device::{AudioDeviceError, AudioInput};
use teamcraft_media::audio_frame::SAMPLE_RATE_HZ;
use teamcraft_media::audio_io::AudioDeviceChoice;
use teamcraft_media::audio_level::MeterWindow;
use teamcraft_media::video_capture::{FrameReceiver, VideoCapture, frame_queue};
use teamcraft_media::video_pipeline::FrameFilter;
use tracing::{debug, warn};

use super::ChannelId;

/// How long a camera may take to its first picture before the check gives up on it.
const FIRST_PICTURE_TIMEOUT: Duration = Duration::from_secs(10);
/// How often a check's thread looks whether it should end while nothing arrives.
const STOP_POLL: Duration = Duration::from_millis(100);
/// The meter gets a reading this often: ten a second.
const LEVEL_INTERVAL: Duration = Duration::from_millis(100);
/// A reading older than this is from a microphone that stopped delivering: the meter shows
/// silence instead.
const STALE_LEVEL: Duration = Duration::from_millis(500);

/// What the check asked for and what runs of it.
#[derive(Default)]
pub(super) struct Preview {
  /// The last request: an input starts when it is asked for, not again with every request, so a
  /// refused one isn't asked for over and over.
  pub(super) wanted: CallPreview,
  pub(super) maybe_camera: Option<CameraCheck>,
  pub(super) maybe_microphone: Option<MicrophoneCheck>,
}

/// The camera pumped through the call's frame filter and dropped.
pub(super) struct CameraCheck {
  stop: std_mpsc::Sender<()>,
  thread: JoinHandle<()>,
}

/// The microphone measured for the meter.
pub(super) struct MicrophoneCheck {
  stop: std_mpsc::Sender<()>,
  thread: JoinHandle<()>,
}

/// Where the microphone check's readings go: the webview's meter, while it watches.
#[derive(Clone, Default)]
pub(crate) struct LevelWatch {
  maybe_watcher: Arc<Mutex<Option<LevelWatcher>>>,
}

struct LevelWatcher {
  channel: ChannelId,
  send: Box<dyn Fn(f32) -> bool + Send>,
}

impl Preview {
  /// Ends the camera check and waits until the camera is released: a join may open it next.
  pub(super) async fn stop_camera(&mut self) {
    if let Some(check) = self.maybe_camera.take()
      && let Err(error) = tokio::task::spawn_blocking(move || check.stop()).await
    {
      warn!(%error, "the camera check did not stop cleanly");
    }
  }

  /// Ends the microphone check and waits until the microphone is released.
  pub(super) async fn stop_microphone(&mut self) {
    if let Some(check) = self.maybe_microphone.take()
      && let Err(error) = tokio::task::spawn_blocking(move || check.stop()).await
    {
      warn!(%error, "the microphone check did not stop cleanly");
    }
  }
}

impl CameraCheck {
  /// Starts `capture` and hands each frame to `filter` (effects, then the self view). `on_state`
  /// hears when the first picture passed, or that none came in time or the camera stopped.
  /// Blocking: the capture may take a moment to start. A camera that doesn't start fails with
  /// `ActionFailure` wire text.
  pub(super) fn start(
    mut capture: Box<dyn VideoCapture>,
    filter: FrameFilter,
    on_state: impl Fn(PreviewCamera) + Send + 'static,
  ) -> Result<Self, String> {
    let (frames_tx, frames) = frame_queue();
    capture
      .start(frames_tx)
      .map_err(|error| super::video::camera_failure_wire(&error))?;
    let (stop, stop_rx) = std_mpsc::channel();
    let thread = std::thread::Builder::new()
      .name("teamcraft-camera-check".into())
      .spawn(move || {
        pump(&frames, filter, &stop_rx, &on_state, FIRST_PICTURE_TIMEOUT);
        capture.stop();
      })
      .map_err(|error| error.to_string())?;
    Ok(Self { stop, thread })
  }

  /// Stops the camera and returns once it is released (blocking).
  pub(super) fn stop(self) {
    drop(self.stop);
    if self.thread.join().is_err() {
      warn!("the camera check's thread panicked");
    }
  }
}

impl MicrophoneCheck {
  /// Opens the microphone on a thread of its own (an audio stream stays on the thread that opened
  /// it) and sends its level to `levels` until stopped. Blocking: returns once it opened or failed;
  /// the error is `ActionFailure` wire text.
  pub(super) fn start(choice: AudioDeviceChoice, levels: LevelWatch) -> Result<Self, String> {
    let (stop, stop_rx) = std_mpsc::channel::<()>();
    let (opened_tx, opened) = std_mpsc::channel();
    let thread = std::thread::Builder::new()
      .name("teamcraft-microphone-check".into())
      .spawn(move || measure(&choice, &levels, &stop_rx, &opened_tx))
      .map_err(|error| error.to_string())?;
    match opened.recv() {
      Ok(Ok(())) => Ok(Self { stop, thread }),
      Ok(Err(error)) => {
        // The thread ends right after telling.
        let _ = thread.join();
        Err(microphone_failure_wire(&error))
      },
      Err(_) => Err("the microphone check stopped before the microphone opened".to_owned()),
    }
  }

  /// Releases the microphone and returns once it is (blocking).
  pub(super) fn stop(self) {
    drop(self.stop);
    if self.thread.join().is_err() {
      warn!("the microphone check's thread panicked");
    }
  }
}

impl LevelWatch {
  /// Sends the readings to `send` until [`Self::unwatch`], another watcher, or `send` answering
  /// that it could not deliver.
  pub(crate) fn watch(&self, channel: ChannelId, send: impl Fn(f32) -> bool + Send + 'static) {
    *self.lock() = Some(LevelWatcher {
      channel,
      send: Box::new(send),
    });
  }

  /// Stops the watcher of `channel`; a newer watcher (another channel) stays.
  pub(crate) fn unwatch(&self, channel: ChannelId) {
    let mut maybe_watcher = self.lock();
    if maybe_watcher
      .as_ref()
      .is_some_and(|watcher| watcher.channel == channel)
    {
      *maybe_watcher = None;
    }
  }

  pub(super) fn publish(&self, level: f32) {
    let mut maybe_watcher = self.lock();
    if maybe_watcher
      .as_ref()
      .is_some_and(|watcher| !(watcher.send)(level))
    {
      debug!("the webview takes no more microphone levels");
      *maybe_watcher = None;
    }
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, Option<LevelWatcher>> {
    self.maybe_watcher.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

/// The camera check's thread: frames through `filter` until stopped, the camera fails, or no
/// first picture comes within `first_picture_timeout`.
fn pump(
  frames: &FrameReceiver,
  mut filter: FrameFilter,
  stop: &std_mpsc::Receiver<()>,
  on_state: &dyn Fn(PreviewCamera),
  first_picture_timeout: Duration,
) {
  let started = Instant::now();
  let mut is_live = false;
  loop {
    // Stopped, or the check is gone.
    if !matches!(stop.try_recv(), Err(TryRecvError::Empty)) {
      return;
    }
    match frames.recv_timeout(STOP_POLL) {
      Ok(frame) => {
        drop(filter(frame));
        if !is_live {
          is_live = true;
          on_state(PreviewCamera::Live);
        }
      },
      Err(RecvTimeoutError::Timeout) if is_live || started.elapsed() < first_picture_timeout => {},
      Err(RecvTimeoutError::Timeout) => {
        warn!("the camera sent no picture; the check gives up on it");
        on_state(PreviewCamera::Failed);
        return;
      },
      Err(RecvTimeoutError::Disconnected) => {
        warn!("the camera stopped during the check");
        on_state(PreviewCamera::Failed);
        return;
      },
    }
  }
}

/// The microphone check's thread: opens the microphone, tells `opened`, then sends the newest
/// reading to `levels` every [`LEVEL_INTERVAL`] until stopped.
fn measure(
  choice: &AudioDeviceChoice,
  levels: &LevelWatch,
  stop: &std_mpsc::Receiver<()>,
  opened: &std_mpsc::Sender<Result<(), AudioDeviceError>>,
) {
  // The audio callback only stores the reading: the webview is told from this thread, never
  // from the audio thread. A microphone that stopped delivering (unplugged) reads as silence
  // instead of its last level.
  let newest = Arc::new(AtomicU32::new(0.0_f32.to_bits()));
  let is_fresh = Arc::new(AtomicBool::new(false));
  let (reading, has_read) = (Arc::clone(&newest), Arc::clone(&is_fresh));
  let mut window = MeterWindow::new((SAMPLE_RATE_HZ / 10) as usize);
  let input = match AudioInput::open(choice, move |samples| {
    if let Some(level) = window.push(samples) {
      reading.store(level.to_bits(), Ordering::Relaxed);
      has_read.store(true, Ordering::Relaxed);
    }
  }) {
    Ok(input) => input,
    Err(error) => {
      // Nobody waits any more only if the host is gone.
      let _ = opened.send(Err(error));
      return;
    },
  };
  let _ = opened.send(Ok(()));
  let mut last_fresh = Instant::now();
  while let Err(RecvTimeoutError::Timeout) = stop.recv_timeout(LEVEL_INTERVAL) {
    if is_fresh.swap(false, Ordering::Relaxed) {
      last_fresh = Instant::now();
    }
    let level = if last_fresh.elapsed() > STALE_LEVEL {
      0.0
    } else {
      f32::from_bits(newest.load(Ordering::Relaxed))
    };
    levels.publish(level);
  }
  drop(input);
  levels.publish(0.0);
}

/// A microphone that didn't open, as the UI reads it.
fn microphone_failure_wire(error: &AudioDeviceError) -> String {
  let kind = match error {
    AudioDeviceError::NoDevice(_) | AudioDeviceError::DeviceGone(_) => {
      FailureKind::Call(CallFailure::MicrophoneMissing)
    },
    _ => FailureKind::Other,
  };
  ActionFailure::new(kind, error.to_string()).to_wire()
}

#[cfg(test)]
mod tests {
  use std::sync::mpsc::Receiver;

  use teamcraft_media::video::{VideoFrame, VideoResolution};
  use teamcraft_media::video_capture::{CaptureError, CaptureFormat, FrameSender};

  use super::*;

  const FORMAT: CaptureFormat = CaptureFormat {
    resolution: VideoResolution {
      width: 320,
      height: 180,
    },
    framerate: 30,
  };
  const WAIT: Duration = Duration::from_secs(10);

  mod camera {
    use super::*;

    #[test]
    fn the_first_picture_makes_the_check_live_and_a_stop_is_no_failure() {
      let (states, state_rx) = std_mpsc::channel();
      let (filtered, filtered_rx) = std_mpsc::channel();
      let filter: FrameFilter = Box::new(move |frame| {
        let _ = filtered.send(frame.timestamp_us());
        frame
      });
      let (frames_tx, frames) = frame_queue();
      let check = spawn_pump(frames, filter, states, WAIT);
      assert!(frames_tx.send(gray_frame(1_000)));
      assert_eq!(filtered_rx.recv_timeout(WAIT).unwrap(), 1_000);
      assert_eq!(state_rx.recv_timeout(WAIT).unwrap(), PreviewCamera::Live);
      // Live once, not per picture.
      assert!(frames_tx.send(gray_frame(2_000)));
      assert_eq!(filtered_rx.recv_timeout(WAIT).unwrap(), 2_000);
      check.stop();
      assert!(state_rx.try_recv().is_err(), "a stop is no failure");
    }

    #[test]
    fn a_camera_without_pictures_fails_the_check() {
      let (frames_tx, frames) = frame_queue();
      let (states, state_rx) = std_mpsc::channel();
      let check = spawn_pump(frames, Box::new(|frame| frame), states, Duration::ZERO);
      assert_eq!(state_rx.recv_timeout(WAIT).unwrap(), PreviewCamera::Failed);
      drop(frames_tx);
      check.stop();
    }

    #[test]
    fn a_camera_that_stops_fails_the_check() {
      let (frames_tx, frames) = frame_queue();
      let (states, state_rx) = std_mpsc::channel();
      let check = spawn_pump(frames, Box::new(|frame| frame), states, WAIT);
      drop(frames_tx);
      assert_eq!(state_rx.recv_timeout(WAIT).unwrap(), PreviewCamera::Failed);
      check.stop();
    }

    #[test]
    fn a_capture_that_does_not_start_is_an_error() {
      let result = CameraCheck::start(Box::new(RefusingCapture), Box::new(|frame| frame), |_| {});
      let Err(wire) = result else {
        panic!("the check started");
      };
      assert_eq!(
        ActionFailure::from_wire(&wire).kind,
        FailureKind::Call(CallFailure::CameraDenied)
      );
    }

    /// A camera check around [`pump`] without a capture: the test feeds the queue.
    fn spawn_pump(
      frames: FrameReceiver,
      filter: FrameFilter,
      states: std_mpsc::Sender<PreviewCamera>,
      first_picture_timeout: Duration,
    ) -> CameraCheck {
      let (stop, stop_rx) = std_mpsc::channel();
      let thread = std::thread::spawn(move || {
        let on_state = move |state| {
          // The test may have stopped listening.
          let _ = states.send(state);
        };
        pump(&frames, filter, &stop_rx, &on_state, first_picture_timeout);
      });
      CameraCheck { stop, thread }
    }

    struct RefusingCapture;

    impl VideoCapture for RefusingCapture {
      fn format(&self) -> CaptureFormat {
        FORMAT
      }

      fn start(&mut self, _frames: FrameSender) -> Result<(), CaptureError> {
        Err(CaptureError::Denied("camera"))
      }

      fn stop(&mut self) {}
    }
  }

  mod levels {
    use super::*;

    #[test]
    fn readings_reach_the_watcher_until_it_unwatches() {
      let watch = LevelWatch::default();
      let received = watched(&watch, ChannelId(1));
      watch.publish(0.5);
      assert_eq!(received.try_recv().unwrap(), 0.5);
      watch.unwatch(ChannelId(1));
      watch.publish(0.7);
      assert!(received.try_recv().is_err());
    }

    #[test]
    fn a_late_unwatch_of_a_replaced_channel_keeps_the_newer_watcher() {
      let watch = LevelWatch::default();
      let _old = watched(&watch, ChannelId(1));
      let newer = watched(&watch, ChannelId(2));
      watch.unwatch(ChannelId(1));
      watch.publish(0.25);
      assert_eq!(newer.try_recv().unwrap(), 0.25);
    }

    #[test]
    fn a_watcher_that_takes_no_more_is_forgotten() {
      let watch = LevelWatch::default();
      watch.watch(ChannelId(1), |_| false);
      watch.publish(0.5);
      assert!(watch.lock().is_none());
    }

    #[test]
    fn a_missing_microphone_reads_as_missing() {
      let wire = microphone_failure_wire(&AudioDeviceError::NoDevice("input"));
      assert_eq!(
        ActionFailure::from_wire(&wire).kind,
        FailureKind::Call(CallFailure::MicrophoneMissing)
      );
      let other = microphone_failure_wire(&AudioDeviceError::AudioThread);
      assert_eq!(ActionFailure::from_wire(&other).kind, FailureKind::Other);
    }

    fn watched(watch: &LevelWatch, channel: ChannelId) -> Receiver<f32> {
      let (sent, received) = std_mpsc::channel();
      watch.watch(channel, move |level| sent.send(level).is_ok());
      received
    }
  }

  fn gray_frame(timestamp_us: u64) -> VideoFrame {
    VideoFrame::new(FORMAT.resolution, timestamp_us, vec![128; 320 * 180 * 3 / 2]).unwrap()
  }
}
