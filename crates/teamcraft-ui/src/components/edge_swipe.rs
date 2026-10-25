//! Swiping back from the leading edge on a phone (#368). Installed as an app, iOS gives the page no
//! back gesture of its own, and the back button sits in the corner that is hardest to reach. In a
//! browser tab the system's own edge swipe goes back, so this one only runs installed.
//!
//! A touch that starts at the leading edge of a deeper page and moves sideways takes the page with
//! it: the page follows the finger, its parent peeks out from underneath. Let go past half the width
//! (with momentum, `gestures.rs`) and it goes back the way the back button does; otherwise the page
//! springs back. Never in the composer, in a sheet or dialog, or while text is selected. With
//! reduced motion the page doesn't move, the gesture still goes back.
//!
//! While the finger moves only the page and its parent get a new `translate` (no layout reads, no
//! inherited properties): the width is measured once when the swipe takes the finger.

use std::time::Duration;

use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::gestures::{EdgeSwipe, SwipeRelease, SwipeState};
use crate::history::back_or_up;
use crate::reactive::SetIfAlive;
use crate::state::{AppState, Overlay};
use crate::viewport::{CompactDepth, CompactPane};

/// The longest the page waits off screen for the step back to show the parent; then it is put back
/// (a step that did nothing must not leave an empty screen).
const COMMIT_TIMEOUT: Duration = Duration::from_millis(600);
/// How long the page takes to slide out or back after the finger lifted (`--dur-base`).
const SETTLE: Duration = Duration::from_millis(180);
/// Where the parent starts when the swipe starts, as a part of the width (it peeks from −30 %).
const PARENT_PEEK: f64 = 0.3;
/// Where a touch must not start a swipe: the composer and text fields keep their own gestures, and
/// sheets, dialogs and menus lie over the page.
const NO_SWIPE: &str = ".composer, input, textarea, [contenteditable], .sheet, .sheet-backdrop, .modal, \
                        .popover, .popover-backdrop, [role='dialog']";

/// The gesture of one `.app-body`; its handlers go on that element.
#[derive(Clone, Copy)]
pub struct EdgeSwipeBack {
  state: AppState,
  body: NodeRef<html::Div>,
  maybe_swipe: StoredValue<Option<EdgeSwipe>>,
  /// The elements that move and the width, from when the swipe took the finger.
  maybe_targets: StoredValue<Option<SwipeTargets>, LocalStorage>,
  /// Bumped by every swipe, so a late timer of an earlier one does nothing.
  generation: StoredValue<u32>,
  /// A swipe went back: the parent is on screen already and must not slide in again.
  is_committed: RwSignal<bool>,
}

/// The page under the finger and the parent beneath it (a list is the rail and the sidebar).
struct SwipeTargets {
  page: web_sys::HtmlElement,
  parents: Vec<web_sys::HtmlElement>,
  width: f64,
}

impl EdgeSwipeBack {
  pub fn new(state: AppState, body: NodeRef<html::Div>) -> Self {
    let swipe = Self {
      state,
      body,
      maybe_swipe: StoredValue::new(None),
      maybe_targets: StoredValue::new_local(None),
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
    let Some(swipe) = EdgeSwipe::start(
      f64::from(ev.client_x()),
      f64::from(ev.client_y()),
      ev.time_stamp(),
    ) else {
      return;
    };
    self.maybe_swipe.set_value(Some(swipe));
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
        if !was_following {
          self.take_the_finger(ev);
        }
        if !prefers_reduced_motion() {
          self.follow(offset);
        }
      },
    }
  }

  pub fn pointer_up(&self, ev: &web_sys::PointerEvent) {
    let Some(swipe) = self.maybe_swipe.try_update_value(Option::take).flatten() else {
      return;
    };
    let Some(width) = self
      .maybe_targets
      .try_with_value(|maybe_targets| maybe_targets.as_ref().map(|targets| targets.width))
      .flatten()
    else {
      return;
    };
    match swipe.released(f64::from(ev.client_x()), width) {
      SwipeRelease::Back => self.commit(width),
      SwipeRelease::Stay => self.settle(),
    }
  }

  pub fn pointer_cancel(&self) {
    let was_following = self
      .maybe_swipe
      .try_update_value(Option::take)
      .flatten()
      .is_some_and(|swipe| matches!(swipe.state(), SwipeState::Following(_)));
    if was_following {
      self.settle();
    }
  }

  /// A touch at the edge of a deeper page of the installed app, with nothing in the way.
  fn may_start(&self, ev: &web_sys::PointerEvent) -> bool {
    if ev.pointer_type() != "touch" || !ev.is_primary() || !is_installed() {
      return false;
    }
    let state = self.state;
    if !state.layout.get_untracked().is_compact()
      || state.compact_depth() == CompactDepth::TopLevel
      || state.overlay.get_untracked() != Overlay::None
    {
      return false;
    }
    let is_in_the_way = ev
      .target()
      .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
      .and_then(|element| element.closest(NO_SWIPE).ok().flatten())
      .is_some();
    !is_in_the_way && !has_text_selection()
  }

  /// The swipe is decided: it keeps the finger, shows the parent and measures once.
  fn take_the_finger(&self, ev: &web_sys::PointerEvent) {
    let Some(body) = self.body.get_untracked() else {
      return;
    };
    // A page still settling is grabbed again where it is: its pending step back or reset is off.
    // Only now, once the touch is a swipe: a tap or a scroll during the settle leaves it running.
    self.bump();
    self.is_committed.set(false);
    set_app_class(&body, "is-swipe-settling", false);
    drop(body.set_pointer_capture(ev.pointer_id()));
    set_app_class(&body, "is-swiping-back", true);
    let maybe_targets = swipe_targets(&body, self.state.compact_pane());
    self.maybe_targets.set_value(maybe_targets);
  }

  /// Slides the page out and goes back; the parent is already underneath.
  fn commit(&self, width: f64) {
    let generation = self.bump();
    self.is_committed.set(true);
    if prefers_reduced_motion() {
      back_or_up(self.state);
      self.reset_later(generation, COMMIT_TIMEOUT);
      return;
    }
    self.slide_to(width);
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
  fn settle(&self) {
    let generation = self.bump();
    self.slide_to(0.0);
    self.reset_later(generation, SETTLE);
  }

  /// Lets the page and its parent glide to `offset` after the finger lifted.
  fn slide_to(&self, offset: f64) {
    if let Some(body) = self.body.get_untracked() {
      set_app_class(&body, "is-swipe-settling", true);
    }
    self.follow(offset);
  }

  /// Moves the page to `offset` pixels from its place and its parent along from −30 % of the width.
  fn follow(&self, offset: f64) {
    self.maybe_targets.with_value(|maybe_targets| {
      let Some(targets) = maybe_targets else {
        return;
      };
      let progress = (offset / targets.width.max(1.0)).clamp(0.0, 1.0);
      let parent_offset = -PARENT_PEEK * targets.width * (1.0 - progress);
      set_translate(&targets.page, Some(offset));
      for parent in &targets.parents {
        set_translate(parent, Some(parent_offset));
      }
    });
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
    let _ = self.maybe_targets.try_update_value(|maybe_targets| {
      if let Some(targets) = maybe_targets.take() {
        set_translate(&targets.page, None);
        for parent in &targets.parents {
          set_translate(parent, None);
        }
      }
    });
    if let Some(body) = self.body.try_get_untracked().flatten() {
      set_app_class(&body, "is-swiping-back", false);
      set_app_class(&body, "is-swipe-settling", false);
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

/// The page of `pane` and what lies beneath it: the list (rail and sidebar) under a page, the page
/// under a side panel.
fn swipe_targets(body: &web_sys::HtmlDivElement, pane: CompactPane) -> Option<SwipeTargets> {
  let find = |selector: &str| {
    body
      .query_selector(selector)
      .ok()
      .flatten()
      .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
  };
  let (page, parents) = match pane {
    CompactPane::Panel => (find(":scope > .side-panel")?, vec![find(":scope > .main")?]),
    CompactPane::Main => (
      find(":scope > .main")?,
      [":scope > .space-rail", ":scope > .sidebar"]
        .into_iter()
        .filter_map(find)
        .collect(),
    ),
    CompactPane::List => return None,
  };
  Some(SwipeTargets {
    page,
    parents,
    width: body.get_bounding_client_rect().width(),
  })
}

/// Moves an element by `offset` pixels sideways; `None` puts it back.
fn set_translate(element: &web_sys::HtmlElement, maybe_offset: Option<f64>) {
  let style = element.style();
  // A failed style write only means the element doesn't move with the finger.
  drop(match maybe_offset {
    Some(offset) => style.set_property("translate", &format!("{offset}px 0")),
    None => style.remove_property("translate").map(|_| ()),
  });
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

/// Installed (home screen app, desktop app window): no system edge swipe to go back.
fn is_installed() -> bool {
  let Some(window) = web_sys::window() else {
    return false;
  };
  let matches = |query: &str| {
    window
      .match_media(query)
      .ok()
      .flatten()
      .is_some_and(|list| list.matches())
  };
  // iOS reports home screen apps through `navigator.standalone` only on older versions.
  let is_ios_standalone = js_sys::Reflect::get(&window.navigator(), &"standalone".into())
    .ok()
    .and_then(|value| value.as_bool())
    .unwrap_or(false);
  matches("(display-mode: standalone)") || matches("(display-mode: fullscreen)") || is_ios_standalone
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
