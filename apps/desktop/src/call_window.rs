//! The incoming-call window (#650): a small window above all others, like the in-app card, while a
//! call rings and the app is in the background (notifications.md, "Calls"). The UI decides when it
//! shows and what it says ([`CallWindow`]); the window's page renders only that card and reports
//! its buttons here, which hands them to the main window as notification actions for the call's tag.
//! So answering from the window, the notification or the in-app card is one path.
//!
//! The window's own capability (`capabilities/call-window.json`) allows only the commands it needs.

use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use teamcraft_client_api::call_window::{CALL_WINDOW_LABEL, CallWindow};
use teamcraft_client_api::os_notify::{NotificationAction, NotificationActionEvent, NotificationTag};

/// The window's size in logical pixels: the in-app card's.
const WIDTH: f64 = 400.0;
const HEIGHT: f64 = 80.0;
/// Distance from the screen's edges.
const MARGIN: f64 = 16.0;
/// Longest the window stays: should the main window's page die while a call rings, the window
/// doesn't stay on screen for good. Rings end long before.
const MAX_RING: Duration = Duration::from_secs(120);

/// The call the window shows, if one.
#[derive(Default)]
pub struct CallWindowState {
  inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
  maybe_shown: Option<CallWindow>,
  /// Counts the calls shown, so a timeout closes only the window it was started for.
  generation: u64,
}

/// Shows the window for a ringing call (or shows another call in it).
#[tauri::command(rename_all = "snake_case")]
pub fn call_window_show(
  app: AppHandle,
  state: State<'_, CallWindowState>,
  window: CallWindow,
) -> Result<(), String> {
  let generation = {
    let mut inner = state.lock();
    inner.maybe_shown = Some(window);
    inner.generation += 1;
    inner.generation
  };
  match app.get_webview_window(CALL_WINDOW_LABEL) {
    // The page reads the new call when it loads again.
    Some(shown) => shown.reload().map_err(|error| error.to_string())?,
    None => create(&app).map_err(|error| error.to_string())?,
  }
  close_after_timeout(&app, generation);
  Ok(())
}

/// Closes the window if it shows the call with this tag (the call stopped ringing here, or the
/// user came back to the main window).
#[tauri::command(rename_all = "snake_case")]
pub fn call_window_close(app: AppHandle, state: State<'_, CallWindowState>, tag: NotificationTag) {
  let is_shown = {
    let mut inner = state.lock();
    let is_shown = inner.maybe_shown.as_ref().is_some_and(|shown| shown.tag == tag);
    if is_shown {
      inner.maybe_shown = None;
    }
    is_shown
  };
  if is_shown {
    close(&app);
  }
}

/// The window's page: the call to show.
#[tauri::command(rename_all = "snake_case")]
pub fn call_window_info(state: State<'_, CallWindowState>) -> Option<CallWindow> {
  state.lock().maybe_shown.clone()
}

/// The window's page has rendered: the window appears (it starts hidden, so the loading screen
/// never shows in it).
#[tauri::command(rename_all = "snake_case")]
pub fn call_window_ready(window: WebviewWindow) {
  if window.label() != CALL_WINDOW_LABEL {
    return;
  }
  if let Err(error) = window.show() {
    tracing::warn!(%error, "could not show the incoming-call window");
  }
}

/// A button of the window: closes it and hands the action to the main window, as if it came from
/// the call's notification. Only answering and declining are buttons there.
#[tauri::command(rename_all = "snake_case")]
pub fn call_window_action(app: AppHandle, state: State<'_, CallWindowState>, action: NotificationAction) {
  if !matches!(
    action,
    NotificationAction::AcceptAudio | NotificationAction::Decline
  ) {
    return;
  }
  let Some(shown) = state.lock().maybe_shown.take() else {
    return;
  };
  close(&app);
  crate::notifications::report(
    &app,
    NotificationActionEvent {
      tag: shown.tag,
      action,
      maybe_text: None,
    },
  );
}

impl CallWindowState {
  fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
    self
      .inner
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
  }
}

/// A borderless window above all others, in the corner where the system's notifications appear,
/// hidden until its page is ready.
fn create(app: &AppHandle) -> tauri::Result<()> {
  let (x, y) = corner(app);
  WebviewWindowBuilder::new(app, CALL_WINDOW_LABEL, WebviewUrl::default())
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

/// Top right (macOS, Linux) or bottom right (Windows, where toasts appear) of the screen with the
/// main window, within its work area (not under the menu bar, dock or taskbar).
fn corner(app: &AppHandle) -> (f64, f64) {
  let maybe_monitor = app
    .get_webview_window(crate::tray::MAIN_WINDOW)
    .and_then(|window| window.current_monitor().ok().flatten())
    .or_else(|| app.primary_monitor().ok().flatten());
  let Some(monitor) = maybe_monitor else {
    return (MARGIN, MARGIN);
  };
  let scale = monitor.scale_factor();
  let area = monitor.work_area();
  let left = f64::from(area.position.x) / scale;
  let top = f64::from(area.position.y) / scale;
  let width = f64::from(area.size.width) / scale;
  let height = f64::from(area.size.height) / scale;
  let x = (left + width - WIDTH - MARGIN).max(left);
  let y = if cfg!(windows) {
    (top + height - HEIGHT - MARGIN).max(top)
  } else {
    top + MARGIN
  };
  (x, y)
}

fn close(app: &AppHandle) {
  if let Some(window) = app.get_webview_window(CALL_WINDOW_LABEL)
    && let Err(error) = window.destroy()
  {
    tracing::debug!(%error, "could not close the incoming-call window");
  }
}

fn close_after_timeout(app: &AppHandle, generation: u64) {
  let app = app.clone();
  tauri::async_runtime::spawn(async move {
    tokio::time::sleep(MAX_RING).await;
    let state = app.state::<CallWindowState>();
    let is_same = {
      let mut inner = state.lock();
      let is_same = inner.generation == generation && inner.maybe_shown.is_some();
      if is_same {
        inner.maybe_shown = None;
      }
      is_same
    };
    if is_same {
      close(&app);
    }
  });
}
