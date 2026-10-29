//! Audio level of a PCM frame for the RFC 6464 header extension, and the microphone meter of the
//! pre-join check (#442) and of the call's device menu (#986).
//!
//! The level is the frame's RMS in -dBov (0 = full-scale square wave, 127 = silence). The voice
//! activity flag uses a plain level threshold; a real voice activity detector (Opus' or the audio
//! processing module's) can replace it later without changing the wire format.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use web_time::Instant;

use crate::audio_frame::SAMPLE_RATE_HZ;
use crate::rtp::AudioLevel;

/// Frames louder than this (in -dBov, lower is louder) count as voice by default.
pub const DEFAULT_VOICE_THRESHOLD_DBOV: u8 = 50;
/// Quietest representable level (RFC 6464).
const SILENCE_DBOV: f64 = 127.0;
/// The meter's empty end: a quiet room is about -60 dBFS, speech at a laptop -35 to -15 dBFS.
const METER_FLOOR_DBFS: f64 = -60.0;

/// Averages captured samples over windows of a fixed length, so a meter reads a few levels a
/// second instead of one per audio callback.
#[derive(Debug, Clone)]
pub struct MeterWindow {
  window_samples: usize,
  sum_of_squares: f64,
  count: usize,
}

/// How many samples one reading of a call's meter averages: ten readings a second.
const CALL_METER_WINDOW: usize = (SAMPLE_RATE_HZ / 10) as usize;
/// A reading older than this is from a microphone that stopped delivering (unplugged, or the
/// call switching to another one): the meter shows silence instead.
const STALE_READING: Duration = Duration::from_millis(500);

/// The newest meter reading of a running call's microphone (0.0 to 1.0), for the call's device
/// menu (#986). The call's task writes it, the app reads it from anywhere.
#[derive(Clone, Debug, Default)]
pub struct SharedLevel(Arc<AtomicU32>);

/// Measures what a call captures from its microphone into a [`SharedLevel`].
#[derive(Debug)]
pub struct CallMeter {
  window: MeterWindow,
  maybe_last_reading: Option<Instant>,
  shared: SharedLevel,
}

/// Measures one frame of 16-bit PCM (any channel layout, interleaved samples are fine).
pub fn measure_audio_level(samples: &[i16], voice_threshold_dbov: u8) -> AudioLevel {
  let level_dbov = level_dbov(samples);
  // Invariant: `level_dbov` is clamped to 0..=127.
  AudioLevel::new(level_dbov, level_dbov < voice_threshold_dbov).unwrap_or(AudioLevel::SILENCE)
}

/// A level meter's reading of float samples (-1.0..=1.0): 0.0 at or below -60 dBFS RMS, 1.0 at
/// full scale, linear in decibels between, which is how loudness is heard.
pub fn meter_level(samples: &[f32]) -> f32 {
  if samples.is_empty() {
    return 0.0;
  }
  let sum_of_squares: f64 = samples.iter().map(|&sample| f64::from(sample).powi(2)).sum();
  meter_level_of_mean_square(sum_of_squares / samples.len() as f64)
}

impl MeterWindow {
  /// `window_samples`: how many samples one reading averages (4800 at 48 kHz: ten a second).
  pub fn new(window_samples: usize) -> Self {
    Self {
      window_samples: window_samples.max(1),
      sum_of_squares: 0.0,
      count: 0,
    }
  }

  /// Adds captured samples; returns a reading each time a window is full.
  pub fn push(&mut self, samples: &[f32]) -> Option<f32> {
    let mut maybe_reading = None;
    for &sample in samples {
      self.sum_of_squares += f64::from(sample).powi(2);
      self.count += 1;
      if self.count == self.window_samples {
        maybe_reading = Some(meter_level_of_mean_square(
          self.sum_of_squares / self.count as f64,
        ));
        self.sum_of_squares = 0.0;
        self.count = 0;
      }
    }
    maybe_reading
  }
}

impl SharedLevel {
  /// The newest reading; 0.0 before the first one.
  pub fn get(&self) -> f32 {
    f32::from_bits(self.0.load(Ordering::Relaxed))
  }

  fn set(&self, level: f32) {
    self.0.store(level.to_bits(), Ordering::Relaxed);
  }
}

impl CallMeter {
  pub fn new(shared: SharedLevel) -> Self {
    Self {
      window: MeterWindow::new(CALL_METER_WINDOW),
      maybe_last_reading: None,
      shared,
    }
  }

  /// Adds captured microphone samples; a full window becomes the newest reading.
  pub fn push(&mut self, samples: &[f32], now: Instant) {
    if let Some(level) = self.window.push(samples) {
      self.shared.set(level);
      self.maybe_last_reading = Some(now);
    }
  }

  /// Called now and then: a microphone that stopped delivering reads as silence, not as its last
  /// level.
  pub fn tick(&mut self, now: Instant) {
    if self
      .maybe_last_reading
      .is_some_and(|last| now.saturating_duration_since(last) > STALE_READING)
    {
      self.maybe_last_reading = None;
      self.shared.set(0.0);
    }
  }

  /// Another microphone took over: its readings start afresh, from silence.
  pub fn restart(&mut self) {
    self.window = MeterWindow::new(CALL_METER_WINDOW);
    self.maybe_last_reading = None;
    self.shared.set(0.0);
  }
}

fn meter_level_of_mean_square(mean_square: f64) -> f32 {
  if mean_square <= 0.0 {
    return 0.0;
  }
  // 10·log10 of the mean square is 20·log10 of the RMS.
  let dbfs = 10.0 * mean_square.log10();
  ((dbfs - METER_FLOOR_DBFS) / -METER_FLOOR_DBFS).clamp(0.0, 1.0) as f32
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

  mod meter {
    use super::*;

    #[test]
    fn silence_is_an_empty_meter_and_full_scale_a_full_one() {
      assert_eq!(meter_level(&[]), 0.0);
      assert_eq!(meter_level(&[0.0; 960]), 0.0);
      let square: Vec<f32> = (0..960).map(|i| if i % 2 == 0 { 1.0 } else { -1.0 }).collect();
      assert_eq!(meter_level(&square), 1.0);
    }

    #[test]
    fn the_meter_is_linear_in_decibels() {
      // A sine of amplitude 0.1 has an RMS of -23 dBFS: 37 of the meter's 60 dB.
      let speech = meter_level(&float_sine(0.1));
      assert!((speech - 37.0 / 60.0).abs() < 0.01, "{speech}");
      // Ten times quieter is 20 dB less.
      let quiet = meter_level(&float_sine(0.01));
      assert!((speech - quiet - 20.0 / 60.0).abs() < 0.01, "{quiet}");
      // Below the floor the meter stays empty.
      assert_eq!(meter_level(&float_sine(0.0001)), 0.0);
    }

    #[test]
    fn a_window_reads_once_it_is_full() {
      let mut window = MeterWindow::new(1920);
      assert_eq!(window.push(&float_sine(0.1)), None);
      let reading = window.push(&float_sine(0.1)).expect("a full window");
      assert!((reading - meter_level(&float_sine(0.1))).abs() < 0.01);
      // The next window starts empty: silence reads as silence.
      assert_eq!(window.push(&[0.0; 1920]), Some(0.0));
    }

    #[test]
    fn a_call_meter_shares_its_newest_reading() {
      let shared = SharedLevel::default();
      let mut meter = CallMeter::new(shared.clone());
      let start = Instant::now();
      assert_eq!(shared.get(), 0.0);
      // Five 20 ms frames make one reading (a tenth of a second).
      for _ in 0..4 {
        meter.push(&float_sine(0.1), start);
      }
      assert_eq!(shared.get(), 0.0);
      meter.push(&float_sine(0.1), start);
      assert!((shared.get() - meter_level(&float_sine(0.1))).abs() < 0.01);
    }

    #[test]
    fn a_call_meter_falls_silent_when_the_microphone_stops_or_changes() {
      let shared = SharedLevel::default();
      let mut meter = CallMeter::new(shared.clone());
      let start = Instant::now();
      for _ in 0..5 {
        meter.push(&float_sine(0.1), start);
      }
      let level = shared.get();
      assert!(level > 0.0);
      // Still fresh: the reading stays.
      meter.tick(start + Duration::from_millis(400));
      assert_eq!(shared.get(), level);
      // Nothing came for longer than a reading lives: silence.
      meter.tick(start + Duration::from_millis(600));
      assert_eq!(shared.get(), 0.0);
      // A switch to another microphone starts from silence, and half a window of the old one
      // doesn't count towards the new one's first reading.
      for _ in 0..8 {
        meter.push(&float_sine(0.5), start);
      }
      assert!(shared.get() > 0.0);
      meter.restart();
      assert_eq!(shared.get(), 0.0);
      // Two silent frames would complete the old window (three loud ones) into a loud reading.
      for _ in 0..2 {
        meter.push(&[0.0; 960], start);
      }
      assert_eq!(shared.get(), 0.0);
    }
  }

  /// One 20 ms frame of a 440 Hz sine at 48 kHz, as floats (1.0 = full scale).
  fn float_sine(amplitude: f32) -> Vec<f32> {
    (0..960)
      .map(|i| amplitude * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48_000.0).sin())
      .collect()
  }

  /// One 20 ms frame of a 440 Hz sine at 48 kHz with the given amplitude (1.0 = full scale).
  fn sine(amplitude: f64) -> Vec<i16> {
    (0..960)
      .map(|i| (amplitude * f64::from(i16::MAX) * (2.0 * PI * 440.0 * f64::from(i) / 48_000.0).sin()) as i16)
      .collect()
  }
}
