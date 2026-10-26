//! Links in the desktop app open in the system browser (#548). The webview's new-window hook did
//! not fire for `target="_blank"` clicks on WebKitGTK and WKWebView, so the page routes the click
//! itself: one delegated listener for every anchor (messages, preview cards, "what's new",
//! sign-in links), including middle and Ctrl/Cmd clicks. The browser build keeps the browser's
//! own behaviour.

use leptos::prelude::*;
use teamcraft_client_api::failure::ActionFailure;
use teamcraft_client_api::links::is_external_link;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use crate::auth::tauri;
use crate::feedback;

/// What the page does with a click on an anchor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkRoute {
  /// Leave it to the webview: in-page anchors, a browser tab, links the shell may not open.
  Native,
  /// Cancel the click and hand the address to the system.
  OpenExternally(String),
}

/// A right click on a link: the address and where the menu opens (viewport pixels).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkMenuTarget {
  pub url: String,
  pub x: i32,
  pub y: i32,
}

/// Starts the click routing in the desktop shell; a no-op in the browser. A right click on a link
/// fills `menu`, which the app shows as its own menu instead of the webview's (open, copy,
/// download, new tab); elsewhere the webview's menu only stays for text (see
/// [`keeps_webview_menu`]).
pub fn start(menu: RwSignal<Option<LinkMenuTarget>>) {
  if !tauri::is_available() {
    return;
  }
  let Some(document) = web_sys::window().and_then(|window| window.document()) else {
    return;
  };
  let on_context_menu = Closure::<dyn FnMut(web_sys::MouseEvent)>::new(move |event: web_sys::MouseEvent| {
    if let Some(anchor) = anchor_of(&event)
      && let LinkRoute::OpenExternally(url) = route_click(&anchor.href(), 0, false)
    {
      event.prevent_default();
      drop(menu.try_set(Some(LinkMenuTarget {
        url,
        x: event.client_x(),
        y: event.client_y(),
      })));
      return;
    }
    // The webview's menu stays only on text: text fields (cut, paste, spelling) and selected text
    // (copy), where the shell has taken out everything that treats the app as a web page. Elsewhere
    // there is no menu (#719), except the app's own: files (`attachment_menu`) and messages (their
    // actions, `message_row`).
    if !keeps_webview_menu(&event) {
      event.prevent_default();
    }
  });
  if document
    .add_event_listener_with_callback_and_bool("contextmenu", on_context_menu.as_ref().unchecked_ref(), true)
    .is_ok()
  {
    on_context_menu.forget();
  }
  // "auxclick" carries the middle button; "click" the primary one, also with Ctrl/Cmd/Shift held.
  for event in ["click", "auxclick"] {
    let closure = Closure::<dyn FnMut(web_sys::MouseEvent)>::new(on_click);
    // Capture phase: a component's own handler must not swallow the click first.
    if document
      .add_event_listener_with_callback_and_bool(event, closure.as_ref().unchecked_ref(), true)
      .is_ok()
    {
      // Lives as long as the page.
      closure.forget();
    }
  }
}

/// Opens `url` from code (bookmarks): through the shell in the desktop app, in a new tab in the
/// browser. Only addresses the shell may open get that far.
pub fn open(url: &str) {
  if tauri::is_available() {
    open_in_system(url.to_owned());
  } else if let Some(window) = web_sys::window() {
    drop(window.open_with_url_and_target_and_features(url, "_blank", "noopener,noreferrer"));
  }
}

/// The routing decision for a click on an anchor, apart from the DOM so it can be tested.
pub fn route_click(href: &str, button: i16, is_download: bool) -> LinkRoute {
  // Primary and middle button open links; the context menu's button must stay the webview's.
  let is_opening_button = button == 0 || button == 1;
  if is_opening_button && !is_download && is_external_link(href) {
    LinkRoute::OpenExternally(href.to_owned())
  } else {
    LinkRoute::Native
  }
}

fn on_click(event: web_sys::MouseEvent) {
  if event.default_prevented() {
    return;
  }
  let Some(anchor) = anchor_of(&event) else {
    return;
  };
  // `href()` is the resolved address: relative links become app URLs and stay native.
  let route = route_click(&anchor.href(), event.button(), anchor.has_attribute("download"));
  if let LinkRoute::OpenExternally(url) = route {
    event.prevent_default();
    open_in_system(url);
  }
}

/// Whether a right click keeps the webview's own menu: on text ([`is_on_text`]), with Shift held in
/// a debug build, or everywhere when the shell was started with `TEAMCRAFT_DEVTOOLS=1` (the web
/// inspector's "Inspect Element").
fn keeps_webview_menu(event: &web_sys::MouseEvent) -> bool {
  (cfg!(debug_assertions) && event.shift_key()) || wants_native_menu() || is_on_text(event)
}

/// A right click on a text field or with text selected: the webview's text menu (cut, copy,
/// paste, spelling) is the one to show.
pub fn is_on_text(event: &web_sys::MouseEvent) -> bool {
  let is_text_field = event
    .target()
    .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
    .and_then(|element| {
      element
        .closest("input, textarea, [contenteditable]")
        .ok()
        .flatten()
    })
    .is_some();
  let has_selection = web_sys::window()
    .and_then(|window| window.get_selection().ok().flatten())
    .is_some_and(|selection| !selection.is_collapsed());
  is_text_field || has_selection
}

/// The shell set the devtools flag (`TEAMCRAFT_DEVTOOLS=1`).
pub fn wants_native_menu() -> bool {
  web_sys::window()
    .and_then(|window| js_sys::Reflect::get(&window, &"__TEAMCRAFT_NATIVE_MENU__".into()).ok())
    .is_some_and(|flag| flag.is_truthy())
}

fn anchor_of(event: &web_sys::MouseEvent) -> Option<web_sys::HtmlAnchorElement> {
  event
    .target()
    .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
    .and_then(|element| element.closest("a[href]").ok().flatten())
    .and_then(|element| element.dyn_into::<web_sys::HtmlAnchorElement>().ok())
}

fn open_in_system(url: String) {
  leptos::task::spawn_local(async move {
    let args = serde_json::json!({ "url": url });
    if let Err(error) = tauri::call::<()>("open_external", &args).await {
      feedback::report_failure(ActionFailure::from_wire(&error));
    }
  });
}

#[cfg(test)]
mod tests {
  use super::{LinkRoute, route_click};

  #[test]
  fn external_links_open_in_the_system() {
    for url in [
      "https://example.com/a",
      "http://example.com",
      "mailto:a@example.com",
    ] {
      for button in [0, 1] {
        assert_eq!(
          route_click(url, button, false),
          LinkRoute::OpenExternally(url.to_owned()),
          "{url}"
        );
      }
    }
  }

  #[test]
  fn other_links_and_buttons_stay_with_the_webview() {
    for url in [
      "file:///etc/passwd",
      "tauri://localhost/#/x",
      "javascript:alert(1)",
      "about:blank",
      "",
    ] {
      assert_eq!(route_click(url, 0, false), LinkRoute::Native, "{url}");
    }
    // Right click (2) opens the context menu; downloads are not navigations.
    assert_eq!(route_click("https://example.com/", 2, false), LinkRoute::Native);
    assert_eq!(
      route_click("https://example.com/f.zip", 0, true),
      LinkRoute::Native
    );
  }
}
