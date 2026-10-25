//! How the toast stack moves (issue #371): toasts glide to their new places when one arrives or
//! leaves (FLIP: the stack is measured after the change and each toast is played from where it was
//! back to where it now is), and on touch a toast follows the finger sideways and is swiped away.
//! Both only move `translate`/`transform` and `opacity`. With reduced motion nothing glides; a
//! swipe still follows the finger (direct manipulation) and the toast fades instead of flying off.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use crate::motion::prefers_reduced_motion;

/// A swipe dismisses past this distance…
const DISMISS_DISTANCE_PX: f64 = 40.0;
/// …or when the finger leaves faster than this, in the same direction.
const DISMISS_VELOCITY_PX_PER_MS: f64 = 0.11;
/// Sideways movement that turns a touch into a swipe (smaller ones are taps).
const SWIPE_SLOP_PX: f64 = 8.0;
/// How far past its own width a swiped toast flies.
const FLY_MARGIN_PX: f64 = 24.0;
/// The swiped toast's exit (`--dur-exit`) plus a frame, before it is closed.
const SWIPE_EXIT: Duration = Duration::from_millis(120);
/// Moves smaller than this are not played.
const MIN_FLIP_PX: f64 = 1.0;

/// Glides the toasts of `stack` to their new places whenever one is added or removed. The observer
/// goes with the component.
pub fn use_flip_stack(stack: NodeRef<html::Div>) {
  let last_places = Rc::new(RefCell::new(Vec::<(web_sys::Element, f64)>::new()));
  Effect::new(move |_| {
    let Some(stack) = stack.get() else {
      return;
    };
    let stack: web_sys::HtmlElement = stack.unchecked_into();
    let last_places = last_places.clone();
    flip(&stack, &last_places);
    let on_change = Closure::<dyn FnMut()>::new({
      let stack = stack.clone();
      move || flip(&stack, &last_places)
    });
    let Ok(observer) = web_sys::MutationObserver::new(on_change.as_ref().unchecked_ref()) else {
      return;
    };
    let options = web_sys::MutationObserverInit::new();
    options.set_child_list(true);
    if observer.observe_with_options(&stack, &options).is_err() {
      tracing::debug!("could not observe the toast stack");
      return;
    }
    let handle = StoredValue::new_local((observer, on_change));
    on_cleanup(move || {
      handle.try_with_value(|(observer, _)| observer.disconnect());
    });
  });
}

/// Measures every toast and plays the ones that moved from their last place; remembers the new
/// places. A toast still gliding starts from where it is drawn, so a second change retargets.
fn flip(stack: &web_sys::HtmlElement, last_places: &RefCell<Vec<(web_sys::Element, f64)>>) {
  let top = stack.get_bounding_client_rect().top();
  let is_reduced = prefers_reduced_motion();
  let items = stack.children();
  let mut places = Vec::with_capacity(items.length() as usize);
  for index in 0..items.length() {
    let Some(item) = items
      .item(index)
      .and_then(|item| item.dyn_into::<web_sys::HtmlElement>().ok())
    else {
      continue;
    };
    let place = top + f64::from(item.offset_top());
    let maybe_last = last_places
      .borrow()
      .iter()
      .find(|(element, _)| element == item.as_ref())
      .map(|(_, last)| *last);
    if let Some(last) = maybe_last
      && !is_reduced
    {
      let from = last - place + drawn_offset_y(&item);
      if from.abs() >= MIN_FLIP_PX {
        play_from(&item, from);
      }
    }
    places.push((item.unchecked_into(), place));
  }
  *last_places.borrow_mut() = places;
}

/// Draws `item` `from_px` off its place without a transition, then lets it glide back
/// (`.toast`'s `translate` transition).
fn play_from(item: &web_sys::HtmlElement, from_px: f64) {
  let classes = item.class_list();
  let style = item.style();
  drop(classes.add_1("is-flipping"));
  drop(style.set_property("translate", &format!("0 {from_px:.2}px")));
  // Reading the layout draws the start position before the transition is switched back on.
  let _ = item.offset_height();
  drop(classes.remove_1("is-flipping"));
  drop(style.remove_property("translate"));
}

/// How far a gliding toast is drawn off its place right now.
fn drawn_offset_y(item: &web_sys::HtmlElement) -> f64 {
  web_sys::window()
    .and_then(|window| window.get_computed_style(item).ok().flatten())
    .and_then(|style| style.get_property_value("translate").ok())
    .map_or(0.0, |value| translate_y(&value))
}

/// The vertical part of a computed `translate` (`none`, `10px` or `0px 12px`).
fn translate_y(value: &str) -> f64 {
  value
    .split_whitespace()
    .nth(1)
    .and_then(|y| y.strip_suffix("px"))
    .and_then(|y| y.parse().ok())
    .unwrap_or(0.0)
}

/// Swipe-to-dismiss for one toast on touch: the toast follows the finger sideways and is closed
/// past [`DISMISS_DISTANCE_PX`] or a quick flick; otherwise it springs back. Mouse and pen do
/// nothing (they have the close button).
#[derive(Clone, Copy)]
pub struct Swipe {
  maybe_drag: StoredValue<Option<Drag>>,
}

impl Default for Swipe {
  fn default() -> Self {
    Self::new()
  }
}

/// One touch on a toast.
#[derive(Clone, Copy, Debug)]
struct Drag {
  pointer_id: i32,
  origin: (f64, f64),
  /// The two last samples of the move (x, time in ms), for the release velocity.
  last: (f64, f64),
  previous: (f64, f64),
  is_swiping: bool,
}

impl Swipe {
  pub fn new() -> Self {
    Self {
      maybe_drag: StoredValue::new(None),
    }
  }

  pub fn start(&self, ev: &web_sys::PointerEvent) {
    if ev.pointer_type() != "touch" {
      return;
    }
    let sample = (f64::from(ev.client_x()), ev.time_stamp());
    self.maybe_drag.set_value(Some(Drag {
      pointer_id: ev.pointer_id(),
      origin: (sample.0, f64::from(ev.client_y())),
      last: sample,
      previous: sample,
      is_swiping: false,
    }));
  }

  pub fn track(&self, ev: &web_sys::PointerEvent) {
    let Some(mut drag) = self.maybe_drag.get_value() else {
      return;
    };
    let Some(toast) = toast_of(ev) else {
      return;
    };
    let dx = f64::from(ev.client_x()) - drag.origin.0;
    let dy = f64::from(ev.client_y()) - drag.origin.1;
    if !drag.is_swiping {
      if dy.abs() > SWIPE_SLOP_PX && dy.abs() > dx.abs() {
        // A vertical move is the page's, not ours.
        self.maybe_drag.set_value(None);
        return;
      }
      if dx.abs() <= SWIPE_SLOP_PX {
        return;
      }
      drag.is_swiping = true;
      // Keeps the moves coming when the finger leaves the toast; a failed capture only means the
      // swipe ends there.
      drop(toast.set_pointer_capture(drag.pointer_id));
      drop(toast.class_list().add_1("is-dragging"));
    }
    drag.previous = drag.last;
    drag.last = (f64::from(ev.client_x()), ev.time_stamp());
    self.maybe_drag.set_value(Some(drag));
    let style = toast.style();
    drop(style.set_property("transform", &format!("translateX({dx:.1}px)")));
    drop(style.set_property(
      "opacity",
      &format!("{:.3}", swipe_opacity(dx, f64::from(toast.offset_width()))),
    ));
  }

  /// The finger lifted: closes the toast with `on_dismiss` if the swipe went far or fast enough.
  pub fn end(&self, ev: &web_sys::PointerEvent, on_dismiss: impl FnOnce() + 'static) {
    let Some(drag) = self.maybe_drag.get_value() else {
      return;
    };
    self.maybe_drag.set_value(None);
    let Some(toast) = toast_of(ev).filter(|_| drag.is_swiping) else {
      return;
    };
    let release = (f64::from(ev.client_x()), ev.time_stamp());
    let dx = release.0 - drag.origin.0;
    if is_dismissal(dx, release_velocity(drag.previous, release)) {
      fly_off(&toast, dx.signum(), on_dismiss);
    } else {
      spring_back(&toast);
    }
  }

  /// The touch was taken over (scroll, system gesture): the toast goes back.
  pub fn cancel(&self, ev: &web_sys::PointerEvent) {
    let was_swiping = self.maybe_drag.get_value().is_some_and(|drag| drag.is_swiping);
    self.maybe_drag.set_value(None);
    if was_swiping && let Some(toast) = toast_of(ev) {
      spring_back(&toast);
    }
  }
}

/// Whether a swipe that ended `dx_px` off its start, at `velocity` px/ms, dismisses.
fn is_dismissal(dx_px: f64, velocity: f64) -> bool {
  dx_px.abs() > DISMISS_DISTANCE_PX
    || (velocity.abs() > DISMISS_VELOCITY_PX_PER_MS && velocity.signum() == dx_px.signum())
}

/// Speed in px/ms from the sample before the last move to the release: a finger that stopped
/// before lifting has slowed down, one that flicked has not.
fn release_velocity(from: (f64, f64), release: (f64, f64)) -> f64 {
  let elapsed_ms = release.1 - from.1;
  if elapsed_ms > 0.0 {
    (release.0 - from.0) / elapsed_ms
  } else {
    0.0
  }
}

/// The toast fades as it is pulled away: half gone at its own width.
fn swipe_opacity(dx_px: f64, width_px: f64) -> f64 {
  if width_px <= 0.0 {
    return 1.0;
  }
  (1.0 - dx_px.abs() / width_px / 2.0).clamp(0.0, 1.0)
}

/// Flies the toast off in the swipe's direction (fades in place with reduced motion), then closes
/// it. It played its own exit, so it leaves no ghost.
fn fly_off(toast: &web_sys::HtmlElement, direction: f64, on_dismiss: impl FnOnce() + 'static) {
  let classes = toast.class_list();
  drop(classes.remove_1("is-dragging"));
  drop(classes.add_2("is-swiped", "has-exited"));
  let style = toast.style();
  if !prefers_reduced_motion() {
    let distance = f64::from(toast.offset_width()) + FLY_MARGIN_PX;
    drop(style.set_property("transform", &format!("translateX({:.1}px)", direction * distance)));
  }
  drop(style.set_property("opacity", "0"));
  set_timeout(on_dismiss, SWIPE_EXIT);
}

/// Lets go of a toast that wasn't swiped far enough: it glides back into place.
fn spring_back(toast: &web_sys::HtmlElement) {
  drop(toast.class_list().remove_1("is-dragging"));
  let style = toast.style();
  drop(style.remove_property("transform"));
  drop(style.remove_property("opacity"));
}

fn toast_of(ev: &web_sys::PointerEvent) -> Option<web_sys::HtmlElement> {
  ev.current_target()
    .and_then(|target| target.dyn_into::<web_sys::HtmlElement>().ok())
}

#[cfg(test)]
mod tests {
  use super::*;

  mod dismissal {
    use super::*;

    #[test]
    fn a_long_swipe_dismisses_either_way() {
      assert!(is_dismissal(41.0, 0.0));
      assert!(is_dismissal(-41.0, 0.0));
    }

    #[test]
    fn a_quick_flick_dismisses() {
      assert!(is_dismissal(15.0, 0.2));
      assert!(is_dismissal(-15.0, -0.2));
    }

    #[test]
    fn a_short_slow_or_returning_swipe_springs_back() {
      assert!(!is_dismissal(30.0, 0.05));
      assert!(!is_dismissal(30.0, -0.5));
    }
  }

  #[test]
  fn a_finger_that_stopped_before_lifting_does_not_flick() {
    // Moved 15 px in 30 ms, then held still for 200 ms.
    assert!(release_velocity((10.0, 30.0), (25.0, 260.0)) < DISMISS_VELOCITY_PX_PER_MS);
    // Lifted right after the move.
    assert!(release_velocity((10.0, 30.0), (25.0, 65.0)) > DISMISS_VELOCITY_PX_PER_MS);
  }

  #[test]
  fn a_pulled_toast_fades_to_half_at_its_width() {
    assert!((swipe_opacity(0.0, 300.0) - 1.0).abs() < f64::EPSILON);
    assert!((swipe_opacity(-300.0, 300.0) - 0.5).abs() < f64::EPSILON);
    assert!((swipe_opacity(50.0, 0.0) - 1.0).abs() < f64::EPSILON);
  }

  #[test]
  fn the_vertical_translate_is_read_from_the_computed_value() {
    assert!((translate_y("0px 12.5px") - 12.5).abs() < f64::EPSILON);
    assert!(translate_y("10px").abs() < f64::EPSILON);
    assert!(translate_y("none").abs() < f64::EPSILON);
  }
}
