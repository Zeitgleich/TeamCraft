//! Dragging the edge of the sidebar to make it wider or narrower (#842). The width is a setting of
//! this device (it depends on the screen), kept in `localStorage`; it replaces the stylesheet's
//! `--sidebar-w` on the root element, which the sidebar and the top bar's brand read. The
//! compact layout has no resizer, and leaving it removes the override again.

use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::i18n::use_i18n;
use crate::settings::{load_string, store_string};

/// Storage key of the chosen width in pixels; empty means the default.
const WIDTH_KEY: &str = "teamcraft.sidebar_width";
/// The width of the stylesheet, shown when none was chosen.
const DEFAULT_WIDTH_PX: f64 = 264.0;
const MIN_WIDTH_PX: f64 = 200.0;
const MAX_WIDTH_PX: f64 = 480.0;
/// The sidebar never takes more than this share of the window, however wide it was set.
const MAX_WINDOW_SHARE: f64 = 0.4;
const KEY_STEP_PX: f64 = 16.0;
const KEY_STEP_LARGE_PX: f64 = 48.0;

/// The drag handle on the sidebar's right edge: a separator the pointer drags and the arrow keys
/// move, reset by a double click or Enter.
#[component]
pub fn SidebarResizer() -> impl IntoView {
  let i18n = use_i18n();
  let width = RwSignal::new(load_string(WIDTH_KEY).as_deref().and_then(parse_stored_width));
  let handle = NodeRef::<html::Div>::new();
  // Where the pointer and the sidebar's edge were when the drag began.
  let drag_start = StoredValue::new(None::<(f64, f64)>);
  let is_dragging = RwSignal::new(false);
  Effect::new(move |_| apply_width(width.get()));
  on_cleanup(|| apply_width(None));

  let shown_width = move || width.get().unwrap_or(DEFAULT_WIDTH_PX);
  let current_width = move || {
    handle
      .get_untracked()
      .and_then(|handle| handle.previous_element_sibling())
      .map_or_else(shown_width, |sidebar| sidebar.get_bounding_client_rect().width())
  };
  let set_width = move |maybe_width: Option<f64>| {
    let maybe_width = maybe_width.map(|width| clamp_width(width, window_width()));
    width.set(maybe_width);
    store_string(
      WIDTH_KEY,
      &maybe_width.map(|width| format!("{width:.0}")).unwrap_or_default(),
    );
  };

  let on_pointerdown = move |ev: web_sys::PointerEvent| {
    if ev.button() != 0 {
      return;
    }
    ev.prevent_default();
    if let Some(element) = handle.get_untracked() {
      drop(element.set_pointer_capture(ev.pointer_id()));
    }
    drag_start.set_value(Some((f64::from(ev.client_x()), current_width())));
    is_dragging.set(true);
  };
  let on_pointermove = move |ev: web_sys::PointerEvent| {
    if let Some((start_x, start_width)) = drag_start.get_value() {
      set_width(Some(start_width + f64::from(ev.client_x()) - start_x));
    }
  };
  let on_pointerend = move |_| {
    drag_start.set_value(None);
    is_dragging.set(false);
  };
  let on_keydown = move |ev: web_sys::KeyboardEvent| {
    let step = if ev.shift_key() {
      KEY_STEP_LARGE_PX
    } else {
      KEY_STEP_PX
    };
    let next = match ev.key().as_str() {
      "ArrowLeft" => Some(Some(current_width() - step)),
      "ArrowRight" => Some(Some(current_width() + step)),
      "Home" => Some(Some(MIN_WIDTH_PX)),
      "End" => Some(Some(MAX_WIDTH_PX)),
      "Enter" => Some(None),
      _ => None,
    };
    if let Some(next) = next {
      ev.prevent_default();
      set_width(next);
    }
  };

  view! {
    <div
      class="sidebar-resizer"
      class:is-dragging=move || is_dragging.get()
      node_ref=handle
      role="separator"
      aria-orientation="vertical"
      tabindex="0"
      aria-label=move || i18n.t("sidebar-resize")
      aria-valuemin=MIN_WIDTH_PX.to_string()
      aria-valuemax=MAX_WIDTH_PX.to_string()
      aria-valuenow=move || format!("{:.0}", shown_width())
      on:pointerdown=on_pointerdown
      on:pointermove=on_pointermove
      on:pointerup=on_pointerend
      on:pointercancel=on_pointerend
      on:keydown=on_keydown
      on:dblclick=move |_| set_width(None)
    ></div>
  }
}

/// `width` kept inside the limits, and below the window's share.
fn clamp_width(width: f64, window_width: f64) -> f64 {
  let max = MAX_WIDTH_PX
    .min(window_width * MAX_WINDOW_SHARE)
    .max(MIN_WIDTH_PX);
  if width.is_finite() {
    width.clamp(MIN_WIDTH_PX, max)
  } else {
    DEFAULT_WIDTH_PX.clamp(MIN_WIDTH_PX, max)
  }
}

/// A width read back from storage; anything unreadable or outside the limits is ignored.
fn parse_stored_width(text: &str) -> Option<f64> {
  text
    .trim()
    .parse::<f64>()
    .ok()
    .filter(|width| (MIN_WIDTH_PX..=MAX_WIDTH_PX).contains(width))
}

/// Sets (or, for `None`, removes) the width on the root element. Through the style object, which
/// the page's content security policy allows.
fn apply_width(maybe_width: Option<f64>) {
  let Some(root) = web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.document_element())
    .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
  else {
    return;
  };
  let style = root.style();
  let result = match maybe_width {
    // The share keeps a width chosen on a big screen from filling a small window later.
    Some(width) => style.set_property(
      "--sidebar-w",
      &format!("min({width:.0}px, {:.0}vw)", MAX_WINDOW_SHARE * 100.0),
    ),
    None => style.remove_property("--sidebar-w").map(drop),
  };
  if result.is_err() {
    tracing::debug!("could not set the sidebar width");
  }
}

fn window_width() -> f64 {
  web_sys::window()
    .and_then(|window| window.inner_width().ok())
    .and_then(|width| width.as_f64())
    .unwrap_or(1280.0)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_width_stays_between_the_limits_and_below_the_window_share() {
    assert_eq!(clamp_width(100.0, 1600.0), MIN_WIDTH_PX);
    assert_eq!(clamp_width(300.0, 1600.0), 300.0);
    assert_eq!(clamp_width(900.0, 1600.0), MAX_WIDTH_PX);
    // A small window: 40 % of 700 px is 280 px.
    assert_eq!(clamp_width(480.0, 700.0), 280.0);
    // Never below the minimum, however small the window.
    assert_eq!(clamp_width(480.0, 300.0), MIN_WIDTH_PX);
    assert_eq!(clamp_width(f64::NAN, 1600.0), DEFAULT_WIDTH_PX);
  }

  #[test]
  fn a_stored_width_is_read_back_only_when_it_makes_sense() {
    assert_eq!(parse_stored_width("320"), Some(320.0));
    assert_eq!(parse_stored_width(" 264 "), Some(264.0));
    assert_eq!(parse_stored_width("50"), None);
    assert_eq!(parse_stored_width("5000"), None);
    assert_eq!(parse_stored_width("wide"), None);
    assert_eq!(parse_stored_width(""), None);
  }
}
