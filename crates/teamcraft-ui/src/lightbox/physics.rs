//! The gesture math of the lightbox, after Apple's *Designing Fluid Interfaces*: a finger moves the
//! picture 1:1, the release hands its velocity to a spring, momentum is projected to decide where a
//! flick lands, and limits resist (rubber-band) instead of stopping dead.
//!
//! Everything here is pure and works on injected timestamps (milliseconds, the clock of
//! `Event.timeStamp` and `performance.now()`), so the view only samples it once per frame.
//! Velocities are in pixels per millisecond.

use std::collections::VecDeque;
use std::f64::consts::TAU;

use super::{Direction, Point, Size};

/// Travel before a drag picks its axis (paging or closing), so a wobbly tap stays a tap.
pub const DRAG_HYSTERESIS_PX: f64 = 10.0;
/// Deceleration of a page flick per millisecond; a snappy one, like a paging scroll view.
pub const PAGE_DECELERATION: f64 = 0.99;
/// Deceleration of a zoomed picture gliding after a pan flick, like a normal scroll view.
pub const GLIDE_DECELERATION: f64 = 0.998;
/// A flick at least this fast pages, however short it was.
pub const PAGE_FLICK_PX_PER_MS: f64 = 0.3;
/// A flick at least this fast closes, however short it was.
pub const DISMISS_FLICK_PX_PER_MS: f64 = 0.5;
/// Share of the stage height a picture is dragged away to close on release.
pub const DISMISS_DISTANCE_RATIO: f64 = 0.25;
/// Smallest scale of a picture dragged away to close.
pub const DISMISS_MIN_SCALE: f64 = 0.85;
/// How strongly a limit resists: the classic UIScrollView constant.
pub const RUBBERBAND_CONSTANT: f64 = 0.55;
/// Response of the critically damped spring every transition settles with.
pub const SPRING_RESPONSE_MS: f64 = 300.0;
/// Space between neighbouring pictures while paging.
pub const PAGE_GAP_PX: f64 = 24.0;
/// How far past its limits a zoom may be pinched, as a factor (in log space, like the slider).
pub const ZOOM_OVERSHOOT_FACTOR: f64 = 1.5;
/// Pointer samples older than this don't count towards the release velocity.
const VELOCITY_WINDOW_MS: f64 = 100.0;
/// A position this close to its target has arrived (half a pixel).
const PIXEL_PRECISION: f64 = 0.5;
/// The same for scales and opacities.
pub const FRACTION_PRECISION: f64 = 0.001;
/// A value at rest moves less than this share of its precision per millisecond.
const REST_VELOCITY_RATIO: f64 = 0.02;

/// Which way a single-finger drag on a fitted picture goes, decided after the hysteresis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragAxis {
  /// Sideways: pages to the neighbours.
  Horizontal,
  /// Up or down: drags the picture away to close.
  Vertical,
}

/// Where a released page drag goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageOutcome {
  /// Back to the picture it started on.
  Stay,
  Turn(Direction),
}

/// Pointer velocity from the samples of the last [`VELOCITY_WINDOW_MS`].
#[derive(Clone, Debug, Default)]
pub struct VelocityTracker {
  samples: VecDeque<(f64, Point)>,
}

/// A critically damped spring from `from` (moving at `velocity`) to `target`: it never overshoots
/// unless it was thrown past the target, and its state is known at any time, so it can be
/// interrupted and re-aimed from wherever it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
  target: f64,
  /// Distance from the target at the start.
  displacement: f64,
  velocity: f64,
  /// Angular frequency per millisecond.
  omega: f64,
}

/// Momentum after a flick, slowing down by `deceleration` per millisecond: the position converges to
/// [`project`]'s endpoint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glide {
  from: f64,
  velocity: f64,
  deceleration: f64,
}

/// One animated number: where it is, and the motion it is in, if any.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Animated {
  value: f64,
  velocity: f64,
  /// How close to its target it counts as arrived.
  precision: f64,
  maybe_motion: Option<(f64, Motion)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Motion {
  Spring(Spring),
  /// A glide that hands over to a spring onto the nearest limit once it leaves `min..=max`.
  Glide {
    glide: Glide,
    min: f64,
    max: f64,
  },
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

/// `value` inside `min..=max`, and resisted beyond.
pub fn rubberband_clamp(value: f64, min: f64, max: f64, dimension: f64) -> f64 {
  if value < min {
    min + rubberband(value - min, dimension, RUBBERBAND_CONSTANT)
  } else if value > max {
    max + rubberband(value - max, dimension, RUBBERBAND_CONSTANT)
  } else {
    value
  }
}

/// The value whose [`rubberband_clamp`] is `shown`.
pub fn unrubberband_clamp(shown: f64, min: f64, max: f64, dimension: f64) -> f64 {
  if shown < min {
    min + unrubberband(shown - min, dimension, RUBBERBAND_CONSTANT)
  } else if shown > max {
    max + unrubberband(shown - max, dimension, RUBBERBAND_CONSTANT)
  } else {
    shown
  }
}

/// The axis of a drag that travelled `travel` from its press, `None` within the hysteresis.
pub fn drag_axis(travel: Point) -> Option<DragAxis> {
  if travel.x.hypot(travel.y) < DRAG_HYSTERESIS_PX {
    return None;
  }
  match travel.x.abs() >= travel.y.abs() {
    true => Some(DragAxis::Horizontal),
    false => Some(DragAxis::Vertical),
  }
}

/// Where a page drag released at `offset` (negative: the next picture shows) with `velocity` goes,
/// on a strip whose pictures are `page_width` apart.
///
/// A flick decides by its direction (reversing a drag cancels it), a slow release by where its
/// momentum would carry it; a missing neighbour always means staying.
pub fn page_outcome(
  offset: f64,
  velocity: f64,
  page_width: f64,
  has_previous: bool,
  has_next: bool,
) -> PageOutcome {
  let half = page_width / 2.0;
  let wanted = if velocity <= -PAGE_FLICK_PX_PER_MS {
    // A flick towards the next picture, unless the previous one is already mostly in view.
    (offset < half / 2.0).then_some(Direction::Next)
  } else if velocity >= PAGE_FLICK_PX_PER_MS {
    (offset > -half / 2.0).then_some(Direction::Previous)
  } else {
    let projected = offset + project(velocity, PAGE_DECELERATION);
    if projected <= -half {
      Some(Direction::Next)
    } else if projected >= half {
      Some(Direction::Previous)
    } else {
      None
    }
  };
  match wanted {
    Some(Direction::Next) if has_next => PageOutcome::Turn(Direction::Next),
    Some(Direction::Previous) if has_previous => PageOutcome::Turn(Direction::Previous),
    _ => PageOutcome::Stay,
  }
}

/// Where the strip rests for an outcome: the next picture sits one page to the left.
pub fn page_target(outcome: PageOutcome, page_width: f64) -> f64 {
  match outcome {
    PageOutcome::Stay => 0.0,
    PageOutcome::Turn(Direction::Next) => -page_width,
    PageOutcome::Turn(Direction::Previous) => page_width,
  }
}

/// The strip's offset for a page drag of `raw` pixels: 1:1 towards a neighbour, resisted where
/// there is none.
pub fn page_offset(raw: f64, page_width: f64, has_previous: bool, has_next: bool) -> f64 {
  let min = if has_next { -page_width } else { 0.0 };
  let max = if has_previous { page_width } else { 0.0 };
  rubberband_clamp(raw, min, max, page_width)
}

/// How far a picture dragged `offset` pixels up or down is on its way out, from 0 to 1 (a full
/// stage height).
pub fn dismiss_progress(offset: f64, stage_height: f64) -> f64 {
  match stage_height > 0.0 {
    true => (offset.abs() / stage_height).clamp(0.0, 1.0),
    false => 0.0,
  }
}

/// The scale of a picture on its way out: it shrinks to [`DISMISS_MIN_SCALE`] over half the height.
pub fn dismiss_scale(progress: f64) -> f64 {
  1.0 - (1.0 - DISMISS_MIN_SCALE) * (progress * 2.0).min(1.0)
}

/// The backdrop fades with the distance: half gone at the closing threshold, gone at twice it.
pub fn dismiss_backdrop_opacity(progress: f64) -> f64 {
  (1.0 - progress / (2.0 * DISMISS_DISTANCE_RATIO)).clamp(0.0, 1.0)
}

/// Whether a picture released `offset` pixels up or down with `velocity` closes the lightbox: far
/// enough, or flicked away. A flick back towards the middle keeps it open.
pub fn should_dismiss(offset: f64, velocity: f64, stage_height: f64) -> bool {
  let is_flicked_away = velocity.abs() >= DISMISS_FLICK_PX_PER_MS && velocity.signum() == offset.signum();
  let is_flicked_back = velocity.abs() >= DISMISS_FLICK_PX_PER_MS && velocity.signum() != offset.signum();
  let is_far = dismiss_progress(offset, stage_height) >= DISMISS_DISTANCE_RATIO;
  is_flicked_away || (is_far && !is_flicked_back)
}

/// A pinched zoom past its limits, resisted in log space like the slider: at most
/// [`ZOOM_OVERSHOOT_FACTOR`] beyond either end.
pub fn rubberband_scale(raw: f64, min: f64, max: f64) -> f64 {
  if raw <= 0.0 || !raw.is_finite() {
    return min;
  }
  rubberband_clamp(raw.ln(), min.ln(), max.ln(), ZOOM_OVERSHOOT_FACTOR.ln()).exp()
}

/// The raw pinch zoom whose [`rubberband_scale`] is `shown`.
pub fn unrubberband_scale(shown: f64, min: f64, max: f64) -> f64 {
  if shown <= 0.0 || !shown.is_finite() {
    return min;
  }
  unrubberband_clamp(shown.ln(), min.ln(), max.ln(), ZOOM_OVERSHOOT_FACTOR.ln()).exp()
}

/// The transform that lays a picture shown at `shown` (its centre and size on screen) over a tile
/// at `tile`: the offset of the centres and one scale. A tile crops its picture, so the scale is
/// the mean of the two sides' ratios.
pub fn flip_from(tile: Rect, shown: Rect) -> (Point, f64) {
  if shown.width <= 0.0 || shown.height <= 0.0 {
    return (Point::ORIGIN, 1.0);
  }
  let scale = (tile.width / shown.width * tile.height / shown.height).sqrt();
  let offset = Point::new(
    tile.center().x - shown.center().x,
    tile.center().y - shown.center().y,
  );
  (offset, scale)
}

/// The picture a box shows with `object-fit: contain`: `natural` scaled to fit `bounds`, the box
/// itself while the natural size is unknown.
pub fn contained_size(bounds: Size, natural: Size) -> Size {
  if natural.width <= 0.0 || natural.height <= 0.0 {
    return bounds;
  }
  let scale = (bounds.width / natural.width).min(bounds.height / natural.height);
  Size::new(natural.width * scale, natural.height * scale)
}

/// A rectangle in CSS pixels, from `getBoundingClientRect()`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
  pub left: f64,
  pub top: f64,
  pub width: f64,
  pub height: f64,
}

impl Rect {
  pub fn new(left: f64, top: f64, width: f64, height: f64) -> Self {
    Self {
      left,
      top,
      width,
      height,
    }
  }

  pub fn center(&self) -> Point {
    Point::new(self.left + self.width / 2.0, self.top + self.height / 2.0)
  }

  /// The part both rectangles cover (empty when they don't overlap).
  pub fn intersect(&self, other: Rect) -> Rect {
    let left = self.left.max(other.left);
    let top = self.top.max(other.top);
    let right = (self.left + self.width).min(other.left + other.width);
    let bottom = (self.top + self.height).min(other.top + other.height);
    Rect::new(left, top, (right - left).max(0.0), (bottom - top).max(0.0))
  }

  /// Whether the whole rectangle is inside `bounds` (a tile half scrolled away is no target).
  pub fn is_inside(&self, bounds: Rect) -> bool {
    self.width > 0.0
      && self.height > 0.0
      && self.left >= bounds.left
      && self.top >= bounds.top
      && self.left + self.width <= bounds.left + bounds.width
      && self.top + self.height <= bounds.top + bounds.height
  }
}

impl VelocityTracker {
  /// Starts over at the press.
  pub fn reset(&mut self, time_ms: f64, at: Point) {
    self.samples.clear();
    self.samples.push_back((time_ms, at));
  }

  /// Forgets the samples: a pinch's fingers say nothing about a pan's velocity.
  pub fn clear(&mut self) {
    self.samples.clear();
  }

  pub fn push(&mut self, time_ms: f64, at: Point) {
    self.samples.push_back((time_ms, at));
    while self
      .samples
      .front()
      .is_some_and(|(sampled, _)| time_ms - sampled > VELOCITY_WINDOW_MS)
      && self.samples.len() > 2
    {
      self.samples.pop_front();
    }
  }

  /// The velocity at `time_ms` (the release): zero when the pointer rested for the whole window
  /// before letting go.
  pub fn velocity(&self, time_ms: f64) -> Point {
    let (Some(&(first_ms, first)), Some(&(last_ms, last))) = (self.samples.front(), self.samples.back())
    else {
      return Point::ORIGIN;
    };
    let elapsed = last_ms - first_ms;
    if elapsed <= 0.0 || time_ms - last_ms > VELOCITY_WINDOW_MS {
      return Point::ORIGIN;
    }
    Point::new((last.x - first.x) / elapsed, (last.y - first.y) / elapsed)
  }
}

impl Spring {
  pub fn new(from: f64, target: f64, velocity: f64, response_ms: f64) -> Self {
    Self {
      target,
      displacement: from - target,
      velocity,
      omega: TAU / response_ms.max(1.0),
    }
  }

  pub fn target(&self) -> f64 {
    self.target
  }

  /// Position and velocity `elapsed_ms` after the start.
  pub fn at(&self, elapsed_ms: f64) -> (f64, f64) {
    let t = elapsed_ms.max(0.0);
    let (c1, c2) = (self.displacement, self.velocity + self.omega * self.displacement);
    let decay = (-self.omega * t).exp();
    let position = self.target + (c1 + c2 * t) * decay;
    let velocity = (c2 - self.omega * (c1 + c2 * t)) * decay;
    (position, velocity)
  }
}

impl Glide {
  pub fn new(from: f64, velocity: f64, deceleration: f64) -> Self {
    Self {
      from,
      velocity,
      deceleration,
    }
  }

  /// Position and velocity `elapsed_ms` after the release.
  pub fn at(&self, elapsed_ms: f64) -> (f64, f64) {
    let t = elapsed_ms.max(0.0);
    let ln = self.deceleration.ln();
    let factor = self.deceleration.powf(t);
    (
      self.from + self.velocity * (factor - 1.0) / ln,
      self.velocity * factor,
    )
  }
}

impl Animated {
  /// A position in pixels.
  pub fn new(value: f64) -> Self {
    Self::with_precision(value, PIXEL_PRECISION)
  }

  /// A scale or opacity, which needs a finer rest than pixels.
  pub fn fraction(value: f64) -> Self {
    Self::with_precision(value, FRACTION_PRECISION)
  }

  fn with_precision(value: f64, precision: f64) -> Self {
    Self {
      value,
      velocity: 0.0,
      precision,
      maybe_motion: None,
    }
  }

  pub fn value(&self) -> f64 {
    self.value
  }

  pub fn velocity(&self) -> f64 {
    self.velocity
  }

  pub fn is_moving(&self) -> bool {
    self.maybe_motion.is_some()
  }

  /// Where it is heading: the spring's target, or the current value.
  pub fn target(&self) -> f64 {
    match self.maybe_motion {
      Some((_, Motion::Spring(spring))) => spring.target(),
      _ => self.value,
    }
  }

  /// Whether it is within `tolerance` of where it is heading.
  pub fn is_near(&self, tolerance: f64) -> bool {
    (self.value - self.target()).abs() <= tolerance
  }

  /// Jumps to `value` and stops (a finger holds it, or reduced motion).
  pub fn set(&mut self, value: f64) {
    self.value = value;
    self.velocity = 0.0;
    self.maybe_motion = None;
  }

  /// Stops where it is, keeping the value on screen: a finger grabbed it mid-flight.
  pub fn stop(&mut self) {
    self.velocity = 0.0;
    self.maybe_motion = None;
  }

  /// Springs from where it is to `target`, starting at `velocity` (the finger's, on a release).
  pub fn spring_to(&mut self, target: f64, velocity: f64, now_ms: f64) {
    self.velocity = velocity;
    let spring = Spring::new(self.value, target, velocity, SPRING_RESPONSE_MS);
    self.maybe_motion = Some((now_ms, Motion::Spring(spring)));
  }

  /// Glides on with `velocity`, and springs back onto `min` or `max` if it leaves them.
  pub fn glide(&mut self, velocity: f64, min: f64, max: f64, now_ms: f64) {
    self.velocity = velocity;
    if self.value < min || self.value > max {
      self.spring_to(self.value.clamp(min, max), velocity, now_ms);
      return;
    }
    let glide = Glide::new(self.value, velocity, GLIDE_DECELERATION);
    self.maybe_motion = Some((now_ms, Motion::Glide { glide, min, max }));
  }

  /// Advances to `now_ms`; returns whether it is still moving.
  pub fn tick(&mut self, now_ms: f64) -> bool {
    let Some((start_ms, motion)) = self.maybe_motion else {
      return false;
    };
    let (value, velocity) = match motion {
      Motion::Spring(spring) => spring.at(now_ms - start_ms),
      Motion::Glide { glide, .. } => glide.at(now_ms - start_ms),
    };
    self.value = value;
    self.velocity = velocity;
    let rest_velocity = self.precision * REST_VELOCITY_RATIO;
    match motion {
      Motion::Spring(spring) => {
        if (value - spring.target()).abs() < self.precision && velocity.abs() < rest_velocity {
          self.set(spring.target());
        }
      },
      Motion::Glide { min, max, .. } => {
        if value < min || value > max {
          // Past the edge: the momentum carries it a little further, then it settles on the edge.
          self.spring_to(value.clamp(min, max), velocity, now_ms);
        } else if velocity.abs() < rest_velocity {
          self.stop();
        }
      },
    }
    self.is_moving()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const WIDTH: f64 = 1000.0;
  const HEIGHT: f64 = 800.0;

  mod projection {
    use super::*;

    #[test]
    fn projection_follows_apples_formula() {
      // 1000 px/s at 0.99: 1 · 0.99 / 0.01 = 99 px.
      assert!((project(1.0, PAGE_DECELERATION) - 99.0).abs() < 1e-9);
      assert!((project(-2.0, GLIDE_DECELERATION) + 998.0).abs() < 1e-6);
      assert_eq!(project(0.0, PAGE_DECELERATION), 0.0);
    }

    #[test]
    fn a_glide_comes_to_rest_at_the_projected_endpoint() {
      let glide = Glide::new(100.0, 1.5, GLIDE_DECELERATION);
      let (end, velocity) = glide.at(20_000.0);
      let expected = 100.0 + project(1.5, GLIDE_DECELERATION);
      // The continuous decay lands within a pixel of the discrete projection.
      assert!((end - expected).abs() < 1.0, "{end} vs {expected}");
      assert!(velocity.abs() < 1e-6);
      assert_eq!(glide.at(0.0), (100.0, 1.5));
    }
  }

  mod rubber {
    use super::*;

    #[test]
    fn inside_the_limits_values_pass_through() {
      assert_eq!(rubberband_clamp(40.0, -100.0, 100.0, WIDTH), 40.0);
      assert_eq!(rubberband_clamp(-100.0, -100.0, 100.0, WIDTH), -100.0);
    }

    #[test]
    fn past_a_limit_resistance_grows_and_never_reaches_the_dimension() {
      let small = rubberband(10.0, WIDTH, RUBBERBAND_CONSTANT);
      let large = rubberband(1000.0, WIDTH, RUBBERBAND_CONSTANT);
      let huge = rubberband(1e9, WIDTH, RUBBERBAND_CONSTANT);
      assert!(
        small > 5.0 && small < 10.0,
        "a little resistance at first: {small}"
      );
      assert!(large < 400.0, "much more further out: {large}");
      assert!(huge < WIDTH, "bounded by the dimension: {huge}");
      assert_eq!(rubberband(-10.0, WIDTH, RUBBERBAND_CONSTANT), -small, "symmetric");
      assert_eq!(
        rubberband(10.0, 0.0, RUBBERBAND_CONSTANT),
        0.0,
        "no room, no movement"
      );
    }

    #[test]
    fn a_resisted_value_can_be_picked_up_where_it_is() {
      for raw in [-900.0, -150.0, -100.0, 0.0, 64.0, 100.0, 120.0, 2500.0] {
        let shown = rubberband_clamp(raw, -100.0, 100.0, WIDTH);
        let back = unrubberband_clamp(shown, -100.0, 100.0, WIDTH);
        assert!((back - raw).abs() < 1e-6, "{raw} → {shown} → {back}");
      }
    }

    #[test]
    fn a_pinch_past_the_zoom_limits_resists_in_both_directions() {
      assert_eq!(rubberband_scale(2.0, 1.0, 8.0), 2.0);
      let over = rubberband_scale(16.0, 1.0, 8.0);
      assert!(over > 8.0 && over < 8.0 * ZOOM_OVERSHOOT_FACTOR, "{over}");
      let under = rubberband_scale(0.25, 1.0, 8.0);
      assert!(under < 1.0 && under > 1.0 / ZOOM_OVERSHOOT_FACTOR, "{under}");
      assert_eq!(rubberband_scale(f64::NAN, 1.0, 8.0), 1.0);
      let back = unrubberband_scale(over, 1.0, 8.0);
      assert!((back - 16.0).abs() < 1e-6, "{back}");
    }

    #[test]
    fn paging_without_a_neighbour_resists() {
      assert_eq!(
        page_offset(-300.0, WIDTH, true, true),
        -300.0,
        "1:1 with a neighbour"
      );
      let at_end = page_offset(-300.0, WIDTH, true, false);
      assert!(at_end < 0.0 && at_end > -300.0, "{at_end}");
      let at_start = page_offset(300.0, WIDTH, false, true);
      assert!(at_start > 0.0 && at_start < 300.0, "{at_start}");
      assert!(
        page_offset(-1500.0, WIDTH, true, true) > -1500.0,
        "never past the neighbour without resistance"
      );
    }
  }

  mod hysteresis {
    use super::*;

    #[test]
    fn small_movements_pick_no_axis() {
      assert_eq!(drag_axis(Point::new(6.0, 6.0)), None);
      assert_eq!(drag_axis(Point::new(-9.9, 0.0)), None);
    }

    #[test]
    fn the_larger_component_picks_the_axis() {
      assert_eq!(drag_axis(Point::new(-12.0, 3.0)), Some(DragAxis::Horizontal));
      assert_eq!(drag_axis(Point::new(4.0, 11.0)), Some(DragAxis::Vertical));
      assert_eq!(drag_axis(Point::new(0.0, -10.0)), Some(DragAxis::Vertical));
    }
  }

  mod paging {
    use super::*;

    #[test]
    fn a_slow_release_past_half_the_width_pages() {
      assert_eq!(
        page_outcome(-520.0, 0.0, WIDTH, true, true),
        PageOutcome::Turn(Direction::Next)
      );
      assert_eq!(
        page_outcome(510.0, 0.05, WIDTH, true, true),
        PageOutcome::Turn(Direction::Previous)
      );
      assert_eq!(page_outcome(-480.0, 0.0, WIDTH, true, true), PageOutcome::Stay);
    }

    #[test]
    fn momentum_carries_a_short_drag_over_half_the_width() {
      // 0.25 px/ms projects 24.75 px further: -480 lands at -504.75.
      assert_eq!(
        page_outcome(-480.0, -0.25, WIDTH, true, true),
        PageOutcome::Turn(Direction::Next)
      );
    }

    #[test]
    fn a_flick_pages_however_short() {
      assert_eq!(
        page_outcome(-30.0, -0.4, WIDTH, true, true),
        PageOutcome::Turn(Direction::Next)
      );
      assert_eq!(
        page_outcome(20.0, 0.31, WIDTH, true, true),
        PageOutcome::Turn(Direction::Previous)
      );
    }

    #[test]
    fn a_flick_back_cancels_a_long_drag() {
      assert_eq!(page_outcome(-700.0, 0.5, WIDTH, true, true), PageOutcome::Stay);
      assert_eq!(page_outcome(700.0, -0.5, WIDTH, true, true), PageOutcome::Stay);
    }

    #[test]
    fn there_is_no_paging_past_the_ends() {
      assert_eq!(page_outcome(-900.0, -2.0, WIDTH, true, false), PageOutcome::Stay);
      assert_eq!(page_outcome(900.0, 2.0, WIDTH, false, true), PageOutcome::Stay);
    }

    #[test]
    fn the_strip_rests_one_page_away() {
      assert_eq!(page_target(PageOutcome::Stay, WIDTH), 0.0);
      assert_eq!(page_target(PageOutcome::Turn(Direction::Next), WIDTH), -WIDTH);
      assert_eq!(page_target(PageOutcome::Turn(Direction::Previous), WIDTH), WIDTH);
    }
  }

  mod dismiss {
    use super::*;

    #[test]
    fn a_quarter_of_the_height_closes() {
      assert!(should_dismiss(0.25 * HEIGHT, 0.0, HEIGHT));
      assert!(should_dismiss(-0.3 * HEIGHT, 0.0, HEIGHT), "up as well as down");
      assert!(!should_dismiss(0.2 * HEIGHT, 0.0, HEIGHT));
    }

    #[test]
    fn a_flick_closes_and_a_flick_back_keeps_it_open() {
      assert!(should_dismiss(40.0, 0.6, HEIGHT));
      assert!(!should_dismiss(40.0, 0.4, HEIGHT), "too slow and too short");
      assert!(
        !should_dismiss(0.4 * HEIGHT, -0.8, HEIGHT),
        "thrown back to the middle"
      );
    }

    #[test]
    fn the_picture_shrinks_and_the_backdrop_fades_with_the_distance() {
      assert_eq!(dismiss_scale(0.0), 1.0);
      assert!((dismiss_scale(0.25) - 0.925).abs() < 1e-9);
      assert_eq!(dismiss_scale(0.9), DISMISS_MIN_SCALE, "never below the minimum");
      assert_eq!(dismiss_backdrop_opacity(0.0), 1.0);
      assert!((dismiss_backdrop_opacity(dismiss_progress(200.0, HEIGHT)) - 0.5).abs() < 1e-9);
      assert_eq!(dismiss_backdrop_opacity(1.0), 0.0);
      assert_eq!(dismiss_progress(100.0, 0.0), 0.0, "not laid out yet");
    }
  }

  mod springs {
    use super::*;

    #[test]
    fn a_spring_starts_where_it_is_and_settles_on_its_target() {
      let spring = Spring::new(-300.0, -1000.0, -0.8, SPRING_RESPONSE_MS);
      let (start, start_velocity) = spring.at(0.0);
      assert!(
        (start + 300.0).abs() < 1e-9 && (start_velocity + 0.8).abs() < 1e-9,
        "velocity handoff"
      );
      let (end, end_velocity) = spring.at(2000.0);
      assert!((end + 1000.0).abs() < 0.01 && end_velocity.abs() < 1e-4);
    }

    #[test]
    fn a_critically_damped_spring_does_not_overshoot_from_rest() {
      let spring = Spring::new(0.0, 100.0, 0.0, SPRING_RESPONSE_MS);
      let positions = (0..200).map(|step| spring.at(f64::from(step) * 5.0).0);
      assert!(positions.clone().all(|position| position <= 100.0 + 1e-9));
      let collected: Vec<f64> = positions.collect();
      assert!(
        collected.windows(2).all(|pair| pair[1] >= pair[0] - 1e-9),
        "monotonic"
      );
    }

    #[test]
    fn an_animated_value_comes_to_rest_and_can_be_interrupted() {
      let mut value = Animated::new(0.0);
      value.spring_to(100.0, 0.0, 1000.0);
      assert!(value.tick(1100.0));
      let midway = value.value();
      assert!(midway > 0.0 && midway < 100.0);
      value.stop();
      assert_eq!(value.value(), midway, "a finger holds it where it is");
      assert!(!value.tick(1200.0));
      value.spring_to(0.0, 0.0, 1200.0);
      let mut now = 1200.0;
      while value.tick(now) {
        now += 16.0;
        assert!(now < 5000.0, "it settles");
      }
      assert_eq!(value.value(), 0.0);
    }

    #[test]
    fn a_scale_settles_precisely() {
      let mut scale = Animated::fraction(0.2);
      scale.spring_to(1.0, 0.0, 0.0);
      assert!(scale.tick(16.0), "0.8 away is far for a scale");
      let mut now = 16.0;
      while scale.tick(now) {
        now += 16.0;
      }
      assert_eq!(scale.value(), 1.0);
      assert!(now > 200.0, "it took a while: {now}");
    }

    #[test]
    fn a_glide_stops_at_the_edge_and_springs_back_onto_it() {
      let mut value = Animated::new(0.0);
      value.glide(2.0, -100.0, 100.0, 0.0);
      let mut now = 0.0;
      let mut furthest: f64 = 0.0;
      while value.tick(now) {
        furthest = furthest.max(value.value());
        now += 16.0;
        assert!(now < 10_000.0, "it settles");
      }
      assert!(
        furthest > 100.0 && furthest < 150.0,
        "a short overshoot: {furthest}"
      );
      assert_eq!(value.value(), 100.0);
    }

    #[test]
    fn a_glide_inside_the_limits_just_slows_down() {
      let mut value = Animated::new(0.0);
      value.glide(0.2, -1000.0, 1000.0, 0.0);
      let mut now = 0.0;
      while value.tick(now) {
        now += 16.0;
      }
      let expected = project(0.2, GLIDE_DECELERATION);
      assert!(
        (value.value() - expected).abs() < 6.0,
        "{} vs {expected}",
        value.value()
      );
    }

    #[test]
    fn released_outside_the_limits_it_springs_back() {
      let mut value = Animated::new(130.0);
      value.glide(0.0, -100.0, 100.0, 0.0);
      assert_eq!(value.target(), 100.0);
    }
  }

  mod velocity {
    use super::*;

    #[test]
    fn velocity_comes_from_the_recent_samples() {
      let mut tracker = VelocityTracker::default();
      tracker.reset(0.0, Point::ORIGIN);
      // Slow at first, then fast: only the last 100 ms count.
      tracker.push(200.0, Point::new(10.0, 0.0));
      tracker.push(250.0, Point::new(40.0, 5.0));
      tracker.push(300.0, Point::new(90.0, 10.0));
      let velocity = tracker.velocity(300.0);
      assert!(
        (velocity.x - 0.8).abs() < 1e-9 && (velocity.y - 0.1).abs() < 1e-9,
        "{velocity:?}"
      );
    }

    #[test]
    fn a_pointer_that_rested_before_release_has_no_velocity() {
      let mut tracker = VelocityTracker::default();
      tracker.reset(0.0, Point::ORIGIN);
      tracker.push(16.0, Point::new(50.0, 0.0));
      assert_eq!(tracker.velocity(500.0), Point::ORIGIN);
      assert_eq!(VelocityTracker::default().velocity(0.0), Point::ORIGIN);
    }
  }

  mod flip {
    use super::*;

    #[test]
    fn the_picture_maps_onto_its_tile() {
      let shown = Rect::new(100.0, 100.0, 800.0, 600.0);
      let tile = Rect::new(20.0, 500.0, 200.0, 150.0);
      let (offset, scale) = flip_from(tile, shown);
      assert_eq!(offset, Point::new(120.0 - 500.0, 575.0 - 400.0));
      assert!((scale - 0.25).abs() < 1e-9);
      assert_eq!(
        flip_from(tile, Rect::default()),
        (Point::ORIGIN, 1.0),
        "not laid out"
      );
    }

    #[test]
    fn a_letterboxed_picture_is_measured_by_what_it_shows() {
      // A portrait picture in a wide placeholder box shows narrower than the box.
      let shown = contained_size(Size::new(768.0, 600.0), Size::new(900.0, 1200.0));
      assert_eq!(shown, Size::new(450.0, 600.0));
      assert_eq!(
        contained_size(Size::new(768.0, 600.0), Size::default()),
        Size::new(768.0, 600.0),
        "not decoded yet"
      );
    }

    #[test]
    fn only_a_tile_fully_in_view_is_a_target() {
      let view = Rect::new(0.0, 0.0, 1000.0, 800.0);
      assert!(Rect::new(10.0, 10.0, 100.0, 100.0).is_inside(view));
      assert!(!Rect::new(950.0, 10.0, 100.0, 100.0).is_inside(view));
      assert!(!Rect::new(10.0, 10.0, 0.0, 0.0).is_inside(view), "hidden");
      let list = Rect::new(0.0, 100.0, 1000.0, 500.0);
      let clipped = view.intersect(list);
      assert_eq!(clipped, list);
      assert!(
        !Rect::new(10.0, 580.0, 100.0, 100.0).is_inside(clipped),
        "under the composer"
      );
      assert_eq!(view.intersect(Rect::new(2000.0, 0.0, 10.0, 10.0)).width, 0.0);
    }
  }
}
