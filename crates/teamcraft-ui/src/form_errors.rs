//! Errors of a form, as `.claude/rules/forms.md` wants them: each at its field (`aria-invalid`,
//! `aria-describedby` naming the message), and a refused submit moves the focus to the first invalid
//! field and brings it into view, so an error is never out of sight (issue #1006).

use leptos::prelude::request_animation_frame;
use wasm_bindgen::JsCast;

use crate::motion::prefers_reduced_motion;

/// The id of the message of the field `field_id`, for its `aria-describedby`.
pub fn error_id(field_id: &str) -> String {
  format!("{field_id}-error")
}

/// After the errors are rendered (the next frame), focuses the first field marked
/// `aria-invalid="true"` inside the element `container_id` and scrolls it to the middle of the view.
pub fn focus_first_invalid(container_id: &'static str) {
  request_animation_frame(move || {
    let Some(field) = web_sys::window()
      .and_then(|window| window.document())
      .and_then(|document| document.get_element_by_id(container_id))
      .and_then(|container| container.query_selector("[aria-invalid=\"true\"]").ok().flatten())
      .and_then(|field| field.dyn_into::<web_sys::HtmlElement>().ok())
    else {
      return;
    };
    let options = web_sys::ScrollIntoViewOptions::new();
    options.set_block(web_sys::ScrollLogicalPosition::Center);
    if !prefers_reduced_motion() {
      options.set_behavior(web_sys::ScrollBehavior::Smooth);
    }
    field.scroll_into_view_with_scroll_into_view_options(&options);
    // The scroll above places it; focusing must not jump again.
    let focus = web_sys::FocusOptions::new();
    focus.set_prevent_scroll(true);
    // Ignored on failure: an element that can't take the focus still shows its error.
    drop(field.focus_with_options(&focus));
  });
}
