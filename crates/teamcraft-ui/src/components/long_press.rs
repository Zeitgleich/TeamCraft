//! A finger resting on something opens its menu or action sheet; moving it (scrolling) or lifting
//! it early does not. Messages (#364) and the sidebar's entries (#903) use it, so a long-press
//! feels the same everywhere.

use leptos::prelude::*;

use crate::components::message_toolbar::LONG_PRESS;
use crate::reactive::SetIfAlive;

/// A finger that moves further than this is scrolling, not pressing.
const LONG_PRESS_SLOP_PX: i32 = 10;
/// The short tick when the menu opens, where the device can vibrate (not iOS Safari).
const LONG_PRESS_HAPTIC_MS: u32 = 10;

/// One element's long-press. The element tints from the first touch (`is_pressing`), so the press
/// is felt before the menu comes.
#[derive(Clone, Copy)]
pub struct LongPress {
  /// Bumped by every new press and cancel, so a timer of an earlier press does nothing.
  generation: StoredValue<u32>,
  maybe_origin: StoredValue<Option<(i32, i32)>>,
  has_fired: StoredValue<bool>,
  /// The last press was a finger (not a mouse or pen).
  was_touch: StoredValue<bool>,
  pub is_pressing: RwSignal<bool>,
  /// Called with where the finger rests.
  on_fire: StoredValue<Box<dyn Fn(i32, i32) + Send + Sync>>,
}

impl LongPress {
  pub fn new(on_fire: impl Fn(i32, i32) + Send + Sync + 'static) -> Self {
    Self {
      generation: StoredValue::new(0),
      maybe_origin: StoredValue::new(None),
      has_fired: StoredValue::new(false),
      was_touch: StoredValue::new(false),
      is_pressing: RwSignal::new(false),
      on_fire: StoredValue::new(Box::new(on_fire)),
    }
  }

  /// A press starts; only a finger with `may_fire` can turn into a long-press.
  pub fn start(&self, ev: &web_sys::PointerEvent, may_fire: bool) {
    self.has_fired.set_value(false);
    self.was_touch.set_value(ev.pointer_type() == "touch");
    if ev.pointer_type() != "touch" || !may_fire {
      return;
    }
    let generation = self.bump();
    let origin = (ev.client_x(), ev.client_y());
    self.maybe_origin.set_value(Some(origin));
    self.is_pressing.set(true);
    let press = *self;
    set_timeout(
      move || {
        if press.generation.try_get_value() == Some(generation) {
          press.has_fired.try_set_value(true);
          press.maybe_origin.try_set_value(None);
          press.is_pressing.set_if_alive(false);
          press.on_fire.try_with_value(|fire| fire(origin.0, origin.1));
          tick_haptic();
        }
      },
      LONG_PRESS,
    );
  }

  pub fn track(&self, ev: &web_sys::PointerEvent) {
    let Some((x, y)) = self.maybe_origin.get_value() else {
      return;
    };
    if (ev.client_x() - x).abs() > LONG_PRESS_SLOP_PX || (ev.client_y() - y).abs() > LONG_PRESS_SLOP_PX {
      self.cancel();
    }
  }

  pub fn cancel(&self) {
    self.bump();
    self.maybe_origin.set_value(None);
    self.is_pressing.set(false);
  }

  /// Whether the last press opened the menu: its `click` and `contextmenu` are no tap.
  pub fn has_fired(&self) -> bool {
    self.has_fired.try_get_value().unwrap_or(false)
  }

  pub fn was_touch(&self) -> bool {
    self.was_touch.try_get_value().unwrap_or(false)
  }

  fn bump(&self) -> u32 {
    let mut next = 0;
    self.generation.update_value(|generation| {
      *generation = generation.wrapping_add(1);
      next = *generation;
    });
    next
  }
}

/// A short vibration where the device offers one; without it the menu itself is the feedback.
fn tick_haptic() {
  let Some(window) = web_sys::window() else {
    return;
  };
  let navigator = window.navigator();
  // WebKit (iOS Safari, WKWebView, WebKitGTK) has no `vibrate`; calling it would throw.
  if !js_sys::Reflect::has(&navigator, &"vibrate".into()).unwrap_or(false) {
    return;
  }
  // Refused (no permission yet, no motor): nothing to tell the user.
  let _ = navigator.vibrate_with_duration(LONG_PRESS_HAPTIC_MS);
}
