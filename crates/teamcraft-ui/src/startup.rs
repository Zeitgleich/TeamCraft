//! The end of the start: tells the loading screen of index.html (loader.js) that the app's first real
//! view is on screen, so the screen hands over straight to it. Until then it covers the session check
//! and the account's data opening, instead of the app showing a card of its own in between.

use std::cell::Cell;

use leptos::prelude::*;

/// Set on `<html>` once the first view is there, for a loader that starts listening late.
const SHOWN_ATTRIBUTE: &str = "data-first-view";
/// Sent to `window` at the same time.
const SHOWN_EVENT: &str = "teamcraft-first-view";

thread_local! {
  static IS_SHOWN: Cell<bool> = const { Cell::new(false) };
}

/// Marks the view being built as the first real one: once it is rendered (effects run after the
/// DOM is in place), the loading screen hands over. Only the first call of a page load counts.
pub fn first_view() {
  Effect::new(|_| first_view_shown());
}

/// The first view is on screen now.
pub fn first_view_shown() {
  if IS_SHOWN.with(|shown| shown.replace(true)) {
    return;
  }
  let Some(window) = web_sys::window() else { return };
  if let Some(root) = window.document().and_then(|document| document.document_element()) {
    // Without the attribute the loader still gets the event, or goes by its own deadline.
    let _ = root.set_attribute(SHOWN_ATTRIBUTE, "");
  }
  if let Ok(event) = web_sys::Event::new(SHOWN_EVENT) {
    // Nothing listening (no loading screen, as in tests) is not an error.
    let _ = window.dispatch_event(&event);
  }
}

/// The loading screen goes at once (the panic overlay must not sit behind it).
pub fn remove_loading_screen() {
  if let Some(screen) = web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.get_element_by_id("tc-loader"))
  {
    screen.remove();
  }
}
