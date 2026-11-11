//! The sound the computer plays (system audio), for screen shares with sound.
//!
//! Platform captures (PipeWire on Linux, WASAPI process loopback on Windows, ScreenCaptureKit on
//! macOS) run on their own threads and hand mono 48 kHz samples to a bounded queue; the call
//! driver encodes them as a second Opus stream. Every capture leaves out this app's own playback,
//! so the other participants never hear themselves.

use thiserror::Error;
use tokio::sync::mpsc;

pub use crate::resample::MonoResampler;

/// Chunks waiting for the encoder (a chunk is typically 10–20 ms); more are dropped.
const SYSTEM_AUDIO_QUEUE_CHUNKS: usize = 50;

/// A source of system audio.
pub trait SystemAudioCapture: Send {
  /// Starts delivering samples into `samples` until [`SystemAudioCapture::stop`] or drop.
  fn start(&mut self, samples: SystemAudioSender) -> Result<(), SystemAudioError>;
  fn stop(&mut self);
}

/// The capture side of the sample queue; drops chunks while the consumer is busy.
#[derive(Debug, Clone)]
pub struct SystemAudioSender {
  sender: mpsc::Sender<Vec<f32>>,
}

/// The receiving end of [`system_audio_queue`].
pub type SystemAudioReceiver = mpsc::Receiver<Vec<f32>>;

/// Platforms without a system audio capture: starting always fails with
/// [`SystemAudioError::Unsupported`].
#[derive(Debug, Default)]
pub struct UnsupportedSystemAudio;

/// A fake for tests: samples are pushed by hand.
#[derive(Debug, Default)]
pub struct ManualSystemAudio {
  maybe_samples: Option<SystemAudioSender>,
}

#[derive(Debug, Error)]
pub enum SystemAudioError {
  #[error("sharing the computer's sound is not supported here")]
  Unsupported,
  #[error("the user or the system denied capturing the computer's sound")]
  Denied,
  #[error("system audio: {0}")]
  Device(String),
}

/// A bounded queue between a system audio capture and the call driver.
pub fn system_audio_queue() -> (SystemAudioSender, SystemAudioReceiver) {
  let (sender, receiver) = mpsc::channel(SYSTEM_AUDIO_QUEUE_CHUNKS);
  (SystemAudioSender { sender }, receiver)
}

impl SystemAudioSender {
  /// Queues mono 48 kHz samples; `false` once the consumer is gone (stop capturing then).
  pub fn send(&self, samples: Vec<f32>) -> bool {
    match self.sender.try_send(samples) {
      Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => true,
      Err(mpsc::error::TrySendError::Closed(_)) => false,
    }
  }
}

impl ManualSystemAudio {
  /// Delivers `samples` if started; `false` otherwise or when the consumer is gone.
  pub fn push(&self, samples: Vec<f32>) -> bool {
    self
      .maybe_samples
      .as_ref()
      .is_some_and(|sender| sender.send(samples))
  }

  /// A handle that pushes samples from elsewhere after `start`.
  pub fn sender(&self) -> Option<SystemAudioSender> {
    self.maybe_samples.clone()
  }
}

impl SystemAudioCapture for UnsupportedSystemAudio {
  fn start(&mut self, _samples: SystemAudioSender) -> Result<(), SystemAudioError> {
    Err(SystemAudioError::Unsupported)
  }

  fn stop(&mut self) {}
}

impl SystemAudioCapture for ManualSystemAudio {
  fn start(&mut self, samples: SystemAudioSender) -> Result<(), SystemAudioError> {
    self.maybe_samples = Some(samples);
    Ok(())
  }

  fn stop(&mut self) {
    self.maybe_samples = None;
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_full_queue_drops_and_a_closed_one_stops_the_capture() {
    let (sender, mut receiver) = system_audio_queue();
    for _ in 0..SYSTEM_AUDIO_QUEUE_CHUNKS + 5 {
      assert!(sender.send(vec![0.0; 480]));
    }
    let mut queued = 0;
    while receiver.try_recv().is_ok() {
      queued += 1;
    }
    assert_eq!(queued, SYSTEM_AUDIO_QUEUE_CHUNKS);
    drop(receiver);
    assert!(!sender.send(vec![0.0; 480]));
  }
}
