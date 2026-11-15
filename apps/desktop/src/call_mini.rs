//! The small call window (#2077): during a call, while the main window is away (minimized, hidden
//! to the tray or behind other windows), a small window above all others with who talks, the
//! call's time and mute, camera, sound, hang up and "back to the call". The call engine stays with
//! the main window: its UI hands over what the window shows ([`CallMini`]) and the user's
//! [`CallMiniPolicy`]; this module decides from the main window's focus when the window shows,
//! and hands its buttons to the main window as tray actions, the path the tray menu's call
//! controls take ([`crate::tray`]).
//!
//! The window doesn't take the focus when it appears, can be dragged anywhere, and opens where the
//! user last left it (`call-mini.json` in the config directory).
//!
//! Wayland: no client can place itself or stay above other windows there, so the compositor
//! decides where the window opens and whether it stays on top (docs/notifications.md, "Calls").
//!
//! The window's own capability (`capabilities/call-mini.json`) allows only the commands it needs.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{
  AppHandle, Emitter, LogicalPosition, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
  Window,
};
use teamcraft_client_api::call_mini::{CALL_MINI_CHANGED_EVENT, CALL_MINI_LABEL, CallMini, CallMiniPolicy};
use teamcraft_client_api::tray_menu::{TRAY_ACTION_EVENT, TrayAction};

use crate::tray::MAIN_WINDOW;

/// The window's size in logical pixels.
const WIDTH: f64 = 320.0;
const HEIGHT: f64 = 120.0;
/// Distance from the screen's edges where it first opens.
const MARGIN: f64 = 16.0;
/// Room left for the incoming-call window in the same corner (its height and margin), so a second
/// call that rings during this one doesn't cover the window.
const INCOMING_ROOM: f64 = 96.0;
/// Where the window was last left, in the app's config directory.
const POSITION_FILE: &str = "call-mini.json";

/// What the small window shows and whether it should.
#[derive(Default)]
pub struct CallMiniState {
  inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
  /// The user's call, while there is one.
  maybe_call: Option<CallMini>,
  policy: CallMiniPolicy,
  /// The main window is minimized, hidden or behind other windows; `None` until it was asked.
  maybe_is_main_away: Option<bool>,
  /// Where the user moved the window (logical pixels), kept for the next call.
  maybe_position: Option<SavedPosition>,
}

/// The window's place, as stored.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
struct SavedPosition {
  x: f64,
  y: f64,
}

/// The UI's call changed (or ended: `None`), or the user's choice of when the window shows.
#[tauri::command(rename_all = "snake_case")]
pub fn call_mini_set(
  app: AppHandle,
  state: State<'_, CallMiniState>,
  maybe_call: Option<CallMini>,
  policy: CallMiniPolicy,
) {
  {
    let mut inner = state.lock();
    inner.maybe_call = maybe_call;
    inner.policy = policy;
  }
  reconcile(&app);
}

/// The window's page: the call to show.
#[tauri::command(rename_all = "snake_case")]
pub fn call_mini_info(state: State<'_, CallMiniState>) -> Option<CallMini> {
  state.lock().maybe_call.clone()
}

/// The window's page has rendered: the window appears (it starts hidden, so it never shows empty).
#[tauri::command(rename_all = "snake_case")]
pub fn call_mini_ready(window: WebviewWindow) {
  if window.label() != CALL_MINI_LABEL {
    return;
  }
  if let Err(error) = window.show() {
    tracing::warn!(%error, "could not show the small call window");
  }
}

/// A button of the window, handed to the main window as the tray's call control with the same
/// meaning. "Back to the call" and a hang up that asks first bring the main window to the front.
#[tauri::command(rename_all = "snake_case")]
pub fn call_mini_action(app: AppHandle, window: WebviewWindow, action: TrayAction) {
  if window.label() != CALL_MINI_LABEL || !is_window_action(action) {
    return;
  }
  if action.shows_window() {
    crate::tray::show_main_window(&app);
  }
  if let Err(error) = app.emit_to(MAIN_WINDOW, TRAY_ACTION_EVENT, action.id()) {
    tracing::debug!(%error, "could not hand the small call window's button to the main window");
  }
}

/// The main window gained or lost the focus: the window shows while it is away, as the policy says.
pub fn main_focus_changed(app: &AppHandle, is_focused: bool) {
  app.state::<CallMiniState>().lock().maybe_is_main_away = Some(!is_focused);
  reconcile(app);
}

/// The main window's page (re)loads: what the old page handed over may be stale, and nothing would
/// take the window's buttons until the new page runs. The window closes; the new page hands the
/// call over again if it still runs (the call itself runs in the shell and goes on).
pub fn main_page_started(app: &AppHandle) {
  app.state::<CallMiniState>().lock().maybe_call = None;
  reconcile(app);
}

/// The user moved the window: it opens there next time.
pub fn moved(window: &Window, position: tauri::PhysicalPosition<i32>) {
  if window.label() != CALL_MINI_LABEL {
    return;
  }
  let Ok(scale) = window.scale_factor() else {
    return;
  };
  let logical = position.to_logical::<f64>(scale);
  window.state::<CallMiniState>().lock().maybe_position = Some(SavedPosition {
    x: logical.x,
    y: logical.y,
  });
}

/// The window's buttons; everything else of the tray stays the tray's.
fn is_window_action(action: TrayAction) -> bool {
  matches!(
    action,
    TrayAction::Mute
      | TrayAction::Unmute
      | TrayAction::CameraOn
      | TrayAction::CameraOff
      | TrayAction::SoundOff
      | TrayAction::SoundOn
      | TrayAction::LeaveCall
      | TrayAction::LeaveCallAsking
      | TrayAction::OpenCall
  )
}

/// Opens, updates or closes the window to match the call, the policy and the main window.
fn reconcile(app: &AppHandle) {
  let state = app.state::<CallMiniState>();
  let is_main_away = {
    let maybe_known = state.lock().maybe_is_main_away;
    maybe_known.unwrap_or_else(|| is_main_away(app))
  };
  let should_show = {
    let mut inner = state.lock();
    inner.maybe_is_main_away = Some(is_main_away);
    inner.maybe_call.is_some() && inner.policy.shows(is_main_away)
  };
  match (should_show, app.get_webview_window(CALL_MINI_LABEL)) {
    (true, Some(_)) => {
      if let Err(error) = app.emit_to(CALL_MINI_LABEL, CALL_MINI_CHANGED_EVENT, ()) {
        tracing::debug!(%error, "could not update the small call window");
      }
    },
    (true, None) => {
      if let Err(error) = create(app) {
        tracing::warn!(%error, "could not open the small call window");
      }
    },
    (false, Some(window)) => close(app, &window),
    (false, None) => {},
  }
}

/// The main window is not in front: hidden (to the tray), minimized or without the focus.
fn is_main_away(app: &AppHandle) -> bool {
  let Some(window) = app.get_webview_window(MAIN_WINDOW) else {
    return true;
  };
  main_away(
    window.is_visible().unwrap_or(false),
    window.is_minimized().unwrap_or(false),
    window.is_focused().unwrap_or(false),
  )
}

fn main_away(is_visible: bool, is_minimized: bool, is_focused: bool) -> bool {
  !is_visible || is_minimized || !is_focused
}

/// A borderless window above all others, where the user left it last (else in the corner where the
/// incoming-call window appears), hidden until its page is ready, and without taking the focus.
fn create(app: &AppHandle) -> tauri::Result<()> {
  let (x, y) = position(app);
  WebviewWindowBuilder::new(app, CALL_MINI_LABEL, WebviewUrl::default())
    .title("TeamCraft")
    .inner_size(WIDTH, HEIGHT)
    .position(x, y)
    .resizable(false)
    .maximizable(false)
    .minimizable(false)
    .decorations(false)
    .shadow(true)
    .always_on_top(true)
    .visible_on_all_workspaces(true)
    .skip_taskbar(true)
    .focused(false)
    .visible(false)
    .disable_drag_drop_handler()
    .build()?;
  Ok(())
}

/// Where the window opens: where the user left it if that is still on a screen, else the corner.
fn position(app: &AppHandle) -> (f64, f64) {
  let state = app.state::<CallMiniState>();
  let maybe_saved = {
    let mut inner = state.lock();
    if inner.maybe_position.is_none() {
      inner.maybe_position = load_position(app);
    }
    inner.maybe_position
  };
  let screens: Vec<Area> = app
    .available_monitors()
    .unwrap_or_default()
    .iter()
    .map(Area::of)
    .collect();
  match maybe_saved {
    Some(saved) if is_on_a_screen(saved, &screens) => (saved.x, saved.y),
    _ => corner(app),
  }
}

/// A screen's work area in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Area {
  left: f64,
  top: f64,
  width: f64,
  height: f64,
}

impl Area {
  fn of(monitor: &tauri::Monitor) -> Self {
    let scale = monitor.scale_factor();
    let area = monitor.work_area();
    Self {
      left: f64::from(area.position.x) / scale,
      top: f64::from(area.position.y) / scale,
      width: f64::from(area.size.width) / scale,
      height: f64::from(area.size.height) / scale,
    }
  }
}

/// Whether the window's top left and enough of it to grab lie on one of the screens (a screen that
/// was unplugged since doesn't count).
fn is_on_a_screen(position: SavedPosition, screens: &[Area]) -> bool {
  screens.iter().any(|area| {
    position.x >= area.left
      && position.y >= area.top
      && position.x + WIDTH / 2.0 <= area.left + area.width
      && position.y + HEIGHT / 2.0 <= area.top + area.height
  })
}

/// Top right (macOS, Linux) or bottom right (Windows, where toasts appear) of the screen with the
/// main window, within its work area, next to where the incoming-call window appears.
fn corner(app: &AppHandle) -> (f64, f64) {
  let maybe_monitor = app
    .get_webview_window(MAIN_WINDOW)
    .and_then(|window| window.current_monitor().ok().flatten())
    .or_else(|| app.primary_monitor().ok().flatten());
  let Some(monitor) = maybe_monitor else {
    return (MARGIN, MARGIN);
  };
  let area = Area::of(&monitor);
  let x = (area.left + area.width - WIDTH - MARGIN).max(area.left);
  let y = if cfg!(windows) {
    (area.top + area.height - HEIGHT - MARGIN - INCOMING_ROOM).max(area.top)
  } else {
    area.top + MARGIN + INCOMING_ROOM
  };
  (x, y)
}

/// Closes the window and keeps where it was for the next call.
fn close(app: &AppHandle, window: &WebviewWindow) {
  if let Ok(position) = window.outer_position()
    && let Ok(scale) = window.scale_factor()
  {
    let logical: LogicalPosition<f64> = position.to_logical(scale);
    let saved = SavedPosition {
      x: logical.x,
      y: logical.y,
    };
    app.state::<CallMiniState>().lock().maybe_position = Some(saved);
    save_position(app, saved);
  }
  if let Err(error) = window.destroy() {
    tracing::debug!(%error, "could not close the small call window");
  }
}

fn load_position(app: &AppHandle) -> Option<SavedPosition> {
  let text = std::fs::read_to_string(position_path(app)?).ok()?;
  serde_json::from_str(&text).ok()
}

fn save_position(app: &AppHandle, position: SavedPosition) {
  let Some(path) = position_path(app) else {
    return;
  };
  let written = serde_json::to_string(&position)
    .map_err(|error| error.to_string())
    .and_then(|text| {
      if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
      }
      std::fs::write(&path, text).map_err(|error| error.to_string())
    });
  if let Err(error) = written {
    tracing::debug!(%error, "could not keep where the small call window was");
  }
}

fn position_path(app: &AppHandle) -> Option<PathBuf> {
  app
    .path()
    .app_config_dir()
    .ok()
    .map(|dir| dir.join(POSITION_FILE))
}

impl CallMiniState {
  fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
    self
      .inner
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_main_window_is_away_unless_it_is_shown_and_focused() {
    assert!(!main_away(true, false, true));
    assert!(main_away(false, false, false), "hidden to the tray");
    assert!(main_away(true, true, false), "minimized");
    assert!(main_away(true, false, false), "behind other windows");
  }

  #[test]
  fn only_the_call_controls_are_the_windows_buttons() {
    let allowed: Vec<_> = TrayAction::ALL
      .into_iter()
      .filter(|action| is_window_action(*action))
      .collect();
    assert_eq!(
      allowed,
      vec![
        TrayAction::Mute,
        TrayAction::Unmute,
        TrayAction::CameraOn,
        TrayAction::CameraOff,
        TrayAction::LeaveCall,
        TrayAction::OpenCall,
        TrayAction::LeaveCallAsking,
        TrayAction::SoundOff,
        TrayAction::SoundOn,
      ]
    );
  }

  #[test]
  fn a_saved_place_counts_only_on_a_screen_that_is_still_there() {
    let screen = Area {
      left: 0.0,
      top: 0.0,
      width: 1920.0,
      height: 1080.0,
    };
    let right = Area {
      left: 1920.0,
      top: 0.0,
      width: 1280.0,
      height: 1024.0,
    };
    let on_right = SavedPosition { x: 2000.0, y: 100.0 };
    assert!(is_on_a_screen(on_right, &[screen, right]));
    assert!(
      !is_on_a_screen(on_right, &[screen]),
      "the right screen was unplugged"
    );
    assert!(
      !is_on_a_screen(SavedPosition { x: 1900.0, y: 10.0 }, &[screen]),
      "hangs off the edge"
    );
    assert!(!is_on_a_screen(SavedPosition { x: -50.0, y: 10.0 }, &[screen]));
  }
}
