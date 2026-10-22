//! The sound the computer plays (system audio), for screen shares with sound.
//!
//! Platform captures (PipeWire on Linux, WASAPI process loopback on Windows, ScreenCaptureKit on
//! macOS) run on their own threads and hand mono 48 kHz samples to a bounded queue; the call
//! driver encodes them as a second Opus stream. Every capture leaves out this app's own playback,
//! so the other participants never hear themselves.

use thiserror::Error;
use tokio::sync::mpsc;

use crate::audio_frame::SAMPLE_RATE_HZ;

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

/// Turns a capture's native format (interleaved channels at any rate) into mono 48 kHz.
#[derive(Debug, Clone)]
pub struct MonoResampler {
  channels: usize,
  /// Input samples per output sample.
  step: f64,
  /// Position of the next output sample, in input samples after `previous`.
  position: f64,
  /// The last input sample of the previous chunk, for interpolation across chunks.
  previous: f32,
}

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

impl MonoResampler {
  pub fn new(channels: usize, rate_hz: u32) -> Self {
    Self {
      channels: channels.max(1),
      step: f64::from(rate_hz) / f64::from(SAMPLE_RATE_HZ),
      position: 0.0,
      previous: 0.0,
    }
  }

  /// Mixes the channels down and resamples linearly; keeps its phase across chunks.
  pub fn process(&mut self, interleaved: &[f32]) -> Vec<f32> {
    let mono: Vec<f32> = interleaved
      .chunks(self.channels)
      .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
      .collect();
    if (self.step - 1.0).abs() < f64::EPSILON {
      return mono;
    }
    let mut out = Vec::with_capacity((mono.len() as f64 / self.step) as usize + 1);
    // Input index -1 is `previous`, 0.. are this chunk's samples.
    while self.position < mono.len() as f64 {
      let index = self.position.floor();
      let fraction = (self.position - index) as f32;
      let before = match index as isize - 1 {
        -1 => self.previous,
        at => mono[at as usize],
      };
      let after = mono[index as usize];
      out.push(before + (after - before) * fraction);
      self.position += self.step;
    }
    self.position -= mono.len() as f64;
    if let Some(&last) = mono.last() {
      self.previous = last;
    }
    out
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

  mod resampler {
    use super::*;

    #[test]
    fn stereo_48k_is_averaged_only() {
      let mut resampler = MonoResampler::new(2, 48_000);
      assert_close(&resampler.process(&[0.2, 0.4, -1.0, 1.0]), &[0.3, 0.0]);
    }

    #[test]
    fn rates_are_converted_across_chunks() {
      let mut resampler = MonoResampler::new(1, 44_100);
      // One second in uneven chunks becomes one second at 48 kHz, give or take a sample.
      let input: Vec<f32> = (0..44_100).map(|n| (n as f32 / 100.0).sin()).collect();
      let total: usize = input
        .chunks(441)
        .map(|chunk| resampler.process(chunk).len())
        .sum();
      assert!((47_999..=48_001).contains(&total), "{total} samples");
    }

    #[test]
    fn a_ramp_stays_a_ramp() {
      let mut resampler = MonoResampler::new(1, 24_000);
      let out: Vec<f32> = [0.0, 0.5, 1.0]
        .chunks(1)
        .flat_map(|chunk| resampler.process(chunk))
        .collect();
      // Twice the samples with halfway values in between, one input sample late (the start
      // interpolates from silence).
      assert_close(&out, &[0.0, 0.0, 0.0, 0.25, 0.5, 0.75]);
    }

    fn assert_close(actual: &[f32], expected: &[f32]) {
      assert_eq!(actual.len(), expected.len(), "{actual:?}");
      for (a, e) in actual.iter().zip(expected) {
        assert!((a - e).abs() < 1e-6, "{actual:?} != {expected:?}");
      }
    }
  }

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
