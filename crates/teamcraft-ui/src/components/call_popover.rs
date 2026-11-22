//! The popovers over the call's controls (share options, effects, cameras, microphones and
//! speakers, a video's quality): a non-modal dialog that takes the focus when it opens, closes with
//! Esc and gives the focus back to what opened it (WCAG 2.1.1, 2.4.3, 4.1.2).

use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

/// What takes the focus first: the checked radio (the only one in the tab order), else the first
/// control.
const FIRST_FOCUS: &str = "[role='radio'][tabindex='0'], input, button, [tabindex='0']";

/// A call popover. `labelled_by` is the id of its heading; `on_close` closes it (Esc). Tab leaves it
/// like any other part of the page and doesn't close it.
#[component]
pub fn CallPopover(
  class: &'static str,
  labelled_by: &'static str,
  on_close: Callback<()>,
  children: Children,
) -> impl IntoView {
  let node = NodeRef::<html::Div>::new();
  // What had the focus when the popover opened: its trigger (a click on a button focuses it in
  // every engine but Safari, where nothing is remembered and the focus stays where it falls).
  let maybe_opener = active_element();

  Effect::new(move |_| {
    if let Some(popover) = node.get() {
      focus_first(&popover);
    }
  });
  on_cleanup(move || {
    let Some(opener) = maybe_opener.clone() else {
      return;
    };
    // Closed with a pick or Esc: the focus is in the popover or has fallen to the page. A focus
    // that went elsewhere (another trigger) stays there.
    let is_lost = match (active_element(), node.get_untracked()) {
      (None, _) => true,
      (Some(active), Some(popover)) => popover.contains(Some(&active)) || is_body(&active),
      (Some(active), None) => is_body(&active),
    };
    if is_lost && opener.is_connected() {
      drop(opener.focus());
    }
  });

  view! {
    <div
      class=class
      role="dialog"
      aria-labelledby=labelled_by
      node_ref=node
      on:keydown=move |ev| {
        if ev.key() == "Escape" {
          ev.stop_propagation();
          ev.prevent_default();
          on_close.run(());
        }
      }
    >
      {children()}
    </div>
  }
}

fn focus_first(popover: &web_sys::HtmlDivElement) {
  if let Some(first) = popover
    .query_selector(FIRST_FOCUS)
    .ok()
    .flatten()
    .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
  {
    drop(first.focus());
  }
}

fn active_element() -> Option<web_sys::HtmlElement> {
  web_sys::window()?
    .document()?
    .active_element()?
    .dyn_into::<web_sys::HtmlElement>()
    .ok()
}

fn is_body(element: &web_sys::HtmlElement) -> bool {
  element.tag_name().eq_ignore_ascii_case("body")
}
