//! The desktop app's own title bar (teamcraft_client_api::window_chrome). Without native
//! decorations the top bar is the title bar: its empty parts move the window, a double-click
//! maximizes, and the window controls sit where the desktop puts them. Screens without a top bar
//! (sign-in, guest join) get a drag strip along the top. Nothing of this exists in the browser.

use leptos::ev::MouseEvent;
use leptos::prelude::*;
use teamcraft_client_api::window_chrome::{
  TitleBar, WindowAction, WindowButton, WindowChrome, WindowPlatform,
};
use wasm_bindgen::JsCast;

use crate::auth::tauri;
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;

/// The shell's event when the maximized or fullscreen state or the title bar changed.
const WINDOW_CHROME_EVENT: &str = "window-chrome-changed";
/// Marks the elements a press on which moves the window (not their children).
pub const DRAG_ATTRIBUTE: &str = "data-window-drag";
/// Number of window buttons on each side, for the stylesheet.
const CONTROLS_LEFT_PROPERTY: &str = "--window-controls-left";
const CONTROLS_RIGHT_PROPERTY: &str = "--window-controls-right";

/// What the shell said about the window; `None` in the browser and until it answered.
#[derive(Clone, Copy)]
pub struct WindowChromeState(RwSignal<Option<WindowChrome>>);

/// Asks the shell about the window and keeps the answer current. Called once at the root, before
/// [`WindowControls`] and the top bar look for it.
pub fn provide_window_chrome() {
  let state = WindowChromeState(RwSignal::new(None));
  provide_context(state);
  if tauri::is_available() {
    let refresh = move || {
      leptos::task::spawn_local(async move { state.0.set(tauri::window_chrome().await) });
    };
    refresh();
    tauri::listen(WINDOW_CHROME_EVENT, refresh);
  }
}

/// The window controls and the drag strip, plus the attributes on `<html>` the stylesheet reads
/// (`data-titlebar`, `data-platform`, `data-window`). Mounted once, above every screen.
#[component]
pub fn WindowControls() -> impl IntoView {
  let Some(state) = use_context::<WindowChromeState>() else {
    return ().into_any();
  };
  let on_drag = window_drag_handler();
  Effect::new(move |_| apply_root_attributes(state.0.get().as_ref()));
  let custom = move || {
    state
      .0
      .get()
      .filter(|chrome| chrome.title_bar == TitleBar::Custom)
  };
  view! {
    {move || custom().map(|chrome| {
      let left = (!chrome.controls.left.is_empty())
        .then(|| view! { <ControlGroup buttons=chrome.controls.left.clone() side="left" /> });
      let right = (!chrome.controls.right.is_empty())
        .then(|| view! { <ControlGroup buttons=chrome.controls.right.clone() side="right" /> });
      view! {
        <div class="window-drag-strip" data-window-drag="" on:mousedown=on_drag></div>
        {left}
        {right}
      }
    })}
  }
  .into_any()
}

/// A `mousedown` handler that moves the window on a press on an element marked with
/// [`DRAG_ATTRIBUTE`]; a double press maximizes or restores it, as on a native title bar.
pub fn window_drag_handler() -> impl Fn(MouseEvent) + Copy + 'static {
  let maybe_state = use_context::<WindowChromeState>();
  move |event| {
    let is_custom = maybe_state
      .and_then(|state| state.0.get_untracked())
      .is_some_and(|chrome| chrome.title_bar == TitleBar::Custom);
    if is_custom {
      start_window_drag(&event);
    }
  }
}

fn start_window_drag(event: &MouseEvent) {
  if event.button() != 0 {
    return;
  }
  let is_drag_target = event
    .target()
    .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
    .is_some_and(|element| element.has_attribute(DRAG_ATTRIBUTE));
  if !is_drag_target {
    return;
  }
  event.prevent_default();
  let action = if event.detail() == 2 {
    WindowAction::ToggleMaximize
  } else {
    WindowAction::StartDragging
  };
  leptos::task::spawn_local(tauri::window_action(action));
}

#[component]
fn ControlGroup(buttons: Vec<WindowButton>, side: &'static str) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <div class=format!("window-controls {side}") role="group" aria-label=move || i18n.t("window-controls")>
      {buttons.into_iter().map(|button| view! { <ControlButton button=button /> }).collect_view()}
    </div>
  }
}

#[component]
fn ControlButton(button: WindowButton) -> impl IntoView {
  let i18n = use_i18n();
  let maybe_state = use_context::<WindowChromeState>();
  let is_maximized = move || {
    maybe_state
      .and_then(|state| state.0.get())
      .is_some_and(|chrome| chrome.is_maximized)
  };
  let label = move || {
    let key = match button {
      WindowButton::Minimize => "window-minimize",
      WindowButton::Maximize if is_maximized() => "window-restore",
      WindowButton::Maximize => "window-maximize",
      WindowButton::Close => "window-close",
    };
    i18n.t(key)
  };
  let icon = Signal::derive(move || match button {
    WindowButton::Minimize => IconName::WindowMinimize,
    WindowButton::Maximize if is_maximized() => IconName::WindowRestore,
    WindowButton::Maximize => IconName::WindowMaximize,
    WindowButton::Close => IconName::Close,
  });
  let action = match button {
    WindowButton::Minimize => WindowAction::Minimize,
    WindowButton::Maximize => WindowAction::ToggleMaximize,
    WindowButton::Close => WindowAction::Close,
  };
  let class = match button {
    WindowButton::Close => "window-btn close",
    WindowButton::Minimize | WindowButton::Maximize => "window-btn",
  };
  view! {
    <button
      type="button"
      class=class
      aria-label=label
      title=label
      on:click=move |_| leptos::task::spawn_local(tauri::window_action(action))
    >
      <Icon name=icon />
    </button>
  }
}

fn apply_root_attributes(maybe_chrome: Option<&WindowChrome>) {
  let Some(root) = web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.document_element())
    .and_then(|root| root.dyn_into::<web_sys::HtmlElement>().ok())
  else {
    return;
  };
  // Other properties on the root (brand colours, banner height) belong to others: only ours change.
  let style = root.style();
  let Some(chrome) = maybe_chrome.filter(|chrome| chrome.title_bar == TitleBar::Custom) else {
    for name in ["data-titlebar", "data-platform", "data-window"] {
      drop(root.remove_attribute(name));
    }
    for property in [CONTROLS_LEFT_PROPERTY, CONTROLS_RIGHT_PROPERTY] {
      drop(style.remove_property(property));
    }
    return;
  };
  let platform = match chrome.platform {
    WindowPlatform::Windows => "windows",
    WindowPlatform::Macos => "macos",
    WindowPlatform::Linux => "linux",
  };
  let window_state = if chrome.is_fullscreen {
    "fullscreen"
  } else if chrome.is_maximized {
    "maximized"
  } else {
    "normal"
  };
  // Errors only for invalid names, which these aren't.
  drop(root.set_attribute("data-titlebar", "custom"));
  drop(root.set_attribute("data-platform", platform));
  drop(root.set_attribute("data-window", window_state));
  // The top bar keeps its content clear of the controls (button counts; the stylesheet sizes them).
  drop(style.set_property(CONTROLS_LEFT_PROPERTY, &chrome.controls.left.len().to_string()));
  drop(style.set_property(CONTROLS_RIGHT_PROPERTY, &chrome.controls.right.len().to_string()));
}
