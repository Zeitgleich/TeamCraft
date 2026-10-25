//! Swiping back from the leading edge on a phone (#368). Installed as an app, iOS gives the page no
//! back gesture of its own, and the back button sits in the corner that is hardest to reach.
//!
//! A touch that starts at the leading edge of a deeper page and moves sideways takes the page with
//! it: the page follows the finger, its parent peeks out from underneath. Let go past half the width
//! (with momentum, `gestures.rs`) and it goes back the way the back button does; otherwise the page
//! springs back. Never in the composer or while text is selected. With reduced motion the page
//! doesn't move, the gesture still goes back.

use std::time::Duration;

use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::gestures::{EdgeSwipe, SwipeRelease, SwipeState};
use crate::history::back_or_up;
use crate::reactive::SetIfAlive;
use crate::state::{AppState, Overlay};
use crate::viewport::CompactDepth;

/// The longest the page waits off screen for the step back to show the parent; then it is put back
/// (a step that did nothing must not leave an empty screen).
const COMMIT_TIMEOUT: Duration = Duration::from_millis(600);
/// How long the page takes to slide out or back after the finger lifted (`--dur-base`).
const SETTLE: Duration = Duration::from_millis(180);
/// Where the parent starts when the swipe starts, as a part of the width (it peeks from −30 %).
const PARENT_PEEK: f64 = 0.3;

/// The gesture of one `.app-body`; its handlers go on that element.
#[derive(Clone, Copy)]
pub struct EdgeSwipeBack {
  state: AppState,
  body: NodeRef<html::Div>,
  maybe_swipe: StoredValue<Option<EdgeSwipe>>,
  /// Bumped by every swipe, so a late timer of an earlier one does nothing.
  generation: StoredValue<u32>,
  /// A swipe went back: the parent is on screen already and must not slide in again.
  is_committed: RwSignal<bool>,
}

impl EdgeSwipeBack {
  pub fn new(state: AppState, body: NodeRef<html::Div>) -> Self {
    let swipe = Self {
      state,
      body,
      maybe_swipe: StoredValue::new(None),
      generation: StoredValue::new(0),
      is_committed: RwSignal::new(false),
    };
    // Once the parent shows, the page that was swiped away is done.
    Effect::new(move |previous: Option<CompactDepth>| {
      let depth = state.compact_depth();
      if previous.is_some_and(|before| before != depth) {
        swipe.reset();
      }
      depth
    });
    swipe
  }

  /// Whether the last step back was this gesture's (the panes don't slide then).
  pub fn is_committed(&self) -> bool {
    self.is_committed.get()
  }

  pub fn pointer_down(&self, ev: &web_sys::PointerEvent) {
    if !self.may_start(ev) {
      return;
    }
    self.maybe_swipe.set_value(EdgeSwipe::start(
      f64::from(ev.client_x()),
      f64::from(ev.client_y()),
      ev.time_stamp(),
    ));
  }

  pub fn pointer_move(&self, ev: &web_sys::PointerEvent) {
    let Some(moved) = self
      .maybe_swipe
      .try_update_value(|maybe_swipe| {
        maybe_swipe.as_mut().map(|swipe| {
          let was_following = matches!(swipe.state(), SwipeState::Following(_));
          let now = swipe.moved(
            f64::from(ev.client_x()),
            f64::from(ev.client_y()),
            ev.time_stamp(),
          );
          (was_following, now)
        })
      })
      .flatten()
    else {
      return;
    };
    match moved {
      (_, SwipeState::Pending) => {},
      (_, SwipeState::Abandoned) => self.maybe_swipe.set_value(None),
      (was_following, SwipeState::Following(offset)) => {
        let Some(body) = self.body.get_untracked() else {
          return;
        };
        if !was_following {
          // The swipe has the finger now, also outside the strip it started in.
          drop(body.set_pointer_capture(ev.pointer_id()));
          set_app_class(&body, "is-swiping-back", true);
        }
        if !prefers_reduced_motion() {
          follow(&body, offset);
        }
      },
    }
  }

  pub fn pointer_up(&self, ev: &web_sys::PointerEvent) {
    let Some(swipe) = self.maybe_swipe.try_update_value(Option::take).flatten() else {
      return;
    };
    let Some(body) = self.body.get_untracked() else {
      return;
    };
    let width = body.get_bounding_client_rect().width();
    match swipe.released(f64::from(ev.client_x()), width) {
      SwipeRelease::Back => self.commit(&body, width),
      SwipeRelease::Stay => self.settle(&body),
    }
  }

  pub fn pointer_cancel(&self) {
    if self
      .maybe_swipe
      .try_update_value(Option::take)
      .flatten()
      .is_some()
      && let Some(body) = self.body.get_untracked()
    {
      self.settle(&body);
    }
  }

  /// A touch at the edge of a deeper page, with nothing in the way.
  fn may_start(&self, ev: &web_sys::PointerEvent) -> bool {
    if ev.pointer_type() != "touch" || !ev.is_primary() {
      return false;
    }
    let state = self.state;
    if !state.layout.get_untracked().is_compact()
      || state.compact_depth() == CompactDepth::TopLevel
      || state.overlay.get_untracked() != Overlay::None
    {
      return false;
    }
    let is_in_composer = ev
      .target()
      .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
      .and_then(|element| {
        element
          .closest(".composer, input, textarea, [contenteditable]")
          .ok()
          .flatten()
      })
      .is_some();
    !is_in_composer && !has_text_selection()
  }

  /// Slides the page out and goes back; the parent is already underneath.
  fn commit(&self, body: &web_sys::HtmlDivElement, width: f64) {
    let generation = self.bump();
    self.is_committed.set(true);
    if prefers_reduced_motion() {
      back_or_up(self.state);
      self.reset_later(generation, COMMIT_TIMEOUT);
      return;
    }
    set_app_class(body, "is-swipe-settling", true);
    follow(body, width);
    let swipe = *self;
    set_timeout(
      move || {
        if swipe.generation.try_get_value() == Some(generation) {
          back_or_up(swipe.state);
          swipe.reset_later(generation, COMMIT_TIMEOUT);
        }
      },
      SETTLE,
    );
  }

  /// Springs the page back over its parent.
  fn settle(&self, body: &web_sys::HtmlDivElement) {
    let generation = self.bump();
    set_app_class(body, "is-swipe-settling", true);
    follow(body, 0.0);
    self.reset_later(generation, SETTLE);
  }

  fn reset_later(&self, generation: u32, delay: Duration) {
    let swipe = *self;
    set_timeout(
      move || {
        if swipe.generation.try_get_value() == Some(generation) {
          swipe.reset();
        }
      },
      delay,
    );
  }

  /// Back to no swipe at all.
  fn reset(&self) {
    let _ = self.maybe_swipe.try_set_value(None);
    if self.is_committed.try_get_untracked() == Some(true) {
      self.is_committed.set_if_alive(false);
    }
    if let Some(body) = self.body.try_get_untracked().flatten() {
      set_app_class(&body, "is-swiping-back", false);
      set_app_class(&body, "is-swipe-settling", false);
      let style = web_sys::HtmlElement::style(&body);
      // A failed removal only leaves an unused custom property behind.
      drop(style.remove_property("--swipe-x"));
      drop(style.remove_property("--swipe-parent-x"));
    }
  }

  fn bump(&self) -> u32 {
    let mut next = 0;
    let _ = self.generation.try_update_value(|generation| {
      *generation = generation.wrapping_add(1);
      next = *generation;
    });
    next
  }
}

/// Moves the page to `offset` pixels from its place and its parent along from −30 % of the width,
/// in pixels, so the parent's parts (rail and list) move as one (style/main.css).
fn follow(body: &web_sys::HtmlDivElement, offset: f64) {
  let width = body.get_bounding_client_rect().width().max(1.0);
  let progress = (offset / width).clamp(0.0, 1.0);
  let parent_offset = -PARENT_PEEK * width * (1.0 - progress);
  let style = web_sys::HtmlElement::style(body);
  // Failed style writes only mean the page doesn't move with the finger.
  drop(style.set_property("--swipe-x", &format!("{offset}px")));
  drop(style.set_property("--swipe-parent-x", &format!("{parent_offset}px")));
}

/// The classes go on `.app`, the parent of `.app-body`, so the CSS can show the parent pane too.
fn set_app_class(body: &web_sys::HtmlDivElement, class: &str, is_on: bool) {
  let Some(app) = body.parent_element() else {
    return;
  };
  let classes = app.class_list();
  // A failed class change only leaves the page where it is.
  drop(if is_on {
    classes.add_1(class)
  } else {
    classes.remove_1(class)
  });
}

fn has_text_selection() -> bool {
  web_sys::window()
    .and_then(|window| window.get_selection().ok().flatten())
    .is_some_and(|selection| !selection.is_collapsed())
}

fn prefers_reduced_motion() -> bool {
  web_sys::window()
    .and_then(|window| {
      window
        .match_media("(prefers-reduced-motion: reduce)")
        .ok()
        .flatten()
    })
    .is_some_and(|query| query.matches())
}
