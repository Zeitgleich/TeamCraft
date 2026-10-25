//! Tray icon, unread badge and showing the window again. The UI sends the unread count and the
//! menu texts in its language (`tray_update`); the shell owns the OS side.
//!
//! | Platform | Unread badge                     | Tray click                     |
//! |----------|----------------------------------|--------------------------------|
//! | Linux    | launcher badge (Unity API)       | menu only (no click events)    |
//! | macOS    | dock badge                       | left click shows the window    |
//! | Windows  | taskbar overlay dot              | left click shows the window    |
//!
//! Without libayatana-appindicator (Linux) there is no tray icon; the badge still works and the
//! window is never hidden (see `has_tray`). The same holds while no StatusNotifier host shows the
//! icon (GNOME without the AppIndicator extension): the icon exists but nobody can see it.

use std::sync::Mutex;
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicU8, Ordering};

use serde::Deserialize;
#[cfg(windows)]
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, State, Wry};

pub const MAIN_WINDOW: &str = "main";
const TRAY_ID: &str = "main";
const MENU_SHOW: &str = "show";
const MENU_QUIT: &str = "quit";
/// Zoom limits; the UI offers steps between them.
const MIN_ZOOM: f64 = 0.5;
const MAX_ZOOM: f64 = 2.0;
/// Side of the Windows taskbar overlay dot, in pixels.
#[cfg(windows)]
const DOT_SIZE: u32 = 16;

/// The texts of the tray menu.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MenuLabels {
  show: String,
  quit: String,
}

/// Language of the built-in default menu texts, until the UI sends its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuLanguage {
  English,
  German,
}

/// The menu state, kept to change the texts when the UI's language changes. Only managed while the
/// tray icon exists.
pub struct TrayMenu {
  labels: Mutex<MenuLabels>,
  /// Appindicator menus do not reflect `set_text`; there the menu is rebuilt instead.
  #[cfg(not(target_os = "linux"))]
  show: MenuItem<Wry>,
  #[cfg(not(target_os = "linux"))]
  quit: MenuItem<Wry>,
}

/// Linux: whether a StatusNotifier host shows the tray icon ([`HostState`] as `u8`). Managed while
/// the tray icon exists; asked in the background at start, then follows the D-Bus watcher, since a
/// panel may start after the app (autostart at login).
#[cfg(target_os = "linux")]
pub struct TrayHost(AtomicU8);

/// What is known about the StatusNotifier host.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum HostState {
  /// The D-Bus answer is still on its way (the first milliseconds after start).
  Unknown = 0,
  Present = 1,
  Absent = 2,
}

/// The unread count last shown on the badge; managed with or without a tray icon.
#[derive(Default)]
pub struct LastUnread(Mutex<Option<u32>>);

/// What the UI tells the tray: unread count and the texts in the user's language.
#[derive(Debug, Deserialize)]
pub struct TrayUpdate {
  /// `None` leaves the badge as it is (the UI sends the texts before it knows any count).
  pub maybe_unread: Option<u32>,
  pub tooltip: String,
  pub show_label: String,
  pub quit_label: String,
}

/// Creates the tray icon with "Show" and "Quit". The texts are in the OS language until the UI
/// sends its own.
pub fn install(app: &tauri::App) -> tauri::Result<()> {
  app.manage(LastUnread::default());
  // The tray crate panics (release builds abort) when the library is missing.
  #[cfg(target_os = "linux")]
  if !crate::appindicator::is_available() {
    tracing::warn!("libayatana-appindicator3 not found; running without a tray icon");
    return Ok(());
  }
  let name = app.package_info().name.clone();
  let labels = default_labels(system_language());
  let (menu, _show, _quit) = build_menu(app, &labels)?;
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
  // The app icon is too detailed at tray size; the glyph (icons/tray*.png, scripts/brand) is not.
  // macOS draws the menu bar icon as a template in the bar's own colour.
  #[cfg(target_os = "macos")]
  {
    builder = builder
      .icon(tauri::include_image!("icons/tray-template.png"))
      .icon_as_template(true);
  }
  #[cfg(not(target_os = "macos"))]
  {
    builder = builder.icon(tauri::include_image!("icons/tray.png"));
  }
  builder.build(app)?;
  #[cfg(target_os = "linux")]
  watch_tray_host(app);
  app.manage(TrayMenu {
    labels: Mutex::new(labels),
    #[cfg(not(target_os = "linux"))]
    show: _show,
    #[cfg(not(target_os = "linux"))]
    quit: _quit,
  });
  Ok(())
}

/// Asks the session bus in the background, so the start never waits for D-Bus, then follows the
/// watcher. The window may already exist (hidden, when it starts minimized) by the time the answer
/// arrives: without a host it is brought back minimized, see [`reveal_without_host`].
#[cfg(target_os = "linux")]
fn watch_tray_host(app: &tauri::App) {
  let buses = app.state::<crate::attention::OsProbe>().buses();
  app.manage(TrayHost(AtomicU8::new(HostState::Unknown as u8)));
  let handle = app.handle().clone();
  tauri::async_runtime::spawn(async move {
    let is_present = buses.has_status_notifier_host().await;
    if !is_present {
      tracing::info!("no StatusNotifier host; the window is not hidden to the tray");
    }
    set_host(&handle, is_present);
    buses
      .watch_status_notifier_host(move |is_present| {
        if set_host(&handle, is_present) {
          tracing::info!(is_present, "StatusNotifier host changed");
        }
      })
      .await;
  });
}

/// Records the host; returns whether that changed what was known.
#[cfg(target_os = "linux")]
fn set_host(app: &AppHandle, is_present: bool) -> bool {
  let state = match is_present {
    true => HostState::Present,
    false => HostState::Absent,
  };
  let previous = app.state::<TrayHost>().0.swap(state as u8, Ordering::SeqCst);
  if state == HostState::Absent {
    reveal_without_host(app);
  }
  previous != state as u8
}

/// A window that started hidden while the host was unknown can't come back through an icon nobody
/// sees: show it minimized instead. Called after the answer arrives and after the window is
/// created, so whichever comes second does it.
#[cfg(target_os = "linux")]
pub fn reveal_without_host(app: &AppHandle) {
  let is_absent = app
    .try_state::<TrayHost>()
    .is_some_and(|host| host.0.load(Ordering::SeqCst) == HostState::Absent as u8);
  let Some(window) = app.get_webview_window(MAIN_WINDOW) else {
    return;
  };
  if is_absent
    && !window.is_visible().unwrap_or(true)
    && let Err(error) = window.show().and_then(|()| window.minimize())
  {
    tracing::debug!(%error, "could not bring back the hidden window");
  }
}

/// macOS and Windows always show the icon.
#[cfg(not(target_os = "linux"))]
pub fn reveal_without_host(_app: &AppHandle) {}

fn build_menu(
  manager: &impl Manager<Wry>,
  labels: &MenuLabels,
) -> tauri::Result<(Menu<Wry>, MenuItem<Wry>, MenuItem<Wry>)> {
  let show = MenuItem::with_id(manager, MENU_SHOW, &labels.show, true, None::<&str>)?;
  let quit = MenuItem::with_id(manager, MENU_QUIT, &labels.quit, true, None::<&str>)?;
  let menu = Menu::with_items(manager, &[&show, &quit])?;
  Ok((menu, show, quit))
}

fn default_labels(language: MenuLanguage) -> MenuLabels {
  let (show, quit) = match language {
    MenuLanguage::English => ("Open TeamCraft", "Quit"),
    MenuLanguage::German => ("TeamCraft öffnen", "Beenden"),
  };
  MenuLabels {
    show: show.to_owned(),
    quit: quit.to_owned(),
  }
}

/// The OS language from the POSIX locale variables (the first one that is set wins, as in libc).
fn system_language() -> MenuLanguage {
  ["LC_ALL", "LC_MESSAGES", "LANG"]
    .iter()
    .filter_map(|name| std::env::var(name).ok())
    .find(|value| !value.is_empty())
    .map_or(MenuLanguage::English, |value| parse_language(&value))
}

/// `de_DE.UTF-8`, `de` and `de_AT@euro` are German; everything else (`C`, `en_US`, empty) English.
fn parse_language(locale: &str) -> MenuLanguage {
  let language = locale.split(['_', '.', '@', '-']).next().unwrap_or_default();
  if language.eq_ignore_ascii_case("de") {
    MenuLanguage::German
  } else {
    MenuLanguage::English
  }
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

/// Zooms the main window's webview (1.0 = normal); the UI keeps the user's choice.
#[tauri::command(rename_all = "snake_case")]
pub fn set_zoom(app: AppHandle, factor: f64) {
  let factor = factor.clamp(MIN_ZOOM, MAX_ZOOM);
  if let Some(window) = app.get_webview_window(MAIN_WINDOW)
    && let Err(error) = window.set_zoom(factor)
  {
    tracing::debug!(%error, "could not zoom");
  }
}

/// Whether the tray icon exists and can be seen (closing to the tray only makes sense then).
pub fn has_tray(app: &AppHandle) -> bool {
  app.tray_by_id(TRAY_ID).is_some() && host_state(app) == Some(true)
}

/// Like [`has_tray`], but counting a host that is still being asked about: the window may start
/// hidden then, and [`reveal_without_host`] brings it back if there turns out to be none.
pub fn may_have_tray(app: &AppHandle) -> bool {
  app.tray_by_id(TRAY_ID).is_some() && host_state(app) != Some(false)
}

/// `Some(true)` when the icon is shown, `None` while that isn't known yet.
#[cfg(target_os = "linux")]
fn host_state(app: &AppHandle) -> Option<bool> {
  let state = app.try_state::<TrayHost>()?.0.load(Ordering::SeqCst);
  match state {
    state if state == HostState::Present as u8 => Some(true),
    state if state == HostState::Absent as u8 => Some(false),
    _ => None,
  }
}

/// macOS and Windows always show the icon.
#[cfg(not(target_os = "linux"))]
fn host_state(_app: &AppHandle) -> Option<bool> {
  Some(true)
}

#[tauri::command(rename_all = "snake_case")]
pub fn tray_update(app: AppHandle, last_unread: State<'_, LastUnread>, update: TrayUpdate) {
  if let Some(menu) = app.try_state::<TrayMenu>() {
    update_tray(&app, &menu, &update);
  }
  let mut last = last_unread
    .0
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner);
  if let Some(unread) = update.maybe_unread
    && *last != Some(unread)
  {
    *last = Some(unread);
    set_badge(&app, unread);
  }
}

fn update_tray(app: &AppHandle, menu: &TrayMenu, update: &TrayUpdate) {
  let labels = MenuLabels {
    show: update.show_label.clone(),
    quit: update.quit_label.clone(),
  };
  let mut current = menu
    .labels
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner);
  let mut results = Vec::new();
  if *current != labels {
    results.push(apply_labels(app, menu, &labels));
    *current = labels;
  }
  if let Some(tray) = app.tray_by_id(TRAY_ID) {
    results.push(tray.set_tooltip(Some(&update.tooltip)));
  }
  for error in results.into_iter().filter_map(Result::err) {
    tracing::debug!(%error, "could not update the tray");
  }
}

/// Linux (StatusNotifier) shows a menu as it was when it was set, so it is replaced.
#[cfg(target_os = "linux")]
fn apply_labels(app: &AppHandle, _menu: &TrayMenu, labels: &MenuLabels) -> tauri::Result<()> {
  let (new_menu, _, _) = build_menu(app, labels)?;
  match app.tray_by_id(TRAY_ID) {
    Some(tray) => tray.set_menu(Some(new_menu)),
    None => Ok(()),
  }
}

#[cfg(not(target_os = "linux"))]
fn apply_labels(_app: &AppHandle, menu: &TrayMenu, labels: &MenuLabels) -> tauri::Result<()> {
  menu.show.set_text(&labels.show)?;
  menu.quit.set_text(&labels.quit)
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parses_posix_locales() {
    assert_eq!(parse_language("de_DE.UTF-8"), MenuLanguage::German);
    assert_eq!(parse_language("de"), MenuLanguage::German);
    assert_eq!(parse_language("de_AT@euro"), MenuLanguage::German);
    assert_eq!(parse_language("de-DE"), MenuLanguage::German);
    assert_eq!(parse_language("en_US"), MenuLanguage::English);
    assert_eq!(parse_language("en_US.UTF-8"), MenuLanguage::English);
    assert_eq!(parse_language("C"), MenuLanguage::English);
    assert_eq!(parse_language("POSIX"), MenuLanguage::English);
    assert_eq!(parse_language(""), MenuLanguage::English);
  }

  #[test]
  fn defaults_are_real_texts() {
    assert_eq!(default_labels(MenuLanguage::German).quit, "Beenden");
    assert_eq!(default_labels(MenuLanguage::English).quit, "Quit");
  }
}
