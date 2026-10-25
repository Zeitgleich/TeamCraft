//! The arithmetic of the phone gestures (#368): dragging a sheet down to close it and swiping from
//! the leading edge to go back. Pure, so it is unit-tested; the components feed in pointer
//! positions in CSS pixels and times in milliseconds.
//!
//! Both follow the finger 1:1 and decide on release with the momentum the finger had: where the
//! movement would come to rest if it coasted on (Apple's "projection" with the scroll view's
//! deceleration rate), not only where the finger let go.

/// Deceleration per millisecond of a coasting scroll view (UIScrollView's `normal` rate).
const DECELERATION_RATE: f64 = 0.998;
/// Samples older than this don't count for the release velocity: a finger that stopped before
/// lifting has no momentum.
const VELOCITY_WINDOW_MS: f64 = 100.0;

/// A sheet closes when flicked down faster than this (px/ms) …
const SHEET_DISMISS_VELOCITY: f64 = 0.11;
/// … or dragged down past this part of its height.
const SHEET_DISMISS_FRACTION: f64 = 0.3;
/// Movement before a press on a sheet becomes a drag (taps and small jitters stay taps).
const SHEET_SLOP_PX: f64 = 6.0;
/// How far a sheet gives when pulled up past its resting place: the pull is damped to this scale.
const RUBBER_BAND_PX: f64 = 24.0;

/// Width of the leading-edge strip where a back swipe can start.
pub const EDGE_START_PX: f64 = 20.0;
/// Horizontal movement before an edge press becomes a swipe.
const SWIPE_SLOP_PX: f64 = 10.0;
/// A swipe goes back when its projected end passes this part of the width.
const SWIPE_COMMIT_FRACTION: f64 = 0.5;

/// Where a movement that leaves the finger at `velocity_px_per_ms` comes to rest, measured from
/// where it was let go.
pub fn projected_travel(velocity_px_per_ms: f64) -> f64 {
  velocity_px_per_ms * DECELERATION_RATE / (1.0 - DECELERATION_RATE)
}

/// Positions of a moving finger along one axis, for its velocity at the end.
#[derive(Clone, Debug, Default)]
struct Track {
  samples: Vec<(f64, f64)>,
}

/// Dragging a sheet by its body or its handle.
#[derive(Clone, Debug)]
pub struct SheetDrag {
  start_y: f64,
  start_x: f64,
  is_dragging: bool,
  track: Track,
}

/// What a sheet does when the finger lifts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SheetRelease {
  /// It was a tap, not a drag: nothing moved.
  Tap,
  /// Close it (it leaves from where the finger left it).
  Dismiss,
  /// Spring back to the open position.
  Settle,
}

/// Swiping back from the leading edge of a pane.
#[derive(Clone, Debug)]
pub struct EdgeSwipe {
  start_x: f64,
  start_y: f64,
  state: SwipeState,
  track: Track,
}

/// Where an edge swipe stands after a move.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SwipeState {
  /// Not decided yet: the finger hasn't moved far enough either way.
  Pending,
  /// Horizontal: the pane follows the finger by this many pixels.
  Following(f64),
  /// Mostly vertical, or backwards: not a back swipe; the content scrolls as usual.
  Abandoned,
}

/// What an edge swipe does when the finger lifts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SwipeRelease {
  Back,
  Stay,
}

impl Track {
  fn push(&mut self, position: f64, at_ms: f64) {
    self.samples.push((position, at_ms));
    self
      .samples
      .retain(|(_, sample_at)| at_ms - sample_at <= VELOCITY_WINDOW_MS);
  }

  /// Pixels per millisecond over the recent samples; 0 without two of them.
  fn velocity(&self) -> f64 {
    let (Some(first), Some(last)) = (self.samples.first(), self.samples.last()) else {
      return 0.0;
    };
    let elapsed = last.1 - first.1;
    if elapsed <= 0.0 {
      return 0.0;
    }
    (last.0 - first.0) / elapsed
  }
}

impl SheetDrag {
  pub fn start(x: f64, y: f64, at_ms: f64) -> Self {
    let mut track = Track::default();
    track.push(y, at_ms);
    Self {
      start_y: y,
      start_x: x,
      is_dragging: false,
      track,
    }
  }

  /// The finger moved; returns how far the sheet is pulled down (negative: rubber-banded up), or
  /// `None` while it isn't a drag (yet).
  pub fn moved(&mut self, x: f64, y: f64, at_ms: f64) -> Option<f64> {
    self.track.push(y, at_ms);
    let (dx, dy) = (x - self.start_x, y - self.start_y);
    if !self.is_dragging {
      // Down and more down than sideways; a pull up is the content's scroll, not the sheet's.
      if dy < SHEET_SLOP_PX || dx.abs() > dy {
        return None;
      }
      self.is_dragging = true;
    }
    Some(rubber_band(dy))
  }

  pub fn is_dragging(&self) -> bool {
    self.is_dragging
  }

  /// Whether the finger at `y` pulls the sheet down (rather than scrolling its content up), so the
  /// browser must not take the movement for a scroll.
  pub fn holds_the_finger(&self, y: f64) -> bool {
    self.is_dragging || y > self.start_y
  }

  /// The finger lifted at `y`; `height` is the sheet's own height.
  pub fn released(&self, y: f64, height: f64) -> SheetRelease {
    if !self.is_dragging {
      return SheetRelease::Tap;
    }
    let pulled = y - self.start_y;
    let velocity = self.track.velocity();
    if velocity > SHEET_DISMISS_VELOCITY || pulled > height * SHEET_DISMISS_FRACTION {
      SheetRelease::Dismiss
    } else {
      SheetRelease::Settle
    }
  }
}

/// A pull up past the resting place gives less and less; a pull down follows the finger.
fn rubber_band(offset: f64) -> f64 {
  if offset >= 0.0 {
    return offset;
  }
  let pull = -offset;
  -(RUBBER_BAND_PX * pull / (pull + RUBBER_BAND_PX))
}

impl EdgeSwipe {
  /// A press at `x`; `None` when it is too far from the leading edge to be a back swipe.
  pub fn start(x: f64, y: f64, at_ms: f64) -> Option<Self> {
    if !(0.0..=EDGE_START_PX).contains(&x) {
      return None;
    }
    let mut track = Track::default();
    track.push(x, at_ms);
    Some(Self {
      start_x: x,
      start_y: y,
      state: SwipeState::Pending,
      track,
    })
  }

  pub fn moved(&mut self, x: f64, y: f64, at_ms: f64) -> SwipeState {
    self.track.push(x, at_ms);
    let (dx, dy) = (x - self.start_x, y - self.start_y);
    self.state = match self.state {
      SwipeState::Abandoned => SwipeState::Abandoned,
      SwipeState::Pending if dy.abs() > SWIPE_SLOP_PX && dy.abs() > dx.abs() => SwipeState::Abandoned,
      SwipeState::Pending if dx < SWIPE_SLOP_PX => SwipeState::Pending,
      SwipeState::Pending | SwipeState::Following(_) => SwipeState::Following(dx.max(0.0)),
    };
    self.state
  }

  pub fn state(&self) -> SwipeState {
    self.state
  }

  /// The finger lifted at `x`; `width` is the pane's width.
  pub fn released(&self, x: f64, width: f64) -> SwipeRelease {
    if !matches!(self.state, SwipeState::Following(_)) {
      return SwipeRelease::Stay;
    }
    let travelled = x - self.start_x;
    let projected = travelled + projected_travel(self.track.velocity());
    if projected > width * SWIPE_COMMIT_FRACTION {
      SwipeRelease::Back
    } else {
      SwipeRelease::Stay
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_flick_coasts_about_five_hundred_times_its_speed() {
    assert!((projected_travel(1.0) - 499.0).abs() < 1e-6);
    assert_eq!(projected_travel(0.0), 0.0);
  }

  mod sheet {
    use super::*;

    #[test]
    fn a_pull_down_holds_the_finger_a_scroll_up_does_not() {
      let drag = SheetDrag::start(100.0, 500.0, 0.0);
      assert!(drag.holds_the_finger(501.0));
      assert!(!drag.holds_the_finger(480.0));
    }

    #[test]
    fn a_tap_and_a_jitter_are_no_drag() {
      let mut drag = SheetDrag::start(100.0, 500.0, 0.0);
      assert_eq!(drag.moved(102.0, 503.0, 16.0), None);
      assert_eq!(drag.released(503.0, 400.0), SheetRelease::Tap);
    }

    #[test]
    fn sideways_and_upwards_movement_is_left_to_the_content() {
      let mut drag = SheetDrag::start(100.0, 500.0, 0.0);
      assert_eq!(drag.moved(140.0, 520.0, 16.0), None, "more sideways than down");
      assert_eq!(drag.moved(100.0, 450.0, 32.0), None, "up");
      assert!(!drag.is_dragging());
    }

    #[test]
    fn the_sheet_follows_the_finger_down_and_gives_little_upwards() {
      let mut drag = SheetDrag::start(100.0, 500.0, 0.0);
      assert_eq!(drag.moved(100.0, 580.0, 16.0), Some(80.0));
      let up = drag.moved(100.0, 300.0, 32.0).unwrap();
      assert!(up < 0.0 && up > -RUBBER_BAND_PX, "rubber band: {up}");
    }

    #[test]
    fn a_slow_short_drag_settles_back() {
      let mut drag = SheetDrag::start(100.0, 500.0, 0.0);
      drag.moved(100.0, 540.0, 400.0);
      drag.moved(100.0, 560.0, 800.0);
      assert_eq!(drag.released(560.0, 400.0), SheetRelease::Settle);
    }

    #[test]
    fn a_flick_or_a_long_drag_dismisses() {
      let mut flick = SheetDrag::start(100.0, 500.0, 0.0);
      flick.moved(100.0, 520.0, 10.0);
      flick.moved(100.0, 540.0, 20.0);
      assert_eq!(flick.released(540.0, 400.0), SheetRelease::Dismiss);

      let mut long = SheetDrag::start(100.0, 500.0, 0.0);
      long.moved(100.0, 600.0, 1000.0);
      long.moved(100.0, 640.0, 2000.0);
      assert_eq!(long.released(640.0, 400.0), SheetRelease::Dismiss, "past 30 %");
    }

    #[test]
    fn a_finger_that_stopped_before_lifting_has_no_momentum() {
      let mut drag = SheetDrag::start(100.0, 500.0, 0.0);
      drag.moved(100.0, 540.0, 10.0);
      drag.moved(100.0, 560.0, 20.0);
      drag.moved(100.0, 560.0, 400.0);
      assert_eq!(drag.released(560.0, 400.0), SheetRelease::Settle);
    }
  }

  mod edge_swipe {
    use super::*;

    #[test]
    fn only_a_press_at_the_leading_edge_starts_one() {
      assert!(EdgeSwipe::start(12.0, 300.0, 0.0).is_some());
      assert!(EdgeSwipe::start(EDGE_START_PX + 1.0, 300.0, 0.0).is_none());
    }

    #[test]
    fn vertical_movement_abandons_it_for_good() {
      let mut swipe = EdgeSwipe::start(5.0, 300.0, 0.0).unwrap();
      assert_eq!(swipe.moved(8.0, 330.0, 16.0), SwipeState::Abandoned);
      assert_eq!(swipe.moved(200.0, 330.0, 32.0), SwipeState::Abandoned);
      assert_eq!(swipe.released(200.0, 390.0), SwipeRelease::Stay);
    }

    #[test]
    fn past_the_slop_the_pane_follows_the_finger() {
      let mut swipe = EdgeSwipe::start(5.0, 300.0, 0.0).unwrap();
      assert_eq!(swipe.moved(12.0, 302.0, 16.0), SwipeState::Pending);
      assert_eq!(swipe.moved(65.0, 305.0, 32.0), SwipeState::Following(60.0));
      assert_eq!(
        swipe.moved(0.0, 305.0, 48.0),
        SwipeState::Following(0.0),
        "never past the edge"
      );
    }

    #[test]
    fn a_slow_swipe_goes_back_past_half_the_width_only() {
      let mut swipe = EdgeSwipe::start(5.0, 300.0, 0.0).unwrap();
      swipe.moved(100.0, 300.0, 500.0);
      swipe.moved(150.0, 300.0, 1000.0);
      assert_eq!(swipe.released(150.0, 390.0), SwipeRelease::Stay);
      swipe.moved(220.0, 300.0, 1500.0);
      swipe.moved(220.0, 300.0, 2000.0);
      assert_eq!(swipe.released(220.0, 390.0), SwipeRelease::Back);
    }

    #[test]
    fn a_short_flick_goes_back_by_its_momentum() {
      let mut swipe = EdgeSwipe::start(5.0, 300.0, 0.0).unwrap();
      swipe.moved(30.0, 300.0, 16.0);
      swipe.moved(80.0, 300.0, 48.0);
      assert_eq!(swipe.released(80.0, 390.0), SwipeRelease::Back);
    }
  }
}
