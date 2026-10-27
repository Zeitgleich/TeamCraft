//! The gesture math every gesture of the app shares, after Apple's *Designing Fluid Interfaces*:
//! the velocity a finger had when it lifted, where that momentum would come to rest (projection)
//! and how a limit resists (rubber band, with UIScrollView's constant).
//!
//! The phone sheet and edge swipe ([`super`]) and the lightbox (`crate::lightbox::physics`) build
//! on it and keep their own thresholds. Pure, with injected timestamps (milliseconds, the clock of
//! `Event.timeStamp` and `performance.now()`); velocities are in pixels per millisecond.

use std::collections::VecDeque;

/// Deceleration per millisecond of a coasting scroll view (UIScrollView's `normal` rate).
pub const SCROLL_DECELERATION: f64 = 0.998;
/// How strongly a limit resists: the classic UIScrollView constant.
pub const RUBBERBAND_CONSTANT: f64 = 0.55;
/// Samples older than this don't count for the release velocity: a finger that stopped before
/// lifting has no momentum.
pub const VELOCITY_WINDOW_MS: f64 = 100.0;

/// Positions of a moving finger along one axis, for its velocity at the end.
#[derive(Clone, Debug, Default)]
pub struct AxisTracker {
  samples: VecDeque<(f64, f64)>,
}

impl AxisTracker {
  /// Starts over at the press.
  pub fn reset(&mut self, at_ms: f64, position: f64) {
    self.samples.clear();
    self.samples.push_back((at_ms, position));
  }

  /// Forgets the samples: a pinch's fingers say nothing about a pan's velocity.
  pub fn clear(&mut self) {
    self.samples.clear();
  }

  pub fn push(&mut self, at_ms: f64, position: f64) {
    self.samples.push_back((at_ms, position));
    // The newest two stay however old they are, so a slow movement still has a velocity.
    while self
      .samples
      .front()
      .is_some_and(|(sampled, _)| at_ms - sampled > VELOCITY_WINDOW_MS)
      && self.samples.len() > 2
    {
      self.samples.pop_front();
    }
  }

  /// The velocity of the recent samples; 0 without two of them.
  pub fn velocity(&self) -> f64 {
    let (Some(&(first_ms, first)), Some(&(last_ms, last))) = (self.samples.front(), self.samples.back())
    else {
      return 0.0;
    };
    let elapsed = last_ms - first_ms;
    if elapsed <= 0.0 {
      return 0.0;
    }
    (last - first) / elapsed
  }

  /// The velocity at `at_ms` (the release): 0 when the pointer rested for the whole window before
  /// letting go.
  pub fn velocity_at(&self, at_ms: f64) -> f64 {
    match self.samples.back() {
      Some(&(last_ms, _)) if at_ms - last_ms <= VELOCITY_WINDOW_MS => self.velocity(),
      _ => 0.0,
    }
  }
}

/// How far momentum carries a release at `velocity` (px/ms) with `deceleration` per millisecond:
/// Apple's `v/1000 · d/(1−d)` with the velocity in px/s.
pub fn project(velocity: f64, deceleration: f64) -> f64 {
  velocity * deceleration / (1.0 - deceleration)
}

/// How far something dragged `overshoot` past a limit moves: less and less the further it goes, and
/// never more than `dimension`.
pub fn rubberband(overshoot: f64, dimension: f64, constant: f64) -> f64 {
  if dimension <= 0.0 {
    return 0.0;
  }
  overshoot * dimension * constant / (dimension + constant * overshoot.abs())
}

/// The overshoot that [`rubberband`] turns into `shown`: to pick a resisted value up again where
/// it is on screen.
pub fn unrubberband(shown: f64, dimension: f64, constant: f64) -> f64 {
  if dimension <= 0.0 {
    return 0.0;
  }
  // Just short of the asymptote, so a value at the limit maps to a large but finite overshoot.
  let shown = shown.clamp(-dimension * 0.999, dimension * 0.999);
  shown * dimension / (constant * (dimension - shown.abs()))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn projection_follows_apples_formula() {
    // 1000 px/s at 0.99: 1 · 0.99 / 0.01 = 99 px.
    assert!((project(1.0, 0.99) - 99.0).abs() < 1e-9);
    assert!((project(-2.0, SCROLL_DECELERATION) + 998.0).abs() < 1e-6);
    assert_eq!(project(0.0, 0.99), 0.0);
  }

  #[test]
  fn velocity_comes_from_the_recent_samples() {
    let mut tracker = AxisTracker::default();
    tracker.reset(0.0, 0.0);
    // Slow at first, then fast: only the last 100 ms count.
    tracker.push(200.0, 10.0);
    tracker.push(250.0, 40.0);
    tracker.push(300.0, 90.0);
    assert!((tracker.velocity() - 0.8).abs() < 1e-9);
    assert!((tracker.velocity_at(300.0) - 0.8).abs() < 1e-9);
  }

  #[test]
  fn a_pointer_that_rested_before_release_has_no_velocity() {
    let mut tracker = AxisTracker::default();
    tracker.reset(0.0, 0.0);
    tracker.push(16.0, 50.0);
    assert_eq!(tracker.velocity_at(500.0), 0.0);
    assert_eq!(AxisTracker::default().velocity_at(0.0), 0.0);
    assert_eq!(AxisTracker::default().velocity(), 0.0);
  }

  #[test]
  fn a_slow_movement_keeps_its_last_two_samples() {
    let mut tracker = AxisTracker::default();
    tracker.reset(0.0, 0.0);
    tracker.push(500.0, 50.0);
    assert!((tracker.velocity() - 0.1).abs() < 1e-9);
    tracker.clear();
    assert_eq!(tracker.velocity(), 0.0);
  }

  #[test]
  fn a_limit_resists_more_and_more_but_never_past_the_dimension() {
    let small = rubberband(10.0, 1000.0, RUBBERBAND_CONSTANT);
    let huge = rubberband(1e9, 1000.0, RUBBERBAND_CONSTANT);
    assert!(small > 5.0 && small < 10.0, "{small}");
    assert!(huge < 1000.0, "{huge}");
    assert_eq!(
      rubberband(-10.0, 1000.0, RUBBERBAND_CONSTANT),
      -small,
      "symmetric"
    );
    assert_eq!(
      rubberband(10.0, 0.0, RUBBERBAND_CONSTANT),
      0.0,
      "no room, no movement"
    );
    let back = unrubberband(small, 1000.0, RUBBERBAND_CONSTANT);
    assert!((back - 10.0).abs() < 1e-9, "{back}");
  }
}
