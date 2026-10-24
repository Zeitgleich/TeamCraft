//! The main window's title bar (teamcraft_client_api::window_chrome). By default the window has no
//! native decorations and the UI's top bar is the title bar; on macOS the real traffic lights stay
//! over the top bar. The UI asks for the layout, drives the window through `window_action` and is
//! told when the maximized or fullscreen state changes, so its restore button stays right.

use std::path::PathBuf;
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager, Runtime, State, WebviewWindow, WebviewWindowBuilder, Window};
use teamcraft_client_api::window_chrome::{
  ControlLayout, LinuxDesktop, TitleBar, WindowAction, WindowChrome, WindowPlatform,
};

use crate::shell_prefs::ShellPrefsState;

/// Emitted when the window chrome changed (maximized, fullscreen, title bar); the UI asks again.
pub const WINDOW_CHROME_EVENT: &str = "window-chrome-changed";
/// Packagers and admins can make the system title bar the default (the user's choice still wins).
pub const TITLE_BAR_ENV: &str = "TEAMCRAFT_TITLE_BAR";
/// Top bar height in the UI (`--topbar-h`); the traffic lights are centred in it.
#[cfg(target_os = "macos")]
const TOP_BAR_HEIGHT: f64 = 44.0;
/// Size of a traffic light.
#[cfg(target_os = "macos")]
const TRAFFIC_LIGHT_SIZE: f64 = 14.0;
#[cfg(target_os = "macos")]
const TRAFFIC_LIGHT_INSET: f64 = 16.0;

/// What the window was created with, and the last state the UI was told about.
pub struct WindowChromeState {
  title_bar: TitleBar,
  last_state: Mutex<Option<(bool, bool)>>,
}

#[derive(Debug, serde::Serialize)]
pub struct WindowChromeError {
  kind: &'static str,
  message: String,
}

impl WindowChromeState {
  pub fn new(title_bar: TitleBar) -> Self {
    Self {
      title_bar,
      last_state: Mutex::new(None),
    }
  }
}

/// The title bar the window should be created with.
pub fn configure<R: Runtime, M: Manager<R>>(
  builder: WebviewWindowBuilder<'_, R, M>,
  title_bar: TitleBar,
) -> WebviewWindowBuilder<'_, R, M> {
  match title_bar {
    TitleBar::System => builder.decorations(true),
    TitleBar::Custom => configure_custom(builder),
  }
}

/// macOS: the native title bar turns transparent and the content goes under it, so the traffic
/// lights (and with them fullscreen, zoom and the window menu) stay real.
#[cfg(target_os = "macos")]
fn configure_custom<R: Runtime, M: Manager<R>>(
  builder: WebviewWindowBuilder<'_, R, M>,
) -> WebviewWindowBuilder<'_, R, M> {
  builder
    .decorations(true)
    .title_bar_style(tauri::TitleBarStyle::Overlay)
    .hidden_title(true)
    .traffic_light_position(tauri::LogicalPosition::new(
      TRAFFIC_LIGHT_INSET,
      (TOP_BAR_HEIGHT - TRAFFIC_LIGHT_SIZE) / 2.0,
    ))
}

/// Windows and Linux: no decorations. Windows keeps the system shadow, the rounded corners of
/// Windows 11 and the resize border; on Linux tao provides the resize edges of an undecorated
/// window (Wayland and X11).
#[cfg(not(target_os = "macos"))]
fn configure_custom<R: Runtime, M: Manager<R>>(
  builder: WebviewWindowBuilder<'_, R, M>,
) -> WebviewWindowBuilder<'_, R, M> {
  builder.decorations(false).shadow(true)
}

/// Switches the running window when the user changes the setting. macOS can't hide the title
/// text again once shown, so there the change applies at the next start.
pub fn apply<R: Runtime>(window: &WebviewWindow<R>, title_bar: TitleBar) {
  if !is_live_switchable() {
    return;
  }
  if let Err(error) = window.set_decorations(title_bar == TitleBar::System) {
    tracing::warn!(%error, "could not switch the title bar");
    return;
  }
  // The UI reads the new title bar from the preferences, which are already saved.
  notify(window.app_handle());
}

/// Tells the UI when the window was maximized, restored or went fullscreen.
pub fn on_resized<R: Runtime>(window: &Window<R>) {
  let state = (
    window.is_maximized().unwrap_or(false),
    window.is_fullscreen().unwrap_or(false),
  );
  let chrome = window.state::<WindowChromeState>();
  let mut last = chrome
    .last_state
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner);
  if *last == Some(state) {
    return;
  }
  *last = Some(state);
  drop(last);
  notify(window.app_handle());
}

#[tauri::command]
pub fn window_chrome(
  window: WebviewWindow,
  chrome: State<'_, WindowChromeState>,
  prefs: State<'_, ShellPrefsState>,
) -> WindowChrome {
  // Live switching follows the saved preference; elsewhere the window keeps what it started with.
  let title_bar = if is_live_switchable() {
    prefs.get().title_bar
  } else {
    chrome.title_bar
  };
  let platform = platform();
  let linux_desktop = match platform {
    WindowPlatform::Linux => linux_desktop(),
    WindowPlatform::Windows | WindowPlatform::Macos => LinuxDesktop::Other,
  };
  let controls = match (title_bar, platform) {
    (TitleBar::Custom, WindowPlatform::Windows) => ControlLayout::windows(),
    (TitleBar::Custom, WindowPlatform::Linux) => linux_layout(&window, linux_desktop),
    _ => ControlLayout::default(),
  };
  WindowChrome {
    title_bar,
    platform,
    linux_desktop,
    controls,
    is_maximized: window.is_maximized().unwrap_or(false),
    is_fullscreen: window.is_fullscreen().unwrap_or(false),
    is_live_switchable: is_live_switchable(),
  }
}

#[tauri::command]
pub fn window_action(window: WebviewWindow, action: WindowAction) -> Result<(), WindowChromeError> {
  let result = match action {
    WindowAction::Minimize => window.minimize(),
    WindowAction::ToggleMaximize => toggle_maximize(&window),
    // Goes through `CloseRequested`, so closing to the tray still applies.
    WindowAction::Close => window.close(),
    WindowAction::StartDragging => window.start_dragging(),
  };
  result.map_err(|error| WindowChromeError {
    kind: "failed",
    message: error.to_string(),
  })
}

fn toggle_maximize(window: &WebviewWindow) -> tauri::Result<()> {
  // A double-click on the title bar of a fullscreen window leaves fullscreen instead.
  if window.is_fullscreen()? {
    return window.set_fullscreen(false);
  }
  if window.is_maximized()? {
    window.unmaximize()
  } else {
    window.maximize()
  }
}

fn notify<R: Runtime>(app: &AppHandle<R>) {
  if let Err(error) = app.emit(WINDOW_CHROME_EVENT, ()) {
    tracing::debug!(%error, "could not notify the webview");
  }
}

fn is_live_switchable() -> bool {
  !cfg!(target_os = "macos")
}

fn platform() -> WindowPlatform {
  if cfg!(target_os = "macos") {
    WindowPlatform::Macos
  } else if cfg!(windows) {
    WindowPlatform::Windows
  } else {
    WindowPlatform::Linux
  }
}

/// The running desktop environment (also set inside the Flatpak), for the look of the controls.
fn linux_desktop() -> LinuxDesktop {
  let maybe_current = std::env::var("XDG_CURRENT_DESKTOP").ok();
  let maybe_session = std::env::var("XDG_SESSION_DESKTOP").ok();
  LinuxDesktop::from_xdg(maybe_current.as_deref(), maybe_session.as_deref())
}

/// The desktop's button layout: on KDE KWin's own setting when `kwinrc` is readable (not inside
/// the Flatpak), otherwise as GTK sees it: GNOME's setting, KDE's (written for GTK apps), and
/// inside the Flatpak the settings portal.
fn linux_layout(window: &WebviewWindow, desktop: LinuxDesktop) -> ControlLayout {
  let maybe_kwin = (desktop == LinuxDesktop::Kde)
    .then(kwinrc_path)
    .flatten()
    .and_then(|path| std::fs::read_to_string(path).ok())
    .and_then(|text| ControlLayout::from_kwinrc(&text));
  maybe_kwin.unwrap_or_else(|| gtk_layout(window))
}

/// `$XDG_CONFIG_HOME/kwinrc`, or `$HOME/.config/kwinrc`.
fn kwinrc_path() -> Option<PathBuf> {
  let maybe_config = std::env::var_os("XDG_CONFIG_HOME")
    .map(PathBuf::from)
    .filter(|path| path.is_absolute());
  let config =
    maybe_config.or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
  Some(config.join("kwinrc"))
}

#[cfg(target_os = "linux")]
fn gtk_layout(window: &WebviewWindow) -> ControlLayout {
  use gtk::prelude::{GtkSettingsExt, WidgetExt};

  let maybe_layout = window
    .gtk_window()
    .ok()
    .and_then(|gtk_window| gtk_window.settings())
    .and_then(|settings| settings.gtk_decoration_layout());
  match maybe_layout {
    Some(layout) => ControlLayout::from_decoration_layout(&layout),
    None => ControlLayout::windows(),
  }
}

#[cfg(not(target_os = "linux"))]
fn gtk_layout(_window: &WebviewWindow) -> ControlLayout {
  ControlLayout::windows()
}
