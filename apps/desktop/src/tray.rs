//! Tray icon, unread badge and showing the window again. The UI sends the unread count and the
//! menu texts in its language (`tray_update`); the shell owns the OS side.
//!
//! | Platform | Unread badge                     | Tray click                     |
//! |----------|----------------------------------|--------------------------------|
//! | Linux    | launcher badge (Unity API)       | menu only (no click events)    |
//! | macOS    | dock badge                       | left click shows the window    |
//! | Windows  | taskbar overlay dot              | left click shows the window    |

use std::sync::Mutex;

use serde::Deserialize;
#[cfg(windows)]
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, State, Wry};

pub const MAIN_WINDOW: &str = "main";
const TRAY_ID: &str = "main";
const MENU_SHOW: &str = "show";
const MENU_QUIT: &str = "quit";
/// Side of the Windows taskbar overlay dot, in pixels.
#[cfg(windows)]
const DOT_SIZE: u32 = 16;

/// The menu items, kept to change their text when the UI's language changes.
pub struct TrayMenu {
  show: MenuItem<Wry>,
  quit: MenuItem<Wry>,
  last_unread: Mutex<Option<u32>>,
}

/// What the UI tells the tray: unread count and the texts in the user's language.
#[derive(Debug, Deserialize)]
pub struct TrayUpdate {
  pub unread: u32,
  pub tooltip: String,
  pub show_label: String,
  pub quit_label: String,
}

/// Creates the tray icon with "Show" and "Quit". The texts are placeholders until the UI sends them.
pub fn install(app: &tauri::App) -> tauri::Result<()> {
  let name = app.package_info().name.clone();
  let show = MenuItem::with_id(app, MENU_SHOW, &name, true, None::<&str>)?;
  let quit = MenuItem::with_id(app, MENU_QUIT, "×", true, None::<&str>)?;
  let menu = Menu::with_items(app, &[&show, &quit])?;
  let mut builder = TrayIconBuilder::with_id(TRAY_ID)
    .menu(&menu)
    .tooltip(&name)
    .show_menu_on_left_click(false)
    .on_menu_event(|app, event| match event.id().as_ref() {
      MENU_SHOW => show_main_window(app),
      MENU_QUIT => app.exit(0),
      _ => {},
    })
    .on_tray_icon_event(|tray, event| {
      if let TrayIconEvent::Click {
        button: MouseButton::Left,
        button_state: MouseButtonState::Up,
        ..
      } = event
      {
        show_main_window(tray.app_handle());
      }
    });
  if let Some(icon) = app.default_window_icon() {
    builder = builder.icon(icon.clone());
  }
  builder.build(app)?;
  app.manage(TrayMenu {
    show,
    quit,
    last_unread: Mutex::new(None),
  });
  Ok(())
}

/// Shows, un-minimizes and focuses the main window.
pub fn show_main_window(app: &AppHandle) {
  let Some(window) = app.get_webview_window(MAIN_WINDOW) else {
    return;
  };
  if let Err(error) = window
    .show()
    .and_then(|()| window.unminimize())
    .and_then(|()| window.set_focus())
  {
    tracing::debug!(%error, "could not show the window");
  }
}

/// Whether the tray icon exists (closing to the tray only makes sense with one).
pub fn has_tray(app: &AppHandle) -> bool {
  app.tray_by_id(TRAY_ID).is_some()
}

#[tauri::command]
pub fn tray_update(app: AppHandle, menu: State<'_, TrayMenu>, update: TrayUpdate) {
  let results = [
    menu.show.set_text(&update.show_label),
    menu.quit.set_text(&update.quit_label),
    app
      .tray_by_id(TRAY_ID)
      .map_or(Ok(()), |tray: TrayIcon| tray.set_tooltip(Some(&update.tooltip))),
  ];
  for error in results.into_iter().filter_map(Result::err) {
    tracing::debug!(%error, "could not update the tray");
  }
  let mut last = menu
    .last_unread
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner);
  if *last != Some(update.unread) {
    *last = Some(update.unread);
    set_badge(&app, update.unread);
  }
}

fn set_badge(app: &AppHandle, unread: u32) {
  let Some(window) = app.get_webview_window(MAIN_WINDOW) else {
    return;
  };
  #[cfg(not(windows))]
  let result = window.set_badge_count((unread > 0).then_some(i64::from(unread)));
  #[cfg(windows)]
  let result = window.set_overlay_icon((unread > 0).then(unread_dot));
  if let Err(error) = result {
    tracing::debug!(%error, "could not set the unread badge");
  }
}

/// A red dot for the Windows taskbar button (Windows has no numeric badge for desktop apps).
#[cfg(windows)]
fn unread_dot() -> Image<'static> {
  let center = (DOT_SIZE as f32 - 1.0) / 2.0;
  let radius = DOT_SIZE as f32 / 2.0;
  let mut rgba = Vec::with_capacity((DOT_SIZE * DOT_SIZE * 4) as usize);
  for y in 0..DOT_SIZE {
    for x in 0..DOT_SIZE {
      let distance = ((x as f32 - center).powi(2) + (y as f32 - center).powi(2)).sqrt();
      let alpha = ((radius - distance).clamp(0.0, 1.0) * 255.0) as u8;
      rgba.extend_from_slice(&[0xd9, 0x3f, 0x3f, alpha]);
    }
  }
  Image::new_owned(rgba, DOT_SIZE, DOT_SIZE)
}
