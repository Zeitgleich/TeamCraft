//! Audio level of a PCM frame for the RFC 6464 header extension.
//!
//! The level is the frame's RMS in -dBov (0 = full-scale square wave, 127 = silence). The voice
//! activity flag uses a plain level threshold; a real voice activity detector (Opus' or the audio
//! processing module's) can replace it later without changing the wire format.

use crate::rtp::AudioLevel;

/// Frames louder than this (in -dBov, lower is louder) count as voice by default.
pub const DEFAULT_VOICE_THRESHOLD_DBOV: u8 = 50;
/// Quietest representable level (RFC 6464).
const SILENCE_DBOV: f64 = 127.0;

/// Measures one frame of 16-bit PCM (any channel layout, interleaved samples are fine).
pub fn measure_audio_level(samples: &[i16], voice_threshold_dbov: u8) -> AudioLevel {
  let level_dbov = level_dbov(samples);
  // Invariant: `level_dbov` is clamped to 0..=127.
  AudioLevel::new(level_dbov, level_dbov < voice_threshold_dbov).unwrap_or(AudioLevel::SILENCE)
}

fn level_dbov(samples: &[i16]) -> u8 {
  if samples.is_empty() {
    return SILENCE_DBOV as u8;
  }
  let sum_of_squares: f64 = samples.iter().map(|&sample| f64::from(sample).powi(2)).sum();
  let rms = (sum_of_squares / samples.len() as f64).sqrt() / f64::from(i16::MAX);
  if rms <= 0.0 {
    return SILENCE_DBOV as u8;
  }
  let dbov = (-20.0 * rms.log10()).round().clamp(0.0, SILENCE_DBOV);
  // Invariant: clamped to 0..=127 above, so the cast is lossless.
  dbov as u8
}

#[cfg(test)]
mod tests {
  use std::f64::consts::PI;

  use super::*;

  #[test]
  fn silence_is_127_without_voice() {
    let level = measure_audio_level(&[0; 960], DEFAULT_VOICE_THRESHOLD_DBOV);
    assert_eq!(level, AudioLevel::SILENCE);
    assert_eq!(
      measure_audio_level(&[], DEFAULT_VOICE_THRESHOLD_DBOV),
      AudioLevel::SILENCE
    );
  }

  #[test]
  fn full_scale_square_wave_is_0_dbov() {
    let square: Vec<i16> = (0..960)
      .map(|i| if i % 2 == 0 { i16::MAX } else { -i16::MAX })
      .collect();
    let level = measure_audio_level(&square, DEFAULT_VOICE_THRESHOLD_DBOV);
    assert_eq!(level.level_dbov(), 0);
    assert!(level.has_voice_activity);
  }

  #[test]
  fn full_scale_sine_is_3_dbov_and_halving_adds_6() {
    assert_eq!(
      measure_audio_level(&sine(1.0), DEFAULT_VOICE_THRESHOLD_DBOV).level_dbov(),
      3
    );
    assert_eq!(
      measure_audio_level(&sine(0.5), DEFAULT_VOICE_THRESHOLD_DBOV).level_dbov(),
      9
    );
  }

  #[test]
  fn quiet_noise_is_not_voice() {
    let quiet = measure_audio_level(&sine(0.001), DEFAULT_VOICE_THRESHOLD_DBOV);
    assert_eq!(quiet.level_dbov(), 63);
    assert!(!quiet.has_voice_activity);
  }

  /// One 20 ms frame of a 440 Hz sine at 48 kHz with the given amplitude (1.0 = full scale).
  fn sine(amplitude: f64) -> Vec<i16> {
    (0..960)
      .map(|i| (amplitude * f64::from(i16::MAX) * (2.0 * PI * 440.0 * f64::from(i) / 48_000.0).sin()) as i16)
      .collect()
  }
}
