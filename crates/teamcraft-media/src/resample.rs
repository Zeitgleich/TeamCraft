//! Sample rate conversion to and from the codec's 48 kHz, for audio that runs at another rate:
//! native devices and system audio captures (44.1 kHz, 96 kHz, ...), and the browser call's
//! audio graph where the browser connects a microphone only at its device's rate (Firefox before
//! 148, #1721).

use crate::audio_frame::SAMPLE_RATE_HZ;

/// Turns a capture's native format (interleaved channels at any rate) into mono at another rate
/// (48 kHz unless said otherwise), linearly.
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

impl MonoResampler {
  /// A rate of 0 (a broken device report) counts as the codec's rate instead of never advancing.
  pub fn new(channels: usize, rate_hz: u32) -> Self {
    Self::between(channels, rate_hz, SAMPLE_RATE_HZ)
  }

  /// From `from_hz` to `to_hz` instead of to the codec's rate (playback on a 44.1 or 96 kHz
  /// device). A rate of 0 counts as the codec's rate.
  pub fn between(channels: usize, from_hz: u32, to_hz: u32) -> Self {
    let or_codec = |rate_hz: u32| if rate_hz == 0 { SAMPLE_RATE_HZ } else { rate_hz };
    Self {
      channels: channels.max(1),
      step: f64::from(or_codec(from_hz)) / f64::from(or_codec(to_hz)),
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_zero_rate_does_not_hang() {
    let mut resampler = MonoResampler::new(1, 0);
    assert_close(&resampler.process(&[0.5, -0.5]), &[0.5, -0.5]);
  }

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
  fn the_codec_rate_goes_back_to_a_device_rate() {
    // The browser call's playback on a 44.1 kHz graph (#1721): 20 ms frames of 960 samples come
    // out as 20 ms at 44.1 kHz, one second in all.
    let mut resampler = MonoResampler::between(1, SAMPLE_RATE_HZ, 44_100);
    let total: usize = (0..50).map(|_| resampler.process(&[0.25; 960]).len()).sum();
    assert!((44_099..=44_101).contains(&total), "{total} samples");
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
