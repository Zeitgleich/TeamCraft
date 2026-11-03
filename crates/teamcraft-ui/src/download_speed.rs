//! Speed and time left of downloads in the downloads list, from the bytes each read of the list
//! shows. The core reports bytes, not speed; a smoothed rate keeps the number from jumping with
//! every read, as browsers do.

use std::collections::HashMap;

/// Reads closer together than this don't update the rate: too little time to measure.
const MIN_SAMPLE_MS: f64 = 250.0;
/// Weight of the newest measurement in the smoothed rate.
const NEW_WEIGHT: f64 = 0.3;

/// The rates of the downloads in flight, by file.
#[derive(Debug, Default)]
pub struct SpeedMeter {
  files: HashMap<String, Sample>,
}

#[derive(Debug, Clone, Copy)]
struct Sample {
  at_ms: f64,
  done_bytes: u64,
  maybe_bytes_per_second: Option<f64>,
}

impl SpeedMeter {
  /// Records that `file` had `done_bytes` at `now_ms`; returns its smoothed rate in bytes per
  /// second once there are two readings far enough apart.
  pub fn measure(&mut self, file: &str, done_bytes: u64, now_ms: f64) -> Option<f64> {
    let Some(sample) = self.files.get_mut(file) else {
      self.files.insert(
        file.to_owned(),
        Sample {
          at_ms: now_ms,
          done_bytes,
          maybe_bytes_per_second: None,
        },
      );
      return None;
    };
    let elapsed_ms = now_ms - sample.at_ms;
    if done_bytes < sample.done_bytes {
      // Started over (the storage ignored the range): measure afresh.
      *sample = Sample {
        at_ms: now_ms,
        done_bytes,
        maybe_bytes_per_second: None,
      };
      return None;
    }
    if elapsed_ms < MIN_SAMPLE_MS {
      return sample.maybe_bytes_per_second;
    }
    let rate = (done_bytes - sample.done_bytes) as f64 * 1000.0 / elapsed_ms;
    let smoothed = sample
      .maybe_bytes_per_second
      .map_or(rate, |previous| previous + NEW_WEIGHT * (rate - previous));
    *sample = Sample {
      at_ms: now_ms,
      done_bytes,
      maybe_bytes_per_second: Some(smoothed),
    };
    Some(smoothed)
  }

  /// Forgets the files not in `keep` (finished, paused or gone).
  pub fn retain(&mut self, keep: impl Fn(&str) -> bool) {
    self.files.retain(|file, _| keep(file));
  }
}

/// Whole seconds left at `bytes_per_second`, `None` without a rate to go by.
pub fn seconds_left(done_bytes: u64, total_bytes: u64, bytes_per_second: f64) -> Option<u64> {
  if bytes_per_second < 1.0 {
    return None;
  }
  let left = total_bytes.saturating_sub(done_bytes) as f64 / bytes_per_second;
  Some(left.ceil() as u64)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_rate_needs_two_readings_and_is_smoothed() {
    let mut meter = SpeedMeter::default();
    assert_eq!(meter.measure("a", 0, 0.0), None);
    assert_eq!(meter.measure("a", 1_000_000, 1_000.0), Some(1_000_000.0));
    // Twice as fast for a second: moves 30 % of the way.
    let smoothed = meter.measure("a", 3_000_000, 2_000.0).unwrap();
    assert!((smoothed - 1_300_000.0).abs() < 1.0, "{smoothed}");
  }

  #[test]
  fn readings_too_close_together_keep_the_rate() {
    let mut meter = SpeedMeter::default();
    meter.measure("a", 0, 0.0);
    meter.measure("a", 1_000, 1_000.0);
    assert_eq!(meter.measure("a", 5_000, 1_100.0), Some(1_000.0));
  }

  #[test]
  fn a_download_that_starts_over_is_measured_afresh() {
    let mut meter = SpeedMeter::default();
    meter.measure("a", 0, 0.0);
    meter.measure("a", 5_000, 1_000.0);
    assert_eq!(meter.measure("a", 100, 2_000.0), None);
    meter.retain(|_| false);
    assert_eq!(meter.measure("a", 200, 3_000.0), None);
  }

  #[test]
  fn time_left_rounds_up_and_needs_a_rate() {
    assert_eq!(seconds_left(0, 10_000, 3_000.0), Some(4));
    assert_eq!(seconds_left(10_000, 10_000, 3_000.0), Some(0));
    assert_eq!(seconds_left(0, 10_000, 0.0), None);
  }
}
