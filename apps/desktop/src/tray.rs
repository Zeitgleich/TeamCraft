//! Tray icon, unread badge and showing the window again. The UI sends the unread count and the
//! menu texts in its language (`tray_update`); the shell owns the OS side.
//!
//! | Platform | Unread badge                                     | Tray click                  |
//! |----------|--------------------------------------------------|-----------------------------|
//! | Linux    | launcher badge (LauncherEntry signal), tray dot  | menu only (no click events) |
//! | macOS    | dock badge, tray dot                             | left click shows the window |
//! | Windows  | taskbar overlay dot, tray dot                    | left click shows the window |
//!
//! The tray dot is a red dot drawn into the tray icon's top right while anything is unread (in the
//! menu bar's own colour on macOS, where the icon is a template).
//!
//! Without libayatana-appindicator (Linux) there is no tray icon; the badge still works and the
//! window is never hidden (see `has_tray`). The same holds while no StatusNotifier host shows the
//! icon (GNOME without the AppIndicator extension): the icon exists but nobody can see it.

use std::sync::Mutex;
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicU8, Ordering};

use serde::Deserialize;
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
/// The unread dot's colour.
const DOT_COLOR: [u8; 3] = [0xd9, 0x3f, 0x3f];
/// The tray dot's diameter as a share of the tray icon's side.
const TRAY_DOT_SHARE: f32 = 0.5;
/// The transparent ring around the tray dot that sets it off the glyph, as a share of its radius.
const TRAY_DOT_GAP_SHARE: f32 = 0.3;

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
pub struct LastUnread {
  count: Mutex<Option<u32>>,
  /// Linux: held while a launcher update is sent, so updates go out in order (each sends the
  /// latest count).
  #[cfg(target_os = "linux")]
  launcher_sending: tokio::sync::Mutex<()>,
}

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
  builder = builder.icon(tray_icon(false));
  // macOS draws the menu bar icon as a template in the bar's own colour.
  #[cfg(target_os = "macos")]
  {
    builder = builder.icon_as_template(true);
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
    .count
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner);
  if let Some(unread) = update.maybe_unread
    && *last != Some(unread)
  {
    let had_unread = last.is_some_and(|count| count > 0);
    *last = Some(unread);
    drop(last);
    set_badge(&app, unread);
    if had_unread != (unread > 0)
      && let Some(tray) = app.tray_by_id(TRAY_ID)
    {
      if let Err(error) = tray.set_icon(Some(tray_icon(unread > 0))) {
        tracing::debug!(%error, "could not change the tray icon");
      }
      // A new image drops the template flag, and macOS then draws it black in a dark menu bar (#632).
      #[cfg(target_os = "macos")]
      if let Err(error) = tray.set_icon_as_template(true) {
        tracing::debug!(%error, "could not keep the tray icon a template");
      }
    }
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

#[cfg(not(target_os = "linux"))]
fn set_badge(app: &AppHandle, unread: u32) {
  // The notification center owns the badge from macOS 13 on; the window's dock tile label below is
  // kept for an unbundled development run.
  #[cfg(target_os = "macos")]
  if crate::notifications::mac_center::is_available() {
    crate::notifications::mac_center::set_badge(unread);
  }
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

/// Linux: the LauncherEntry signal for the installed `.desktop` entry (or the one the packages
/// would install, in a development build). Sent in the background; the latest count wins.
#[cfg(target_os = "linux")]
fn set_badge(app: &AppHandle, _unread: u32) {
  let app = app.clone();
  tauri::async_runtime::spawn(async move {
    let desktop_entry = match crate::desktop_entry::installed(&app) {
      Some(entry) => format!("{entry}.desktop"),
      None => format!("{}.desktop", app.package_info().name),
    };
    let buses = app.state::<crate::attention::OsProbe>().buses();
    let last = app.state::<LastUnread>();
    let _sending = last.launcher_sending.lock().await;
    let unread = last
      .count
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .unwrap_or(0);
    buses.set_launcher_count(&desktop_entry, unread).await;
  });
}

/// The tray icon, with the unread dot while anything is unread. The app icon is too detailed at
/// tray size; the glyph (icons/tray*.png, scripts/brand) is not.
fn tray_icon(has_unread: bool) -> Image<'static> {
  #[cfg(target_os = "macos")]
  let icon = tauri::include_image!("icons/tray-template.png");
  #[cfg(not(target_os = "macos"))]
  let icon = tauri::include_image!("icons/tray.png");
  if has_unread { with_unread_dot(&icon) } else { icon }
}

/// `icon` with the unread dot in its top right, set off the glyph by a transparent ring.
fn with_unread_dot(icon: &Image<'_>) -> Image<'static> {
  let (width, height) = (icon.width(), icon.height());
  let radius = width.min(height) as f32 * TRAY_DOT_SHARE / 2.0;
  let dot = Dot {
    center: (width as f32 - radius, radius),
    radius,
    gap: radius * TRAY_DOT_GAP_SHARE,
  };
  let mut rgba = icon.rgba().to_vec();
  dot.paint(&mut rgba, width);
  Image::new_owned(rgba, width, height)
}

/// A red dot for the Windows taskbar button (Windows has no numeric badge for desktop apps).
#[cfg(windows)]
fn unread_dot() -> Image<'static> {
  let radius = DOT_SIZE as f32 / 2.0;
  let dot = Dot {
    center: (radius, radius),
    radius,
    gap: 0.0,
  };
  let mut rgba = vec![0; (DOT_SIZE * DOT_SIZE * 4) as usize];
  dot.paint(&mut rgba, DOT_SIZE);
  Image::new_owned(rgba, DOT_SIZE, DOT_SIZE)
}

/// An anti-aliased dot in [`DOT_COLOR`], in pixel coordinates (pixel centres at `.5`).
struct Dot {
  center: (f32, f32),
  radius: f32,
  /// Width of the ring around the dot that is cleared to transparent.
  gap: f32,
}

impl Dot {
  /// Draws the dot over RGBA pixels `width` wide: the ring is cleared, the dot blended on top.
  fn paint(&self, rgba: &mut [u8], width: u32) {
    for (index, pixel) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
      let x = (index % width as usize) as f32 + 0.5;
      let y = (index / width as usize) as f32 + 0.5;
      let distance = ((x - self.center.0).powi(2) + (y - self.center.1).powi(2)).sqrt();
      let cleared = (self.radius + self.gap - distance).clamp(0.0, 1.0);
      let covered = (self.radius - distance).clamp(0.0, 1.0);
      if cleared <= 0.0 {
        continue;
      }
      let below = f32::from(pixel[3]) / 255.0 * (1.0 - cleared);
      let alpha = covered + below * (1.0 - covered);
      if alpha <= 0.0 {
        pixel.copy_from_slice(&[0, 0, 0, 0]);
        continue;
      }
      for (channel, dot) in pixel.iter_mut().zip(DOT_COLOR) {
        let mixed = (f32::from(dot) * covered + f32::from(*channel) * below * (1.0 - covered)) / alpha;
        *channel = mixed.round() as u8;
      }
      pixel[3] = (alpha * 255.0).round() as u8;
    }
  }
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
  fn the_unread_icon_has_a_red_dot_top_right_and_keeps_the_rest() {
    let plain = tray_icon(false);
    let unread = tray_icon(true);
    assert_eq!((unread.width(), unread.height()), (plain.width(), plain.height()));
    let width = plain.width() as usize;
    let pixel = |image: &Image<'_>, x: usize, y: usize| {
      let start = (y * width + x) * 4;
      image.rgba()[start..start + 4].to_vec()
    };
    let radius = (plain.width() as f32 * TRAY_DOT_SHARE / 2.0) as usize;
    assert_eq!(
      pixel(&unread, width - radius, radius),
      [DOT_COLOR[0], DOT_COLOR[1], DOT_COLOR[2], 255]
    );
    let bottom_left = plain.height() as usize - 1;
    assert_eq!(pixel(&unread, 0, bottom_left), pixel(&plain, 0, bottom_left));
    assert_eq!(
      pixel(&unread, width / 4, width / 2),
      pixel(&plain, width / 4, width / 2)
    );
  }

  #[test]
  fn the_ring_around_the_dot_is_transparent() {
    let mut rgba = [255_u8; 9 * 9 * 4].to_vec();
    let dot = Dot {
      center: (4.5, 4.5),
      radius: 2.0,
      gap: 2.0,
    };
    dot.paint(&mut rgba, 9);
    let alpha = |x: usize, y: usize| rgba[(y * 9 + x) * 4 + 3];
    assert_eq!(alpha(4, 4), 255, "dot");
    assert_eq!(&rgba[(4 * 9 + 4) * 4..(4 * 9 + 4) * 4 + 3], &DOT_COLOR);
    assert_eq!(alpha(4, 1), 0, "ring");
    assert_eq!(alpha(0, 0), 255, "outside");
  }

  #[test]
  fn defaults_are_real_texts() {
    assert_eq!(default_labels(MenuLanguage::German).quit, "Beenden");
    assert_eq!(default_labels(MenuLanguage::English).quit, "Quit");
  }
}
