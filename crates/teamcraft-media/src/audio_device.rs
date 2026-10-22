//! Microphone capture and speaker playback with cpal, as mono 48 kHz f32.
//!
//! Devices are opened at 48 kHz (PipeWire, PulseAudio, CoreAudio and WASAPI resample when needed);
//! multi-channel devices are downmixed on capture and get the mono signal on every channel on
//! playback. The capture callback hands samples to the caller; playback pulls from a shared
//! [`PlaybackBuffer`] and plays silence when it runs dry.
//!
//! cpal streams are not `Send` on every platform: keep [`AudioInput`] and [`AudioOutput`] on the
//! thread that opened them and drop them to stop.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig, SupportedStreamConfigRange};
use thiserror::Error;
use tracing::warn;

use crate::audio_codec::SAMPLE_RATE_HZ;

/// A running capture stream.
pub struct AudioInput {
  _stream: cpal::Stream,
}

/// A running playback stream.
pub struct AudioOutput {
  _stream: cpal::Stream,
}

/// Samples waiting for the speakers, filled by the call engine and drained by the output callback.
#[derive(Debug)]
pub struct PlaybackBuffer {
  state: Mutex<PlaybackState>,
  max_samples: usize,
}

#[derive(Debug, Default)]
struct PlaybackState {
  samples: VecDeque<f32>,
  underruns: u64,
}

#[derive(Debug, Error)]
pub enum AudioDeviceError {
  #[error("no {0} device available")]
  NoDevice(&'static str),
  #[error("the {0} device supports no f32 configuration at 48 kHz")]
  UnsupportedFormat(&'static str),
  #[error("audio device: {0}")]
  Device(#[from] cpal::Error),
}

impl AudioInput {
  /// Opens the default microphone; `on_samples` gets mono 48 kHz samples on the audio thread.
  pub fn open_default(mut on_samples: impl FnMut(&[f32]) + Send + 'static) -> Result<Self, AudioDeviceError> {
    let device = cpal::default_host()
      .default_input_device()
      .ok_or(AudioDeviceError::NoDevice("input"))?;
    let config = pick_config(device.supported_input_configs()?, "input")?;
    let channels = usize::from(config.channels);
    let mut mono = Vec::new();
    let stream = device.build_input_stream(
      config,
      move |data: &[f32], _: &cpal::InputCallbackInfo| {
        mono.clear();
        mono.extend(
          data
            .chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32),
        );
        on_samples(&mono);
      },
      |error| warn!(%error, "capture stream error"),
      None,
    )?;
    stream.play()?;
    Ok(Self { _stream: stream })
  }
}

impl AudioOutput {
  /// Opens the default speakers, playing what is pushed into `buffer`.
  pub fn open_default(buffer: Arc<PlaybackBuffer>) -> Result<Self, AudioDeviceError> {
    let device = cpal::default_host()
      .default_output_device()
      .ok_or(AudioDeviceError::NoDevice("output"))?;
    let config = pick_config(device.supported_output_configs()?, "output")?;
    let channels = usize::from(config.channels);
    let mut mono = Vec::new();
    let stream = device.build_output_stream(
      config,
      move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
        mono.resize(data.len() / channels, 0.0);
        buffer.pull(&mut mono);
        for (frame, sample) in data.chunks_mut(channels).zip(&mono) {
          frame.fill(*sample);
        }
      },
      |error| warn!(%error, "playback stream error"),
      None,
    )?;
    stream.play()?;
    Ok(Self { _stream: stream })
  }
}

impl PlaybackBuffer {
  /// `max_samples` bounds the latency: older samples are dropped when the engine runs ahead.
  pub fn new(max_samples: usize) -> Self {
    Self {
      state: Mutex::new(PlaybackState::default()),
      max_samples,
    }
  }

  pub fn push(&self, samples: &[f32]) {
    let mut state = self.lock();
    state.samples.extend(samples);
    let excess = state.samples.len().saturating_sub(self.max_samples);
    state.samples.drain(..excess);
  }

  /// Fills `out`; missing samples are silence and count as an underrun.
  pub fn pull(&self, out: &mut [f32]) {
    let mut state = self.lock();
    let available = state.samples.len().min(out.len());
    for (slot, sample) in out.iter_mut().zip(state.samples.drain(..available)) {
      *slot = sample;
    }
    if available < out.len() {
      out[available..].fill(0.0);
      state.underruns += 1;
    }
  }

  pub fn len(&self) -> usize {
    self.lock().samples.len()
  }

  pub fn is_empty(&self) -> bool {
    self.len() == 0
  }

  pub fn underruns(&self) -> u64 {
    self.lock().underruns
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, PlaybackState> {
    // Poisoning only means a callback panicked mid-update of a plain sample queue.
    self.state.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

/// A 48 kHz f32 configuration, preferring mono.
fn pick_config(
  ranges: impl Iterator<Item = SupportedStreamConfigRange>,
  direction: &'static str,
) -> Result<StreamConfig, AudioDeviceError> {
  let mut candidates: Vec<SupportedStreamConfigRange> = ranges
    .filter(|range| range.sample_format() == SampleFormat::F32)
    .filter(|range| range.min_sample_rate() <= SAMPLE_RATE_HZ && SAMPLE_RATE_HZ <= range.max_sample_rate())
    .collect();
  candidates.sort_by_key(|range| range.channels());
  let range = candidates
    .into_iter()
    .next()
    .ok_or(AudioDeviceError::UnsupportedFormat(direction))?;
  Ok(StreamConfig {
    channels: range.channels(),
    sample_rate: SAMPLE_RATE_HZ,
    buffer_size: cpal::BufferSize::Default,
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn playback_buffer_pads_underruns_and_bounds_latency() {
    let buffer = PlaybackBuffer::new(4);
    buffer.push(&[0.1, 0.2, 0.3, 0.4, 0.5, 0.6]);
    assert_eq!(buffer.len(), 4, "oldest samples dropped");
    let mut out = [9.0; 6];
    buffer.pull(&mut out);
    assert_eq!(out, [0.3, 0.4, 0.5, 0.6, 0.0, 0.0]);
    assert_eq!(buffer.underruns(), 1);
    assert!(buffer.is_empty());
  }
}
