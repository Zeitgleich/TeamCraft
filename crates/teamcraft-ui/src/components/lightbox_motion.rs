//! The motion of the lightbox's stage: the picture follows the finger, and every release settles
//! with a spring that starts where the picture is and at the finger's velocity
//! ([`crate::lightbox::physics`]).
//!
//! - A sideways drag on a fitted picture moves the strip of previous, current and next picture; a
//!   trackpad's horizontal scroll does the same.
//! - An up or down drag lifts the picture off: it shrinks and the backdrop fades with the distance,
//!   and the release closes the lightbox or puts it back.
//! - A zoomed picture pans and pinches past its limits with resistance, glides after a flick, and
//!   settles back inside.
//! - The picture grows out of the tile it was opened from and returns into it on close, when that
//!   tile is in view; otherwise it fades.
//!
//! Only `transform` and `opacity` change per frame, written through the CSSOM (the web CSP allows no
//! inline styles), and a frame is requested only while something moves. Every motion can be
//! grabbed mid-flight. With reduced motion nothing moves: gestures decide on release and the change
//! is instant (the stylesheet cross-fades the new picture).

use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::components::lightbox::{prefers_reduced_motion, sizes};
use crate::lightbox::physics::{
  Animated, DRAG_HYSTERESIS_PX, DragAxis, PAGE_GAP_PX, PageOutcome, Rect, VelocityTracker, contained_size,
  dismiss_backdrop_opacity, dismiss_progress, dismiss_scale, drag_axis, flip_from, page_offset, page_outcome,
  page_target, rubberband_clamp, rubberband_scale, should_dismiss, unrubberband_clamp, unrubberband_scale,
};
use crate::lightbox::{Direction, Gesture, MediaKey, Point, Size, Viewport};

/// An opening picture that took longer than this to show is no longer connected to its tile: it
/// fades in instead of growing out of it.
const OPEN_FROM_TILE_MAX_MS: f64 = 400.0;
/// A trackpad's horizontal scroll has ended when no wheel event came for this long.
const WHEEL_END_MS: u64 = 120;
/// A page turn or a close is done when the picture is this close to where it goes; the rest of the
/// spring's tail is invisible.
const ARRIVED_PX: f64 = 1.0;
const ARRIVED_FRACTION: f64 = 0.01;
/// What the current slot's picture is, whichever is shown (the placeholder, the file, a video).
const CURRENT_MEDIA: &str =
  ".lightbox-slot.is-current .lightbox-image, .lightbox-slot.is-current .lightbox-video";

/// Drives the stage. Cheap to copy into event handlers; its state lives with the lightbox.
#[derive(Clone, Copy)]
pub struct StageMotion {
  inner: StoredValue<Inner, LocalStorage>,
  nodes: MotionNodes,
  hooks: MotionHooks,
  viewport: RwSignal<Viewport>,
  is_live: RwSignal<bool>,
}

/// The elements the motion writes to.
#[derive(Clone, Copy)]
pub struct MotionNodes {
  /// `.lightbox`: hides its chrome while a picture is lifted off.
  pub root: NodeRef<html::Div>,
  /// `.lightbox-backdrop`: fades while a picture is lifted off.
  pub backdrop: NodeRef<html::Div>,
  pub stage: NodeRef<html::Div>,
  /// `.lightbox-track`: holds the previous, current and next slot and moves as one.
  pub track: NodeRef<html::Div>,
  /// The full picture, for its size.
  pub image: NodeRef<html::Img>,
}

/// What the motion does to the lightbox when a gesture commits.
#[derive(Clone, Copy)]
pub struct MotionHooks {
  pub go: Callback<Direction>,
  pub close: Callback<()>,
  /// Whether there is a previous and a next picture.
  pub neighbours: Signal<(bool, bool)>,
  pub current: Signal<MediaKey>,
}

struct Inner {
  mode: Mode,
  velocity: VelocityTracker,
  /// Where the current press went down, in stage coordinates.
  press: Point,
  /// The track's offset when the press went down: a picture caught mid-flight moves on from there.
  track_at_press: Point,
  /// The offset the drag asks for: shown as it is, except with reduced motion, where only the
  /// release reads it.
  drag_offset: Point,
  /// Whether the press caught something moving: then its release is no tap.
  has_caught: bool,
  /// The page turn the press caught, which its release finishes or takes back.
  maybe_caught_page: Option<Direction>,
  track: TrackPose,
  view: ViewMotion,
  pending: Pending,
  is_frame_requested: bool,
  maybe_wheel_end: Option<TimeoutHandle>,
  last_wheel_ms: f64,
  opened_at_ms: f64,
  has_shown: bool,
  is_reduced: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
  Idle,
  /// Pressed, not moved past the hysteresis yet.
  Pressed,
  Page,
  Dismiss,
  /// Panning or pinching the picture itself.
  Zoom,
  /// A trackpad's horizontal scroll pages.
  Wheel,
}

/// What happens once the track arrives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
  None,
  Page(Direction),
  /// `into_tile`: the picture flies into its tile, so nothing is left for the exit fade.
  Close {
    into_tile: bool,
  },
}

/// The track's transform, the picture's opacity and the backdrop's opacity.
struct TrackPose {
  x: Animated,
  y: Animated,
  scale: Animated,
  content: Animated,
  backdrop: Animated,
}

/// The zoom and pan of the picture while a gesture or its settling owns them.
struct ViewMotion {
  scale: Animated,
  x: Animated,
  y: Animated,
  /// The pinch zoom and pan before rubber-banding.
  raw_scale: f64,
  raw_offset: Point,
  /// The last pinch centre: a zoom that settles keeps it in place.
  anchor: Point,
}

/// One frame's values for the DOM.
struct Paint {
  translate: Point,
  scale: f64,
  content: f64,
  backdrop: f64,
  is_lifted: bool,
  /// Scaled or moved off the paging axis (opening, closing, lifted): the neighbours stay hidden and
  /// the picture may leave the stage on its way to the tile.
  is_flying: bool,
  maybe_view: Option<(f64, Point)>,
}

/// What a release or a frame asks of the lightbox, done once the motion's state is released.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Commit {
  None,
  Go(Direction),
  Close { into_tile: bool },
}

impl StageMotion {
  pub fn new(nodes: MotionNodes, hooks: MotionHooks, viewport: RwSignal<Viewport>) -> Self {
    Self {
      inner: StoredValue::new_local(Inner::new(now_ms())),
      nodes,
      hooks,
      viewport,
      is_live: RwSignal::new(false),
    }
  }

  /// Whether a gesture or its settling owns the picture's transform: the stylesheet's own
  /// transition must not trail behind it then.
  pub fn is_live(&self) -> Signal<bool> {
    self.is_live.into()
  }

  /// A pointer went down on the stage; `pointers` are now pressed. Catches whatever moves.
  pub fn press(self, at: Point, time_ms: f64, pointers: usize) {
    let is_reduced = prefers_reduced_motion();
    let viewport = self.viewport.get_untracked();
    let stage = stage_size(self.nodes);
    self.inner.update_value(|inner| {
      inner.is_reduced = is_reduced;
      let is_view_caught = inner.view.is_moving();
      inner.has_caught = inner.pending != Pending::None || inner.track.is_moving() || is_view_caught;
      inner.maybe_caught_page = match inner.pending {
        Pending::Page(direction) => Some(direction),
        _ => None,
      };
      // Grabbed on its way out (or to the neighbour): it stays and follows the finger again.
      inner.pending = Pending::None;
      inner.track.stop();
      inner.view.stop();
      inner.cancel_wheel();
      inner.press = at;
      inner.track_at_press = Point::new(inner.track.x.value(), inner.track.y.value());
      inner.drag_offset = inner.track_at_press;
      inner.velocity.reset(time_ms, at);
      // A zoom caught while settling (also below fit) stays the finger's to pan and pinch.
      inner.mode = match (pointers, viewport.is_zoomed() || is_view_caught) {
        (1, false) => Mode::Pressed,
        _ => {
          // A second finger on a picture that was paging or lifted: the track goes back.
          inner.settle_track(Point::ORIGIN, time_ms);
          inner.begin_zoom(&viewport, image_size(self.nodes), stage);
          Mode::Zoom
        },
      };
    });
    self.is_live.set(true);
    self.request_frame();
  }

  /// A pressed pointer moved to `at`, which the pointers turned into `gesture`.
  pub fn drag(self, gesture: Gesture, at: Point, time_ms: f64) {
    let viewport = self.viewport.get_untracked();
    let (image, stage) = sizes(self.nodes.image, self.nodes.stage);
    let (has_previous, has_next) = self.hooks.neighbours.get_untracked();
    let page_width = stage.width + PAGE_GAP_PX;
    let maybe_paint = self.inner.try_update_value(|inner| {
      if let Gesture::Pinch { .. } = gesture
        && inner.mode != Mode::Zoom
      {
        // A second finger turns a page drag or a lift into a pinch; the track goes back.
        inner.settle_track(Point::ORIGIN, time_ms);
        inner.begin_zoom(&viewport, image, stage);
        inner.mode = Mode::Zoom;
      }
      if let Gesture::Pan(_) = gesture {
        inner.velocity.push(time_ms, at);
      }
      if inner.mode == Mode::Pressed {
        let travel = at.minus(inner.press);
        inner.mode = match drag_axis(travel) {
          Some(DragAxis::Horizontal) => Mode::Page,
          Some(DragAxis::Vertical) => Mode::Dismiss,
          None => Mode::Pressed,
        };
        if inner.mode != Mode::Pressed {
          // The hysteresis is no movement: the picture starts following from here, without a jump.
          let share = DRAG_HYSTERESIS_PX / travel.x.hypot(travel.y);
          inner.press = Point::new(inner.press.x + travel.x * share, inner.press.y + travel.y * share);
        }
      }
      let travel = at.minus(inner.press);
      let wanted = Point::new(
        inner.track_at_press.x + travel.x,
        inner.track_at_press.y + travel.y,
      );
      match (inner.mode, gesture) {
        (Mode::Page, _) => {
          inner.drag_offset = Point::new(wanted.x, 0.0);
          if !inner.is_reduced {
            inner
              .track
              .x
              .set(page_offset(wanted.x, page_width, has_previous, has_next));
          }
        },
        (Mode::Dismiss, _) => {
          inner.drag_offset = wanted;
          if !inner.is_reduced {
            inner.lift(wanted, stage.height);
          }
        },
        (Mode::Zoom, Gesture::Pan(delta)) => inner.pan(delta, &viewport, image, stage),
        (Mode::Zoom, Gesture::Pinch { factor, center }) => {
          inner.pinch(factor, center, &viewport, image, stage)
        },
        _ => return None,
      }
      Some(inner.paint())
    });
    if let Some(Some(paint)) = maybe_paint {
      self.apply(&paint);
      self.request_frame();
    }
  }

  /// A pointer was released at `time_ms`, `pointers` are still pressed. Returns whether the press
  /// caught something moving: then it is no tap (drags are no taps anyway, see [`Pointers`]).
  ///
  /// [`Pointers`]: crate::lightbox::Pointers
  pub fn release(self, time_ms: f64, pointers: usize) -> bool {
    let viewport = self.viewport.get_untracked();
    let (image, stage) = sizes(self.nodes.image, self.nodes.stage);
    let (has_previous, has_next) = self.hooks.neighbours.get_untracked();
    let page_width = stage.width + PAGE_GAP_PX;
    let now = now_ms();
    let Some((was_gesture, commit, close_velocity)) = self.inner.try_update_value(|inner| {
      let was_gesture = inner.has_caught;
      if pointers > 0 {
        // One finger of a pinch lifted: the other one pans on from here.
        inner.velocity.clear();
        return (was_gesture, Commit::None, None);
      }
      let velocity = inner.velocity.velocity(time_ms);
      let (commit, close_velocity) = match inner.mode {
        Mode::Page | Mode::Wheel => (
          inner.release_page(velocity.x, page_width, has_previous, has_next, time_ms),
          None,
        ),
        Mode::Dismiss => {
          let offset = inner.drag_offset;
          if should_dismiss(offset.y, velocity.y, stage.height) {
            (Commit::Close { into_tile: false }, Some(velocity))
          } else {
            inner.settle_track(velocity, time_ms);
            (Commit::None, None)
          }
        },
        Mode::Zoom => {
          inner.settle_view(velocity, &viewport, image, stage, time_ms);
          (Commit::None, None)
        },
        // Caught mid-flight and let go: a page turn finishes or goes back, a lift goes back.
        Mode::Pressed | Mode::Idle => (
          inner.release_caught(page_width, has_previous, has_next, now),
          None,
        ),
      };
      inner.mode = Mode::Idle;
      (was_gesture, commit, close_velocity)
    }) else {
      return false;
    };
    match (commit, close_velocity) {
      (Commit::Close { .. }, maybe_velocity) => self.close_with(maybe_velocity.unwrap_or(Point::ORIGIN)),
      (Commit::Go(direction), _) => {
        self.hooks.go.run(direction);
        self.request_frame();
      },
      (Commit::None, _) => self.request_frame(),
    }
    was_gesture
  }

  /// The browser took the pointer over: everything goes back, nothing commits.
  pub fn cancel(self) {
    let viewport = self.viewport.get_untracked();
    let (image, stage) = sizes(self.nodes.image, self.nodes.stage);
    let now = now_ms();
    self.inner.update_value(|inner| {
      match inner.mode {
        Mode::Zoom => inner.settle_view(Point::ORIGIN, &viewport, image, stage, now),
        _ => inner.settle_track(Point::ORIGIN, now),
      }
      inner.mode = Mode::Idle;
    });
    self.request_frame();
  }

  /// A wheel event of `delta` pixels. A mostly horizontal one on a fitted picture (a trackpad's
  /// two-finger swipe) pages like a drag; returns whether it did.
  pub fn wheel(self, delta: Point, time_ms: f64) -> bool {
    if self.viewport.get_untracked().is_zoomed() {
      return false;
    }
    let (has_previous, has_next) = self.hooks.neighbours.get_untracked();
    let page_width = stage_size(self.nodes).width + PAGE_GAP_PX;
    let is_reduced = prefers_reduced_motion();
    let maybe_paint = self.inner.try_update_value(|inner| {
      let is_sideways = delta.x.abs() > delta.y.abs();
      match inner.mode {
        // Not while the picture opens, closes or settles from a lift: that motion isn't the strip's.
        Mode::Idle if is_sideways && !inner.is_flying() => {
          inner.is_reduced = is_reduced;
          inner.pending = Pending::None;
          inner.track.stop();
          inner.drag_offset = Point::new(inner.track.x.value(), 0.0);
          inner.velocity.reset(time_ms, inner.drag_offset);
          inner.mode = Mode::Wheel;
        },
        Mode::Wheel => {},
        _ => return None,
      }
      inner.drag_offset.x -= delta.x;
      inner.velocity.push(time_ms, inner.drag_offset);
      inner.last_wheel_ms = time_ms;
      if !inner.is_reduced {
        inner.track.x.set(page_offset(
          inner.drag_offset.x,
          page_width,
          has_previous,
          has_next,
        ));
      }
      inner.cancel_wheel();
      Some(inner.paint())
    });
    let Some(Some(paint)) = maybe_paint else {
      return false;
    };
    self.is_live.set(true);
    self.apply(&paint);
    let handle = set_timeout_with_handle(
      move || self.end_wheel(),
      std::time::Duration::from_millis(WHEEL_END_MS),
    );
    if let Ok(handle) = handle {
      self
        .inner
        .update_value(|inner| inner.maybe_wheel_end = Some(handle));
    }
    true
  }

  /// The current slot's picture is on screen (its `load`): the first time, it grows out of its tile.
  pub fn shown(self) {
    let is_reduced = prefers_reduced_motion();
    let now = now_ms();
    let Some(is_first) = self.inner.try_update_value(|inner| {
      let is_first = !inner.has_shown;
      inner.has_shown = true;
      is_first && !is_reduced && now - inner.opened_at_ms <= OPEN_FROM_TILE_MAX_MS
    }) else {
      return;
    };
    let Some(track) = self.nodes.track.get_untracked() else {
      return;
    };
    let was_pending = track.class_list().contains("is-pending");
    drop(track.class_list().remove_1("is-pending"));
    if !was_pending {
      return;
    }
    let maybe_flip = is_first.then(|| self.flip_to_tile()).flatten();
    let Some((offset, scale)) = maybe_flip else {
      drop(track.class_list().add_1("is-fading-in"));
      return;
    };
    let Some(paint) = self.inner.try_update_value(|inner| {
      inner.track.x.set(offset.x);
      inner.track.y.set(offset.y);
      inner.track.scale.set(scale);
      inner.track.backdrop.set(0.0);
      let paint = inner.paint();
      inner.settle_track(Point::ORIGIN, now);
      paint
    }) else {
      return;
    };
    // The first frame already shows it over the tile.
    self.apply(&paint);
    self.is_live.set(true);
    self.request_frame();
  }

  /// The lightbox mounted: its backdrop fades in (springs, so a picture growing out of its tile can
  /// take it over without a jump).
  pub fn enter(self) {
    if prefers_reduced_motion() {
      return;
    }
    let now = now_ms();
    let Some(paint) = self.inner.try_update_value(|inner| {
      inner.track.backdrop.set(0.0);
      let paint = inner.paint();
      inner.track.backdrop.spring_to(1.0, 0.0, now);
      paint
    }) else {
      return;
    };
    self.apply(&paint);
    self.request_frame();
  }

  /// Closes the lightbox: into the tile of the picture when it is in view, else it fades.
  pub fn close(self) {
    self.close_with(Point::ORIGIN);
  }

  /// Back to rest at once: the picture changed by a button, a key or the strip.
  pub fn reset(self) {
    let maybe_paint = self.inner.try_update_value(|inner| {
      if matches!(inner.pending, Pending::Close { .. }) {
        return None;
      }
      inner.pending = Pending::None;
      inner.mode = Mode::Idle;
      inner.track.rest();
      inner.view.stop();
      Some(inner.paint())
    });
    if let Some(Some(paint)) = maybe_paint {
      self.apply(&paint);
      self.is_live.try_set(false);
    }
  }

  /// The wheel, a key or a button zooms: a settling zoom gives way.
  pub fn stop_view(self) {
    self.inner.update_value(|inner| inner.view.stop());
  }

  fn close_with(self, velocity: Point) {
    if prefers_reduced_motion() {
      // The dialog's exit fade (if any) runs as usual.
      self.hooks.close.run(());
      return;
    }
    let maybe_flip = self.flip_to_tile();
    let now = now_ms();
    self.inner.update_value(|inner| {
      inner.mode = Mode::Idle;
      inner.pending = Pending::Close {
        into_tile: maybe_flip.is_some(),
      };
      inner.cancel_wheel();
      let track = &mut inner.track;
      match maybe_flip {
        Some((offset, scale)) => {
          track.x.spring_to(offset.x, velocity.x, now);
          track.y.spring_to(offset.y, velocity.y, now);
          track.scale.spring_to(scale, 0.0, now);
        },
        None => track.content.spring_to(0.0, 0.0, now),
      }
      track.backdrop.spring_to(0.0, 0.0, now);
      inner.view.settle_to(1.0, Point::ORIGIN, Point::ORIGIN, now);
    });
    self.is_live.set(true);
    self.request_frame();
  }

  /// The transform that lays the fitted current picture over its tile, `None` when the tile is not
  /// in view (scrolled away, or not rendered).
  fn flip_to_tile(self) -> Option<(Point, f64)> {
    let tile = tile_rect(self.hooks.current.get_untracked())?;
    let stage = self.nodes.stage.get_untracked()?;
    let media = self
      .nodes
      .track
      .get_untracked()?
      .query_selector(CURRENT_MEDIA)
      .ok()
      .flatten()?
      .dyn_into::<web_sys::HtmlElement>()
      .ok()?;
    let laid_out = Size::new(f64::from(media.offset_width()), f64::from(media.offset_height()));
    let stage_size = Size::new(f64::from(stage.client_width()), f64::from(stage.client_height()));
    // A picture may be turned; the placeholder and a video letterbox their picture in their box.
    let classes = media.class_list();
    let is_picture = classes.contains("lightbox-image") && !classes.contains("lightbox-placeholder");
    let fitted = match is_picture {
      true => self.viewport.get_untracked().fitted_size(laid_out, stage_size),
      false => contained_size(laid_out, natural_size(&media)),
    };
    let center = rect_of(&stage).center();
    let shown = Rect::new(
      center.x - fitted.width / 2.0,
      center.y - fitted.height / 2.0,
      fitted.width,
      fitted.height,
    );
    let (offset, scale) = flip_from(tile, shown);
    (scale > 0.0 && scale.is_finite()).then_some((offset, scale))
  }

  fn end_wheel(self) {
    let (has_previous, has_next) = self
      .hooks
      .neighbours
      .try_get_untracked()
      .unwrap_or((false, false));
    let page_width = self
      .nodes
      .stage
      .try_get_untracked()
      .flatten()
      .map_or(0.0, |stage| f64::from(stage.client_width()))
      + PAGE_GAP_PX;
    let now = now_ms();
    let Some(commit) = self.inner.try_update_value(|inner| {
      inner.maybe_wheel_end = None;
      if inner.mode != Mode::Wheel {
        return Commit::None;
      }
      inner.mode = Mode::Idle;
      let velocity = inner.velocity.velocity(inner.last_wheel_ms);
      inner.release_page(velocity.x, page_width, has_previous, has_next, now)
    }) else {
      return;
    };
    if let Commit::Go(direction) = commit {
      self.hooks.go.try_run(direction);
    }
    self.request_frame();
  }

  fn request_frame(self) {
    let should_request = self
      .inner
      .try_update_value(|inner| !std::mem::replace(&mut inner.is_frame_requested, true))
      .unwrap_or(false);
    if should_request {
      request_animation_frame(move || self.frame());
    }
  }

  /// Advances every motion to now, paints, and commits a page turn or a close that arrived.
  fn frame(self) {
    let now = now_ms();
    let Some((paint, commit, is_moving, is_held)) = self.inner.try_update_value(|inner| {
      inner.is_frame_requested = false;
      let is_track_moving = inner.track.tick(now);
      let is_view_moving = inner.view.tick(now);
      let commit = inner.arrival();
      let paint = inner.paint();
      let is_held = inner.mode != Mode::Idle;
      (paint, commit, is_track_moving || is_view_moving, is_held)
    }) else {
      return;
    };
    self.apply(&paint);
    match commit {
      Commit::Go(direction) => {
        self.hooks.go.try_run(direction);
      },
      Commit::Close { into_tile } => {
        // The picture is in its tile: the dialog's exit fade would only show it once more.
        self.set_root_class("is-closed", into_tile);
        self.hooks.close.try_run(());
        return;
      },
      Commit::None => {},
    }
    if is_moving {
      self.request_frame();
    } else if !is_held {
      self.is_live.try_set(false);
    }
  }

  fn apply(self, paint: &Paint) {
    if let Some(track) = self.nodes.track.try_get_untracked().flatten() {
      let style = web_sys::HtmlElement::style(&track);
      let transform = format!(
        "translate3d({:.2}px, {:.2}px, 0) scale({:.4})",
        paint.translate.x, paint.translate.y, paint.scale
      );
      // CSSOM writes only fail for unknown properties; these are fixed and valid.
      drop(style.set_property("transform", &transform));
      drop(style.set_property("opacity", &format!("{:.3}", paint.content)));
    }
    if let Some(backdrop) = self.nodes.backdrop.try_get_untracked().flatten() {
      drop(web_sys::HtmlElement::style(&backdrop).set_property("opacity", &format!("{:.3}", paint.backdrop)));
    }
    self.set_root_class("is-lifted", paint.is_lifted);
    self.set_root_class("is-flying", paint.is_flying);
    if let Some((scale, offset)) = paint.maybe_view {
      self
        .viewport
        .try_update(|viewport| viewport.present(scale, offset));
    }
  }

  fn set_root_class(self, class: &str, is_on: bool) {
    if let Some(root) = self.nodes.root.try_get_untracked().flatten() {
      drop(root.class_list().toggle_with_force(class, is_on));
    }
  }
}

impl Inner {
  fn new(now: f64) -> Self {
    Self {
      mode: Mode::Idle,
      velocity: VelocityTracker::default(),
      press: Point::ORIGIN,
      track_at_press: Point::ORIGIN,
      drag_offset: Point::ORIGIN,
      has_caught: false,
      maybe_caught_page: None,
      track: TrackPose::new(),
      view: ViewMotion::new(),
      pending: Pending::None,
      is_frame_requested: false,
      maybe_wheel_end: None,
      last_wheel_ms: 0.0,
      opened_at_ms: now,
      has_shown: false,
      is_reduced: false,
    }
  }

  /// A page drag released with `velocity`: the strip springs to the neighbour or back. With
  /// reduced motion the turn is instant.
  fn release_page(
    &mut self,
    velocity: f64,
    page_width: f64,
    has_previous: bool,
    has_next: bool,
    now: f64,
  ) -> Commit {
    let outcome = page_outcome(self.drag_offset.x, velocity, page_width, has_previous, has_next);
    if self.is_reduced {
      self.track.rest();
      return match outcome {
        PageOutcome::Turn(direction) => Commit::Go(direction),
        PageOutcome::Stay => Commit::None,
      };
    }
    self
      .track
      .x
      .spring_to(page_target(outcome, page_width), velocity, now);
    if let PageOutcome::Turn(direction) = outcome {
      self.pending = Pending::Page(direction);
    }
    Commit::None
  }

  /// A press that caught the track and let go without dragging.
  fn release_caught(&mut self, page_width: f64, has_previous: bool, has_next: bool, now: f64) -> Commit {
    if !self.has_caught {
      return Commit::None;
    }
    // Only a caught page turn may still turn; anything else (an open, a close, a lift) goes back.
    if self.maybe_caught_page.is_none() {
      self.settle_track(Point::ORIGIN, now);
      return Commit::None;
    }
    let commit = self.release_page(0.0, page_width, has_previous, has_next, now);
    let track = &mut self.track;
    track.y.spring_to(0.0, 0.0, now);
    track.scale.spring_to(1.0, 0.0, now);
    track.content.spring_to(1.0, 0.0, now);
    track.backdrop.spring_to(1.0, 0.0, now);
    commit
  }

  /// The picture lifted to `offset`: it shrinks and the backdrop fades with the distance.
  fn lift(&mut self, offset: Point, stage_height: f64) {
    let progress = dismiss_progress(offset.y, stage_height);
    self.track.x.set(offset.x);
    self.track.y.set(offset.y);
    self.track.scale.set(dismiss_scale(progress));
    self.track.backdrop.set(dismiss_backdrop_opacity(progress));
  }

  /// Everything about the track springs back to rest, the offset with `velocity`.
  fn settle_track(&mut self, velocity: Point, now: f64) {
    let track = &mut self.track;
    track.x.spring_to(0.0, velocity.x, now);
    track.y.spring_to(0.0, velocity.y, now);
    track.scale.spring_to(1.0, 0.0, now);
    track.content.spring_to(1.0, 0.0, now);
    track.backdrop.spring_to(1.0, 0.0, now);
  }

  /// A pan or pinch starts from what is on screen, which may be past the limits.
  fn begin_zoom(&mut self, viewport: &Viewport, image: Size, stage: Size) {
    let (scale, offset) = (viewport.scale(), viewport.offset());
    let limits = viewport.pan_limits(scale, image, stage);
    let view = &mut self.view;
    view.stop();
    view.scale.set(scale);
    view.x.set(offset.x);
    view.y.set(offset.y);
    view.raw_scale = unrubberband_scale(scale, 1.0, viewport.max_scale());
    view.raw_offset = Point::new(
      unrubberband_clamp(offset.x, -limits.x, limits.x, stage.width),
      unrubberband_clamp(offset.y, -limits.y, limits.y, stage.height),
    );
    view.anchor = Point::ORIGIN;
    self.velocity.clear();
  }

  fn pan(&mut self, delta: Point, viewport: &Viewport, image: Size, stage: Size) {
    let view = &mut self.view;
    view.raw_offset = Point::new(view.raw_offset.x + delta.x, view.raw_offset.y + delta.y);
    let scale = view.scale.value();
    self.show_offset(viewport.pan_limits(scale, image, stage), stage);
  }

  fn pinch(&mut self, factor: f64, center: Point, viewport: &Viewport, image: Size, stage: Size) {
    if !factor.is_finite() || factor <= 0.0 {
      return;
    }
    // Pinch velocity is no pan velocity.
    self.velocity.clear();
    let max = viewport.max_scale();
    let view = &mut self.view;
    let before = view.scale.value();
    view.raw_scale *= factor;
    let scale = match self.is_reduced {
      true => view.raw_scale.clamp(1.0, max),
      false => rubberband_scale(view.raw_scale, 1.0, max),
    };
    let applied = scale / before;
    let offset = Point::new(
      center.x - (center.x - view.x.value()) * applied,
      center.y - (center.y - view.y.value()) * applied,
    );
    let limits = viewport.pan_limits(scale, image, stage);
    view.scale.set(scale);
    view.anchor = center;
    view.raw_offset = Point::new(
      unrubberband_clamp(offset.x, -limits.x, limits.x, stage.width),
      unrubberband_clamp(offset.y, -limits.y, limits.y, stage.height),
    );
    self.show_offset(limits, stage);
  }

  /// The raw pan, resisted past `limits` (held at them with reduced motion).
  fn show_offset(&mut self, limits: Point, stage: Size) {
    let view = &mut self.view;
    let (x, y) = match self.is_reduced {
      true => (
        view.raw_offset.x.clamp(-limits.x, limits.x),
        view.raw_offset.y.clamp(-limits.y, limits.y),
      ),
      false => (
        rubberband_clamp(view.raw_offset.x, -limits.x, limits.x, stage.width),
        rubberband_clamp(view.raw_offset.y, -limits.y, limits.y, stage.height),
      ),
    };
    view.x.set(x);
    view.y.set(y);
    if self.is_reduced {
      view.raw_offset = Point::new(x, y);
    }
  }

  /// A released pan or pinch: back inside the limits, or on with the flick's momentum.
  fn settle_view(&mut self, velocity: Point, viewport: &Viewport, image: Size, stage: Size, now: f64) {
    let mut shown = *viewport;
    shown.present(
      self.view.scale.value(),
      Point::new(self.view.x.value(), self.view.y.value()),
    );
    let (scale, offset) = shown.settled(self.view.anchor, image, stage);
    if self.is_reduced {
      self.view.settle_to(scale, offset, Point::ORIGIN, f64::NAN);
      return;
    }
    let is_in_limits = (scale - self.view.scale.value()).abs() < 1e-9;
    if !is_in_limits {
      self.view.settle_to(scale, offset, velocity, now);
      return;
    }
    let limits = viewport.pan_limits(scale, image, stage);
    self.view.x.glide(velocity.x, -limits.x, limits.x, now);
    self.view.y.glide(velocity.y, -limits.y, limits.y, now);
  }

  /// A page turn or a close whose track arrived.
  fn arrival(&mut self) -> Commit {
    match self.pending {
      Pending::Page(direction) if self.track.x.is_near(ARRIVED_PX) => {
        // The neighbour is now in the middle: it becomes the current picture where it is.
        self.pending = Pending::None;
        self.track.rest();
        Commit::Go(direction)
      },
      Pending::Close { into_tile } if self.track.has_arrived() => Commit::Close { into_tile },
      _ => Commit::None,
    }
  }

  fn paint(&self) -> Paint {
    let track = &self.track;
    let view = &self.view;
    let is_view_owned = self.mode == Mode::Zoom || view.is_moving();
    Paint {
      translate: Point::new(track.x.value(), track.y.value()),
      scale: track.scale.value(),
      content: track.content.value(),
      backdrop: track.backdrop.value(),
      is_lifted: self.mode == Mode::Dismiss || matches!(self.pending, Pending::Close { .. }),
      is_flying: self.is_flying(),
      maybe_view: is_view_owned.then(|| (view.scale.value(), Point::new(view.x.value(), view.y.value()))),
    }
  }

  /// Whether the track is off the paging axis: lifted, opening or closing.
  fn is_flying(&self) -> bool {
    let track = &self.track;
    matches!(self.pending, Pending::Close { .. })
      || [&track.y, &track.scale, &track.content, &track.backdrop]
        .iter()
        .any(|value| value.is_moving())
      || track.y.value().abs() > 0.5
      || (track.scale.value() - 1.0).abs() > 0.001
  }

  fn cancel_wheel(&mut self) {
    if let Some(handle) = self.maybe_wheel_end.take() {
      handle.clear();
    }
  }
}

impl TrackPose {
  fn new() -> Self {
    Self {
      x: Animated::new(0.0),
      y: Animated::new(0.0),
      scale: Animated::fraction(1.0),
      content: Animated::fraction(1.0),
      backdrop: Animated::fraction(1.0),
    }
  }

  fn all_mut(&mut self) -> [&mut Animated; 5] {
    [
      &mut self.x,
      &mut self.y,
      &mut self.scale,
      &mut self.content,
      &mut self.backdrop,
    ]
  }

  fn is_moving(&self) -> bool {
    [&self.x, &self.y, &self.scale, &self.content, &self.backdrop]
      .iter()
      .any(|value| value.is_moving())
  }

  fn tick(&mut self, now: f64) -> bool {
    // Every value advances; none may be skipped because another one already moves.
    let mut is_moving = false;
    for value in self.all_mut() {
      is_moving |= value.tick(now);
    }
    is_moving
  }

  fn stop(&mut self) {
    for value in self.all_mut() {
      value.stop();
    }
  }

  fn rest(&mut self) {
    self.x.set(0.0);
    self.y.set(0.0);
    self.scale.set(1.0);
    self.content.set(1.0);
    self.backdrop.set(1.0);
  }

  /// Close enough to where a close takes it.
  fn has_arrived(&self) -> bool {
    self.x.is_near(ARRIVED_PX)
      && self.y.is_near(ARRIVED_PX)
      && self.scale.is_near(ARRIVED_FRACTION)
      && self.content.is_near(ARRIVED_FRACTION)
      && self.backdrop.is_near(ARRIVED_FRACTION)
  }
}

impl ViewMotion {
  fn new() -> Self {
    Self {
      scale: Animated::fraction(1.0),
      x: Animated::new(0.0),
      y: Animated::new(0.0),
      raw_scale: 1.0,
      raw_offset: Point::ORIGIN,
      anchor: Point::ORIGIN,
    }
  }

  fn is_moving(&self) -> bool {
    self.scale.is_moving() || self.x.is_moving() || self.y.is_moving()
  }

  fn tick(&mut self, now: f64) -> bool {
    let scale = self.scale.tick(now);
    let x = self.x.tick(now);
    let y = self.y.tick(now);
    scale || x || y
  }

  fn stop(&mut self) {
    self.scale.stop();
    self.x.stop();
    self.y.stop();
  }

  /// Springs to `scale` and `offset`; a `now` that is not a number jumps there.
  fn settle_to(&mut self, scale: f64, offset: Point, velocity: Point, now: f64) {
    if now.is_nan() {
      self.scale.set(scale);
      self.x.set(offset.x);
      self.y.set(offset.y);
      return;
    }
    self.scale.spring_to(scale, 0.0, now);
    self.x.spring_to(offset.x, velocity.x, now);
    self.y.spring_to(offset.y, velocity.y, now);
  }
}

/// Where the tile of `key` is, if one is fully in view outside the lightbox (the conversation's
/// tile, or a file card's thumbnail).
fn tile_rect(key: MediaKey) -> Option<Rect> {
  let window = web_sys::window()?;
  let document = window.document()?;
  let selector = format!("[data-media-key=\"{}\"]", key.dom_key());
  let found = document.query_selector_all(&selector).ok()?;
  (0..found.length())
    .filter_map(|index| found.item(index))
    .filter_map(|node| node.dyn_into::<web_sys::Element>().ok())
    .filter(|element| element.closest(".modal-lightbox").ok().flatten().is_none())
    .find_map(|element| {
      let rect = rect_of(&element);
      rect.is_inside(visible_bounds(&window, &element)).then_some(rect)
    })
}

/// The part of the window an element can be seen in: the window, cut by every scrolling or
/// clipping ancestor (a tile under the composer is out of view).
fn visible_bounds(window: &web_sys::Window, element: &web_sys::Element) -> Rect {
  let width = window
    .inner_width()
    .ok()
    .and_then(|width| width.as_f64())
    .unwrap_or(0.0);
  let height = window
    .inner_height()
    .ok()
    .and_then(|height| height.as_f64())
    .unwrap_or(0.0);
  let mut bounds = Rect::new(0.0, 0.0, width, height);
  let mut maybe_parent = element.parent_element();
  while let Some(parent) = maybe_parent {
    let is_clipping = window
      .get_computed_style(&parent)
      .ok()
      .flatten()
      .and_then(|style| style.get_property_value("overflow").ok())
      .is_some_and(|overflow| overflow != "visible");
    if is_clipping {
      bounds = bounds.intersect(rect_of(&parent));
    }
    maybe_parent = parent.parent_element();
  }
  bounds
}

/// The intrinsic size of a picture or video, zero before it is known.
fn natural_size(media: &web_sys::HtmlElement) -> Size {
  if let Some(image) = media.dyn_ref::<web_sys::HtmlImageElement>() {
    return Size::new(
      f64::from(image.natural_width()),
      f64::from(image.natural_height()),
    );
  }
  if let Some(video) = media.dyn_ref::<web_sys::HtmlVideoElement>() {
    return Size::new(f64::from(video.video_width()), f64::from(video.video_height()));
  }
  Size::default()
}

fn rect_of(element: &web_sys::Element) -> Rect {
  let rect = element.get_bounding_client_rect();
  Rect::new(rect.left(), rect.top(), rect.width(), rect.height())
}

fn stage_size(nodes: MotionNodes) -> Size {
  sizes(nodes.image, nodes.stage).1
}

fn image_size(nodes: MotionNodes) -> Size {
  sizes(nodes.image, nodes.stage).0
}

/// The clock of event time stamps and animation frames.
fn now_ms() -> f64 {
  web_sys::window()
    .and_then(|window| window.performance())
    .map_or(0.0, |performance| performance.now())
}
