//! Motion helpers for what CSS can't do alone (style/main.css has the tokens and the keyframes).
//!
//! Entrances are plain CSS animations that run when an element mounts. Exits need the element to
//! stay a moment after the view dropped it: [`use_leave_ghost`] leaves an inert copy in its place
//! that plays the exit animation (`.is-leaving`) and then removes itself. The real element is gone
//! at once, so focus, state and the next view never wait for an animation. With reduced motion the
//! ghost still fades out (the stylesheet drops the movement, #360). An element that already played
//! its own exit (a swiped toast) carries `.has-exited` and leaves no ghost.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use leptos::html::ElementType;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

/// How long a ghost stays: the longest exit (`--dur-exit`, 100 ms) plus a frame of slack.
const GHOST_LIFETIME: Duration = Duration::from_millis(140);
/// A hidden ghost goes two frames later, or after this when no frames come (a hidden tab).
const GHOST_REMOVAL_FALLBACK: Duration = Duration::from_millis(100);
/// Copies of elements that left the view while their exit plays, so a burst can't pile them up.
const MAX_GHOSTS: u32 = 8;

thread_local! {
  static GHOSTS: Cell<u32> = const { Cell::new(0) };
}

/// A monotonic time for comparing moments of the same page (the page's `performance.now()`), never
/// a date. Zero where there is no page.
pub fn now() -> Duration {
  let millis = web_sys::window()
    .and_then(|window| window.performance())
    .map_or(0.0, |performance| performance.now());
  Duration::from_secs_f64(millis.max(0.0) / 1000.0)
}

/// Whether the user asked the system for less motion: for motion the stylesheet can't turn off
/// (positions measured and played from Rust).
pub fn prefers_reduced_motion() -> bool {
  web_sys::window()
    .and_then(|window| {
      window
        .match_media("(prefers-reduced-motion: reduce)")
        .ok()
        .flatten()
    })
    .is_some_and(|query| query.matches())
}

/// Which side of its trigger a self-placed popover opens on, so it grows out of (and shrinks back
/// into) the edge that touches the trigger (`.opens-up` in style/main.css, #359).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PopoverSide {
  #[default]
  Below,
  Above,
}

impl PopoverSide {
  /// The class that sets the transform origin for this side.
  pub fn class(self) -> &'static str {
    match self {
      Self::Below => "",
      Self::Above => "opens-up",
    }
  }
}

/// When the component that owns `node` is dropped, a copy of the element plays its exit animation
/// where the element was. Nothing happens when the element already left the page.
pub fn use_leave_ghost<E>(node: NodeRef<E>)
where
  E: ElementType,
  E::Output: JsCast + Clone + 'static,
{
  let element = StoredValue::new_local(None::<web_sys::Element>);
  Effect::new(move |_| {
    if let Some(mounted) = node.get() {
      element.set_value(mounted.dyn_ref::<web_sys::Element>().cloned());
    }
  });
  on_cleanup(move || {
    element.try_with_value(|maybe_element| {
      if let Some(element) = maybe_element {
        leave_ghost(element);
      }
    });
  });
}

/// Going back on a phone (#368): the page that is left slides out towards the trailing edge while
/// its parent slides in. Called right after the state went back, while the page is still on screen
/// (the view updates on the next tick). Scroll positions are copied, so the copy shows what was seen.
pub fn leave_pane_ghost() {
  let Some(document) = web_sys::window().and_then(|window| window.document()) else {
    return;
  };
  // An edge swipe already slid the page out under the finger (components/edge_swipe.rs).
  if document
    .query_selector(".app.is-swiping-back")
    .ok()
    .flatten()
    .is_some()
  {
    return;
  }
  let maybe_pane = [".app-body > .side-panel", ".app-body > .main"]
    .into_iter()
    .filter_map(|selector| document.query_selector(selector).ok().flatten())
    .find(|pane| pane.get_bounding_client_rect().width() > 0.0);
  let Some(pane) = maybe_pane else {
    return;
  };
  if let Some(ghost) = leave_ghost(&pane) {
    drop(ghost.class_list().add_1("pane-ghost"));
    copy_scroll_positions(&pane, &ghost);
  }
}

/// Leaves an inert, hidden-from-assistive-tech copy of `element` at its place on screen, with
/// `.is-leaving` for the exit animation, and removes it after [`GHOST_LIFETIME`].
fn leave_ghost(element: &web_sys::Element) -> Option<web_sys::HtmlElement> {
  if !element.is_connected() || element.class_list().contains("has-exited") || GHOSTS.get() >= MAX_GHOSTS {
    return None;
  }
  let body = web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.body())?;
  let rect = element.get_bounding_client_rect();
  if rect.width() <= 0.0 || rect.height() <= 0.0 {
    return None;
  }
  let ghost = element
    .clone_node_with_deep(true)
    .ok()
    .and_then(|node| node.dyn_into::<web_sys::HtmlElement>().ok())?;
  dress_ghost(&ghost, &rect);
  body.append_child(&ghost).ok()?;
  GHOSTS.set(GHOSTS.get() + 1);
  let retired = ghost.clone();
  set_timeout(move || retire_ghost(retired), GHOST_LIFETIME);
  Some(ghost)
}

/// Hides the ghost (`.is-gone`), then removes it two frames later. Its exit holds the last frame
/// (`forwards`); removing it at once drops the layer and the animation in one commit, and WebKit can
/// present one frame of the copy as it was before the exit: the element jumps back for a frame.
fn retire_ghost(ghost: web_sys::HtmlElement) {
  drop(ghost.class_list().add_1("is-gone"));
  let is_removed = Rc::new(Cell::new(false));
  let remove = move || {
    if !is_removed.replace(true) {
      ghost.remove();
      // async-ok: a thread-local Cell, not a signal.
      GHOSTS.set(GHOSTS.get().saturating_sub(1));
    }
  };
  let after_frames = remove.clone();
  request_animation_frame(move || request_animation_frame(after_frames));
  set_timeout(remove, GHOST_REMOVAL_FALLBACK);
}

/// A deep copy starts scrolled to the top; give each scrolled part of it the original's place.
fn copy_scroll_positions(original: &web_sys::Element, copy: &web_sys::Element) {
  let (Ok(originals), Ok(copies)) = (original.query_selector_all("*"), copy.query_selector_all("*")) else {
    return;
  };
  for index in 0..originals.length().min(copies.length()) {
    let pair = (
      originals
        .item(index)
        .and_then(|node| node.dyn_into::<web_sys::Element>().ok()),
      copies
        .item(index)
        .and_then(|node| node.dyn_into::<web_sys::Element>().ok()),
    );
    if let (Some(from), Some(to)) = pair
      && from.scroll_top() > 0
    {
      to.set_scroll_top(from.scroll_top());
    }
  }
}

/// A copy that nobody can reach: no ids (they stay unique), no focus, no pointer, not announced,
/// fixed where the original was.
fn dress_ghost(ghost: &web_sys::HtmlElement, rect: &web_sys::DomRect) {
  // Errors only mean an attribute or property stays as copied; the ghost is gone in a moment.
  drop(ghost.set_attribute("aria-hidden", "true"));
  drop(ghost.set_attribute("inert", ""));
  ghost.remove_attribute("id").ok();
  if let Ok(with_ids) = ghost.query_selector_all("[id]") {
    for index in 0..with_ids.length() {
      if let Some(inner) = with_ids
        .item(index)
        .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
      {
        inner.remove_attribute("id").ok();
      }
    }
  }
  drop(ghost.class_list().add_2("motion-ghost", "is-leaving"));
  let style = ghost.style();
  for (property, value) in [
    ("left", format!("{}px", rect.left())),
    ("top", format!("{}px", rect.top())),
    ("width", format!("{}px", rect.width())),
    ("height", format!("{}px", rect.height())),
  ] {
    drop(style.set_property(property, &value));
  }
}
