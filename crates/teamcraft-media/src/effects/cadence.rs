//! How often the segmenter runs. Natively it runs on every frame. In the browser it runs in a
//! worker of its own (#439), which shares the device's CPU with the page and the encoders: a slow
//! segmentation (about 5 ms in WebAssembly with SIMD for a 256x144 model, more on slower machines)
//! runs only every second or third frame there, so that its share stays within a budget, and the
//! last mask serves the frames in between. The compositor smooths masks over time anyway, so a
//! reused mask doesn't show.

use std::time::Duration;

/// The longest run of frames that reuse one mask: beyond it a moving person outruns the mask.
const MAX_INTERVAL: u8 = 4;
/// Weight of the newest measurement in the running average of the segmentation's cost.
const COST_WEIGHT: f64 = 0.25;

/// When the next mask is due.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaskCadence {
  /// The segmentation time per frame the cadence aims for; `None`: every frame.
  maybe_budget: Option<Duration>,
  /// Average cost of one segmentation, in seconds.
  average_cost: f64,
  /// A new mask every this many frames.
  interval: u8,
  frames_since_mask: u8,
}

impl MaskCadence {
  /// A new mask for every frame.
  pub const EVERY_FRAME: Self = Self {
    maybe_budget: None,
    average_cost: 0.0,
    interval: 1,
    frames_since_mask: 0,
  };

  /// As often as keeps the segmentation within `budget` per frame on average, but at least every
  /// fourth frame.
  pub fn within(budget: Duration) -> Self {
    Self {
      maybe_budget: Some(budget),
      ..Self::EVERY_FRAME
    }
  }

  /// A new mask every this many frames, as of the last measurement.
  pub fn interval(&self) -> u8 {
    self.interval
  }

  /// Whether this frame gets a new mask; call once per frame that has one to reuse.
  pub fn is_due(&mut self) -> bool {
    self.frames_since_mask = self.frames_since_mask.saturating_add(1);
    self.frames_since_mask >= self.interval
  }

  /// A segmentation took `cost`: the interval follows its running average.
  pub fn observe(&mut self, cost: Duration) {
    self.frames_since_mask = 0;
    let Some(budget) = self.maybe_budget else {
      return;
    };
    let cost = cost.as_secs_f64();
    self.average_cost = if self.average_cost == 0.0 {
      cost
    } else {
      self.average_cost + (cost - self.average_cost) * COST_WEIGHT
    };
    let needed = (self.average_cost / budget.as_secs_f64().max(f64::EPSILON)).ceil();
    self.interval = needed.clamp(1.0, f64::from(MAX_INTERVAL)) as u8;
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn natively_every_frame_gets_a_mask() {
    let mut cadence = MaskCadence::EVERY_FRAME;
    cadence.observe(Duration::from_millis(40));
    assert!(!due_pattern(&mut cadence, 5, Duration::from_millis(40)).contains(&false));
    assert_eq!(cadence.interval(), 1);
  }

  #[test]
  fn a_slow_segmenter_runs_every_few_frames_within_the_budget() {
    let mut cadence = MaskCadence::within(Duration::from_millis(6));
    cadence.observe(Duration::from_millis(12));
    assert_eq!(cadence.interval(), 2);
    assert_eq!(
      due_pattern(&mut cadence, 4, Duration::from_millis(12)),
      [false, true, false, true]
    );
    // Far too slow: still a new mask every fourth frame.
    let mut slow = MaskCadence::within(Duration::from_millis(6));
    slow.observe(Duration::from_millis(100));
    assert_eq!(slow.interval(), 4);
    assert_eq!(
      due_pattern(&mut slow, 5, Duration::from_millis(100)),
      [false, false, false, true, false]
    );
  }

  #[test]
  fn the_interval_follows_the_running_average() {
    let mut cadence = MaskCadence::within(Duration::from_millis(6));
    cadence.observe(Duration::from_millis(5));
    assert_eq!(cadence.interval(), 1);
    // One slow frame (a garbage collection) moves the average by a quarter of the difference.
    cadence.observe(Duration::from_millis(13));
    assert_eq!(cadence.interval(), 2);
    for _ in 0..10 {
      cadence.observe(Duration::from_millis(4));
    }
    assert_eq!(cadence.interval(), 1);
  }

  /// Which of the next `frames` get a new mask, each segmentation taking `cost`.
  fn due_pattern(cadence: &mut MaskCadence, frames: usize, cost: Duration) -> Vec<bool> {
    (0..frames)
      .map(|_| {
        let is_due = cadence.is_due();
        if is_due {
          cadence.observe(cost);
        }
        is_due
      })
      .collect()
  }
}
