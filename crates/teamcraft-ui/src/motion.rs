//! Motion helpers for what CSS can't do alone (style/main.css has the tokens and the keyframes).
//!
//! Entrances are plain CSS animations that run when an element mounts. Exits need the element to
//! stay a moment after the view dropped it: [`use_leave_ghost`] leaves an inert copy in its place
//! that plays the exit animation (`.is-leaving`) and then removes itself. The real element is gone
//! at once, so focus, state and the next view never wait for an animation.

use std::time::Duration;

use leptos::html::ElementType;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

/// How long a ghost stays: the longest exit (`--dur-exit`, 100 ms) plus a frame of slack.
const GHOST_LIFETIME: Duration = Duration::from_millis(140);
/// Copies of elements that left the view while their exit plays, so a burst can't pile them up.
const MAX_GHOSTS: u32 = 8;

thread_local! {
  static GHOSTS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Whether the user asked the system for less motion.
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

/// When the component that owns `node` is dropped, a copy of the element plays its exit animation
/// where the element was. Nothing happens with reduced motion or when the element already left the
/// page.
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

/// Leaves an inert, hidden-from-assistive-tech copy of `element` at its place on screen, with
/// `.is-leaving` for the exit animation, and removes it after [`GHOST_LIFETIME`].
fn leave_ghost(element: &web_sys::Element) {
  if !element.is_connected() || prefers_reduced_motion() || GHOSTS.get() >= MAX_GHOSTS {
    return;
  }
  let Some(body) = web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.body())
  else {
    return;
  };
  let rect = element.get_bounding_client_rect();
  if rect.width() <= 0.0 || rect.height() <= 0.0 {
    return;
  }
  let Some(ghost) = element
    .clone_node_with_deep(true)
    .ok()
    .and_then(|node| node.dyn_into::<web_sys::HtmlElement>().ok())
  else {
    return;
  };
  dress_ghost(&ghost, &rect);
  if body.append_child(&ghost).is_err() {
    return;
  }
  GHOSTS.set(GHOSTS.get() + 1);
  set_timeout(
    move || {
      ghost.remove();
      GHOSTS.set(GHOSTS.get().saturating_sub(1));
    },
    GHOST_LIFETIME,
  );
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
