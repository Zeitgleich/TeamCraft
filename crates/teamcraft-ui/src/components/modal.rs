use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::motion::use_leave_ghost;
use crate::state::use_app_state;

const FOCUSABLE: &str = "a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex='-1'])";

/// Accessible modal dialog: labelled, closes on Escape or backdrop click, keeps Tab inside, and
/// returns focus to the control that opened it.
#[component]
pub fn Modal(
  /// Id of the element that labels the dialog.
  #[prop(into)]
  labelled_by: String,
  #[prop(optional, into)] class: &'static str,
  /// Whether the dialog fades out when it closes. The quick switcher, opened from the keyboard many
  /// times a day, comes and goes at once (#359).
  #[prop(default = true)]
  has_exit_motion: bool,
  children: Children,
) -> impl IntoView {
  let state = use_app_state();
  let dialog = NodeRef::<html::Div>::new();
  let backdrop = NodeRef::<html::Div>::new();
  if has_exit_motion {
    use_leave_ghost(backdrop);
  }
  let previously_focused = web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.active_element())
    .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok());
  let previously_focused = StoredValue::new_local(previously_focused);

  Effect::new(move |_| {
    if let Some(element) = dialog.get() {
      let target = element
        .query_selector("[data-autofocus]")
        .ok()
        .flatten()
        .and_then(|found| found.dyn_into::<web_sys::HtmlElement>().ok());
      match target {
        Some(target) => drop(target.focus()),
        None => drop(element.focus()),
      }
    }
  });
  on_cleanup(move || {
    previously_focused.with_value(|maybe_element| {
      // The opener may be gone (e.g. a menu item that closed with its menu); fall back to the account button.
      match maybe_element.as_ref().filter(|element| element.is_connected()) {
        Some(element) => drop(element.focus()),
        None => focus_selector(".avatar-btn"),
      }
    });
  });

  let on_keydown = move |ev: web_sys::KeyboardEvent| match ev.key().as_str() {
    "Escape" => {
      ev.stop_propagation();
      state.close_overlay();
    },
    "Tab" => trap_tab(&ev, dialog),
    _ => {},
  };

  view! {
    <div class="modal-backdrop" node_ref=backdrop on:mousedown=move |ev| {
      if ev.target() == ev.current_target() {
        state.close_overlay();
      }
    }>
      <div
        class=format!("modal {class}")
        role="dialog"
        aria-modal="true"
        aria-labelledby=labelled_by
        tabindex="-1"
        node_ref=dialog
        on:keydown=on_keydown
      >
        {children()}
      </div>
    </div>
  }
}

fn focus_selector(selector: &str) {
  let target = web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.query_selector(selector).ok().flatten())
    .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok());
  if let Some(target) = target {
    drop(target.focus());
  }
}

pub(crate) fn trap_tab(ev: &web_sys::KeyboardEvent, dialog: NodeRef<html::Div>) {
  let Some(root) = dialog.get() else { return };
  let Ok(list) = root.query_selector_all(FOCUSABLE) else {
    return;
  };
  let items: Vec<web_sys::HtmlElement> = (0..list.length())
    .filter_map(|i| list.item(i))
    .filter_map(|n| n.dyn_into().ok())
    .collect();
  let (Some(first), Some(last)) = (items.first(), items.last()) else {
    ev.prevent_default();
    return;
  };
  let active = web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.active_element());
  let at = |candidate: &web_sys::HtmlElement| active.as_ref().is_some_and(|a| a == candidate.as_ref());
  if ev.shift_key()
    && (at(first)
      || active
        .as_ref()
        .is_some_and(|a| a == root.as_ref() as &web_sys::Element))
  {
    ev.prevent_default();
    drop(last.focus());
  } else if !ev.shift_key() && at(last) {
    ev.prevent_default();
    drop(first.focus());
  }
}
