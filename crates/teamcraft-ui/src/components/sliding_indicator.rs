//! The selection indicator of a tab list, segmented control or the bottom navigation: one element
//! per list that slides from the old item to the new one instead of being redrawn on the new item
//! (issue #371). It moves with `transform` only (the compositor's work): a translate to the
//! selected item and a horizontal scale from the width it was laid out with.
//!
//! The indicator watches its list itself, so a list only places `<SlidingIndicator />` as its first
//! child: the selected item is the child with `aria-selected`, `aria-pressed` or `aria-current`.

use std::cell::Cell;
use std::rc::Rc;

use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

/// The selected item among the list's own children (not the indicator, not nested controls).
const SELECTED: &str = ":scope > :is([aria-selected='true'], [aria-pressed='true'], [aria-current='page'])";
/// The attributes that move the selection.
const SELECTION_ATTRIBUTES: [&str; 3] = ["aria-selected", "aria-pressed", "aria-current"];

/// The sliding indicator; the list's stylesheet rule (`.view-tabs > .tab-indicator`, …) gives it its
/// look. The list must be positioned (`position: relative`), so items measure against it.
#[component]
pub fn SlidingIndicator(
  /// Inside the selected item, the element the indicator covers (the bottom navigation's icon
  /// pill); the item itself when absent.
  #[prop(optional)]
  maybe_target: Option<&'static str>,
  /// How much narrower than its item the indicator is on each side (the view tabs' underline).
  #[prop(optional)]
  inset_px: f64,
) -> impl IntoView {
  let node = NodeRef::<html::Span>::new();
  Effect::new(move |_| {
    if let Some(span) = node.get() {
      follow_selection(
        span.unchecked_into(),
        Placing {
          maybe_target,
          inset_px,
        },
      );
    }
  });
  view! { <span class="tab-indicator" aria-hidden="true" node_ref=node></span> }
}

/// What the indicator covers in the selected item.
#[derive(Clone, Copy, Debug)]
struct Placing {
  maybe_target: Option<&'static str>,
  inset_px: f64,
}

/// Where the indicator goes, in the coordinates of its list.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Slot {
  x: f64,
  y: f64,
  width: f64,
  height: f64,
}

/// How the indicator gets to its new place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Move {
  /// The selection changed: slide there.
  Slide,
  /// First placement or the list was resized: be there at once.
  Jump,
}

/// Places the indicator now and again whenever the selection changes (slides) or the list or one
/// of its items is resized (jumps). The observers go with the component.
fn follow_selection(indicator: web_sys::HtmlElement, placing: Placing) {
  let Some(list) = indicator
    .parent_element()
    .and_then(|parent| parent.dyn_into::<web_sys::HtmlElement>().ok())
  else {
    return;
  };
  let laid_out_width = Rc::new(Cell::new(0.0));
  let place = {
    let indicator = indicator.clone();
    let list = list.clone();
    move |how: Move| place(&indicator, &list, placing, &laid_out_width, how)
  };
  place(Move::Jump);

  let on_select = Closure::<dyn FnMut()>::new({
    let place = place.clone();
    move || place(Move::Slide)
  });
  let on_resize = Closure::<dyn FnMut()>::new(move || place(Move::Jump));
  let Ok(selection) = web_sys::MutationObserver::new(on_select.as_ref().unchecked_ref()) else {
    return;
  };
  let Ok(resize) = web_sys::ResizeObserver::new(on_resize.as_ref().unchecked_ref()) else {
    return;
  };
  let options = web_sys::MutationObserverInit::new();
  options.set_attributes(true);
  options.set_subtree(true);
  options.set_child_list(true);
  let filter: js_sys::Array = SELECTION_ATTRIBUTES
    .iter()
    .map(|name| wasm_bindgen::JsValue::from_str(name))
    .collect();
  options.set_attribute_filter(&filter);
  if selection.observe_with_options(&list, &options).is_err() {
    tracing::debug!("could not observe a tab list");
  }
  resize.observe(&list);
  let items = list.children();
  for index in 0..items.length() {
    if let Some(item) = items.item(index) {
      resize.observe(&item);
    }
  }
  let observers = StoredValue::new_local((selection, resize, on_select, on_resize));
  on_cleanup(move || {
    observers.try_with_value(|(selection, resize, _, _)| {
      selection.disconnect();
      resize.disconnect();
    });
  });
}

/// Moves the indicator onto the selected item, or hides it when nothing is selected or the list is
/// not laid out (hidden).
fn place(
  indicator: &web_sys::HtmlElement,
  list: &web_sys::HtmlElement,
  placing: Placing,
  laid_out_width: &Cell<f64>,
  how: Move,
) {
  let classes = indicator.class_list();
  let Some(slot) = measure(list, placing) else {
    drop(classes.remove_1("has-selection"));
    return;
  };
  let style = indicator.style();
  let is_jump = how == Move::Jump || laid_out_width.get() <= 0.0;
  if is_jump {
    drop(classes.remove_1("is-live"));
    laid_out_width.set(slot.width);
    drop(style.set_property("--indicator-base-w", &px(slot.width)));
  }
  for (property, value) in [
    ("--indicator-x", px(slot.x)),
    ("--indicator-y", px(slot.y)),
    ("--indicator-h", px(slot.height)),
    (
      "--indicator-sx",
      format!("{:.4}", scale(slot.width, laid_out_width.get())),
    ),
  ] {
    drop(style.set_property(property, &value));
  }
  drop(classes.add_1("has-selection"));
  if is_jump {
    // Transitions come back once the jump has been drawn, so the next selection slides.
    let indicator = indicator.clone();
    request_animation_frame(move || drop(indicator.class_list().add_1("is-live")));
  }
}

/// The selected item's (or its target's) box relative to the list, narrowed by the inset.
fn measure(list: &web_sys::HtmlElement, placing: Placing) -> Option<Slot> {
  let selected = list.query_selector(SELECTED).ok().flatten()?;
  let target = match placing.maybe_target {
    Some(selector) => selected.query_selector(selector).ok().flatten()?,
    None => selected,
  };
  let target = target.dyn_into::<web_sys::HtmlElement>().ok()?;
  let (x, y) = offset_within(&target, list);
  let width = f64::from(target.offset_width()) - 2.0 * placing.inset_px;
  (width > 0.0).then(|| Slot {
    x: x + placing.inset_px,
    y,
    width,
    height: f64::from(target.offset_height()),
  })
}

/// Layout offset of `element` from `list` (transforms such as the press scale don't count).
fn offset_within(element: &web_sys::HtmlElement, list: &web_sys::HtmlElement) -> (f64, f64) {
  let (mut x, mut y) = (0.0, 0.0);
  let mut current = element.clone();
  while current != *list {
    x += f64::from(current.offset_left());
    y += f64::from(current.offset_top());
    let Some(parent) = current
      .offset_parent()
      .and_then(|parent| parent.dyn_into::<web_sys::HtmlElement>().ok())
    else {
      break;
    };
    current = parent;
  }
  (x, y)
}

/// The horizontal scale that turns the laid-out width into `width`.
fn scale(width: f64, laid_out_width: f64) -> f64 {
  if laid_out_width > 0.0 {
    width / laid_out_width
  } else {
    1.0
  }
}

fn px(value: f64) -> String {
  format!("{value:.2}px")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_scale_maps_the_laid_out_width_to_the_new_one() {
    assert!((scale(150.0, 100.0) - 1.5).abs() < f64::EPSILON);
    assert!((scale(80.0, 80.0) - 1.0).abs() < f64::EPSILON);
  }

  #[test]
  fn an_unmeasured_indicator_does_not_scale() {
    assert!((scale(120.0, 0.0) - 1.0).abs() < f64::EPSILON);
  }
}
