//! The desktop app's own title bar (teamcraft_client_api::window_chrome). Without native
//! decorations the top bar is the title bar: its empty parts move the window, a double-click
//! maximizes, and the window controls sit where the desktop puts them. Screens without a top bar
//! (sign-in, guest join) get a drag strip along the top. Nothing of this exists in the browser.

use leptos::ev::MouseEvent;
use leptos::prelude::*;
use teamcraft_client_api::window_chrome::{
  LinuxDesktop, TitleBar, WindowAction, WindowButton, WindowChrome, WindowPlatform,
};
use wasm_bindgen::JsCast;

use crate::auth::tauri;
use crate::i18n::use_i18n;

/// The shell's event when the maximized or fullscreen state or the title bar changed.
const WINDOW_CHROME_EVENT: &str = "window-chrome-changed";
/// Marks the elements a press on which moves the window (not their children).
pub const DRAG_ATTRIBUTE: &str = "data-window-drag";
/// Number of window buttons on each side, for the stylesheet.
const CONTROLS_LEFT_PROPERTY: &str = "--window-controls-left";
const CONTROLS_RIGHT_PROPERTY: &str = "--window-controls-right";
/// How far (in CSS pixels, per axis) the pointer may move with the button down and still be a click.
const CLICK_SLOP_PX: i32 = 4;

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
      let style = ControlStyle::new(chrome.platform, chrome.linux_desktop);
      let left = (!chrome.controls.left.is_empty())
        .then(|| view! { <ControlGroup buttons=chrome.controls.left.clone() side="left" style=style /> });
      let right = (!chrome.controls.right.is_empty())
        .then(|| view! { <ControlGroup buttons=chrome.controls.right.clone() side="right" style=style /> });
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

/// Click or drag on an element that does both (the brand: a link that also moves the window).
/// Starting the native drag on press would swallow the release, so no click would ever arrive;
/// instead the drag starts once the pointer has moved past [`CLICK_SLOP_PX`] with the button down.
#[derive(Clone, Copy)]
pub struct ClickOrDrag {
  maybe_press: StoredValue<Option<(i32, i32)>>,
  was_drag: StoredValue<bool>,
  maybe_state: Option<WindowChromeState>,
}

impl ClickOrDrag {
  pub fn new() -> Self {
    Self {
      maybe_press: StoredValue::new(None),
      was_drag: StoredValue::new(false),
      maybe_state: use_context::<WindowChromeState>(),
    }
  }

  /// `mousedown`: remembers where the press started.
  pub fn press(&self, event: &MouseEvent) {
    self.was_drag.set_value(false);
    let is_primary = event.button() == 0;
    self
      .maybe_press
      .set_value((is_primary && self.is_custom_title_bar()).then(|| position_of(event)));
  }

  /// `mousemove`: starts moving the window once the press became a drag.
  pub fn moved(&self, event: &MouseEvent) {
    let Some(start) = self.maybe_press.get_value() else {
      return;
    };
    if event.buttons() & 1 == 0 {
      self.maybe_press.set_value(None);
    } else if is_drag(start, position_of(event)) {
      self.maybe_press.set_value(None);
      self.was_drag.set_value(true);
      leptos::task::spawn_local(tauri::window_action(WindowAction::StartDragging));
    }
  }

  /// `mouseup` or `mouseleave`: the press is over.
  pub fn release(&self) {
    self.maybe_press.set_value(None);
  }

  /// `click`: whether it ended a drag (and must not navigate).
  pub fn take_was_drag(&self) -> bool {
    let was_drag = self.was_drag.get_value();
    self.was_drag.set_value(false);
    was_drag
  }

  fn is_custom_title_bar(&self) -> bool {
    self
      .maybe_state
      .and_then(|state| state.0.get_untracked())
      .is_some_and(|chrome| chrome.title_bar == TitleBar::Custom)
  }
}

impl Default for ClickOrDrag {
  fn default() -> Self {
    Self::new()
  }
}

/// Whether a pointer that went down at `start` and is now at `now` is dragging, not clicking.
fn is_drag(start: (i32, i32), now: (i32, i32)) -> bool {
  (now.0 - start.0).abs() > CLICK_SLOP_PX || (now.1 - start.1).abs() > CLICK_SLOP_PX
}

fn position_of(event: &MouseEvent) -> (i32, i32) {
  (event.screen_x(), event.screen_y())
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

/// The group's own padding (the margin to the window edge on Linux) still moves the window.
#[component]
fn ControlGroup(buttons: Vec<WindowButton>, side: &'static str, style: ControlStyle) -> impl IntoView {
  let i18n = use_i18n();
  let on_drag = window_drag_handler();
  view! {
    <div
      class=format!("window-controls {side}")
      role="group"
      aria-label=move || i18n.t("window-controls")
      data-window-drag=""
      on:mousedown=on_drag
    >
      {buttons
        .into_iter()
        .map(|button| view! { <ControlButton button=button style=style /> })
        .collect_view()}
    </div>
  }
}

#[component]
fn ControlButton(button: WindowButton, style: ControlStyle) -> impl IntoView {
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
  let action = match button {
    WindowButton::Minimize => WindowAction::Minimize,
    WindowButton::Maximize => WindowAction::ToggleMaximize,
    WindowButton::Close => WindowAction::Close,
  };
  let class = match button {
    WindowButton::Close => "window-btn close",
    WindowButton::Minimize | WindowButton::Maximize => "window-btn",
  };
  let glyph = style.glyph();
  let path = move || glyph.path(button, is_maximized());
  view! {
    <button
      type="button"
      class=class
      aria-label=label
      title=label
      on:click=move |_| leptos::task::spawn_local(tauri::window_action(action))
    >
      <svg
        class="window-glyph"
        viewBox=glyph.view_box
        width=glyph.size_px
        height=glyph.size_px
        fill="none"
        stroke="currentColor"
        stroke-width=glyph.stroke_width
        stroke-linecap=glyph.line_cap
        stroke-linejoin="round"
        shape-rendering=glyph.shape_rendering
        aria-hidden="true"
        focusable="false"
      >
        <path d=path />
      </svg>
    </button>
  }
}

/// Whose window buttons the controls look like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ControlStyle {
  /// Windows 11 caption buttons.
  Windows,
  /// libadwaita (GNOME, and the default on other Linux desktops).
  Adwaita,
  /// KDE Plasma's Breeze decoration.
  Breeze,
}

/// How a style draws its glyphs (inline SVG, no icon font).
#[derive(Clone, Copy)]
struct GlyphSet {
  view_box: &'static str,
  size_px: &'static str,
  stroke_width: &'static str,
  line_cap: &'static str,
  shape_rendering: &'static str,
  minimize: &'static str,
  maximize: &'static str,
  restore: &'static str,
  close: &'static str,
}

/// Segoe Fluent Icons' caption glyphs: 10 px hairlines on the pixel grid, square ends.
const WINDOWS_GLYPHS: GlyphSet = GlyphSet {
  view_box: "0 0 10 10",
  size_px: "10",
  stroke_width: "1",
  line_cap: "butt",
  shape_rendering: "crispEdges",
  minimize: "M0 5.5h10",
  maximize: "M0.5 0.5h9v9h-9z",
  restore: "M2.5 2.5V0.5h7v7h-2 M0.5 2.5h7v7h-7z",
  close: "M0 0l10 10M10 0 0 10",
};

/// Adwaita's symbolic window icons on a 16 px grid.
const ADWAITA_GLYPHS: GlyphSet = GlyphSet {
  view_box: "0 0 16 16",
  size_px: "16",
  stroke_width: "1.5",
  line_cap: "round",
  shape_rendering: "geometricPrecision",
  minimize: "M4.5 10.75h7",
  maximize: "M4.75 4.75h6.5v6.5h-6.5z",
  restore: "M6.25 6.25h5v5h-5z M4.25 9.25v-5h5",
  close: "M4.75 4.75l6.5 6.5M11.25 4.75l-6.5 6.5",
};

/// Breeze: chevrons for minimize and maximize, a diamond for restore.
const BREEZE_GLYPHS: GlyphSet = GlyphSet {
  view_box: "0 0 16 16",
  size_px: "16",
  stroke_width: "1.5",
  line_cap: "round",
  shape_rendering: "geometricPrecision",
  minimize: "M3.5 6l4.5 4.5 4.5-4.5",
  maximize: "M3.5 10.5 8 6l4.5 4.5",
  restore: "M8 3.25 12.75 8 8 12.75 3.25 8z",
  close: "M4.5 4.5l7 7M11.5 4.5l-7 7",
};

impl ControlStyle {
  fn new(platform: WindowPlatform, linux_desktop: LinuxDesktop) -> Self {
    match (platform, linux_desktop) {
      (WindowPlatform::Windows, _) => Self::Windows,
      (WindowPlatform::Linux, LinuxDesktop::Kde) => Self::Breeze,
      // macOS has no drawn controls (its traffic lights are native).
      (WindowPlatform::Linux | WindowPlatform::Macos, _) => Self::Adwaita,
    }
  }

  fn glyph(self) -> GlyphSet {
    match self {
      Self::Windows => WINDOWS_GLYPHS,
      Self::Adwaita => ADWAITA_GLYPHS,
      Self::Breeze => BREEZE_GLYPHS,
    }
  }
}

impl GlyphSet {
  fn path(&self, button: WindowButton, is_maximized: bool) -> &'static str {
    match button {
      WindowButton::Minimize => self.minimize,
      WindowButton::Maximize if is_maximized => self.restore,
      WindowButton::Maximize => self.maximize,
      WindowButton::Close => self.close,
    }
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
    for name in [
      "data-titlebar",
      "data-platform",
      "data-linux-desktop",
      "data-window",
    ] {
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
  let linux_desktop = match chrome.linux_desktop {
    LinuxDesktop::Gnome => "gnome",
    LinuxDesktop::Kde => "kde",
    LinuxDesktop::Other => "other",
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
  drop(root.set_attribute("data-linux-desktop", linux_desktop));
  drop(root.set_attribute("data-window", window_state));
  // The top bar keeps its content clear of the controls (button counts; the stylesheet sizes them).
  drop(style.set_property(CONTROLS_LEFT_PROPERTY, &chrome.controls.left.len().to_string()));
  drop(style.set_property(CONTROLS_RIGHT_PROPERTY, &chrome.controls.right.len().to_string()));
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn kde_gets_breeze_and_every_other_linux_desktop_adwaita() {
    assert_eq!(
      ControlStyle::new(WindowPlatform::Linux, LinuxDesktop::Kde),
      ControlStyle::Breeze
    );
    assert_eq!(
      ControlStyle::new(WindowPlatform::Linux, LinuxDesktop::Gnome),
      ControlStyle::Adwaita
    );
    assert_eq!(
      ControlStyle::new(WindowPlatform::Linux, LinuxDesktop::Other),
      ControlStyle::Adwaita
    );
    assert_eq!(
      ControlStyle::new(WindowPlatform::Windows, LinuxDesktop::Other),
      ControlStyle::Windows
    );
  }

  #[test]
  fn maximize_turns_into_restore_when_maximized() {
    for style in [ControlStyle::Windows, ControlStyle::Adwaita, ControlStyle::Breeze] {
      let glyph = style.glyph();
      assert_ne!(
        glyph.path(WindowButton::Maximize, false),
        glyph.path(WindowButton::Maximize, true)
      );
      assert_eq!(
        glyph.path(WindowButton::Close, false),
        glyph.path(WindowButton::Close, true)
      );
    }
  }

  #[test]
  fn a_press_without_movement_is_a_click() {
    assert!(!is_drag((100, 50), (100, 50)));
  }

  #[test]
  fn jitter_within_the_slop_is_still_a_click() {
    assert!(!is_drag((100, 50), (104, 46)));
  }

  #[test]
  fn moving_past_the_slop_in_any_direction_is_a_drag() {
    assert!(is_drag((100, 50), (105, 50)));
    assert!(is_drag((100, 50), (100, 44)));
    assert!(is_drag((100, 50), (60, 90)));
  }
}
