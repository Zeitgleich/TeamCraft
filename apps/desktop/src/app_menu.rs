//! The macOS menu bar (#712): the same menus as Tauri's default (app, File, Edit, View, Window,
//! Help), built here so their texts follow the UI's language. Tauri's default menu sets English
//! titles on every item, which AppKit then shows as they are, also on a German Mac. The UI sends
//! the texts with the tray's (`tray_update`); until then the menu has muda's English defaults.
//! macOS only: Windows and Linux windows have no menu bar.

use std::sync::Mutex;

use serde::Deserialize;
use tauri::menu::{AboutMetadata, HELP_SUBMENU_ID, Menu, PredefinedMenuItem, Submenu, WINDOW_SUBMENU_ID};
use tauri::{AppHandle, Manager, Runtime};

/// The menu texts in the user's language, as the UI sends them (i18n keys `menubar-*`).
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct AppMenuLabels {
  pub about: String,
  pub services: String,
  pub hide: String,
  pub hide_others: String,
  pub show_all: String,
  pub quit: String,
  pub file: String,
  pub close_window: String,
  pub edit: String,
  pub undo: String,
  pub redo: String,
  pub cut: String,
  pub copy: String,
  pub paste: String,
  pub select_all: String,
  pub view: String,
  pub fullscreen: String,
  pub window: String,
  pub minimize: String,
  pub zoom: String,
  pub help: String,
}

/// The texts the menu was last built with, so an unchanged update doesn't rebuild it.
#[derive(Default)]
pub struct AppMenuState(Mutex<Option<AppMenuLabels>>);

/// The menu for `Builder::menu`: muda's English texts until the UI sends its own.
pub fn initial<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
  build(app, None)
}

/// Rebuilds the menu bar in the UI's language when its texts changed.
pub fn update<R: Runtime>(app: &AppHandle<R>, labels: &AppMenuLabels) {
  if app.try_state::<AppMenuState>().is_none() {
    app.manage(AppMenuState::default());
  }
  let state = app.state::<AppMenuState>();
  let mut current = state.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
  if current.as_ref() == Some(labels) {
    return;
  }
  match build(app, Some(labels)).and_then(|menu| app.set_menu(menu)) {
    Ok(_) => *current = Some(labels.clone()),
    Err(error) => tracing::debug!(%error, "could not change the menu bar"),
  }
}

/// Tauri's default macOS menu (`Menu::default`) with `maybe_labels` as the texts.
fn build<R: Runtime>(app: &AppHandle<R>, maybe_labels: Option<&AppMenuLabels>) -> tauri::Result<Menu<R>> {
  let text = |pick: fn(&AppMenuLabels) -> &String| maybe_labels.map(|labels| pick(labels).as_str());
  let title = |pick: fn(&AppMenuLabels) -> &String, english: &'static str| {
    maybe_labels
      .map_or(english, |labels| pick(labels).as_str())
      .to_owned()
  };
  let package = app.package_info();
  let config = app.config();
  let about = AboutMetadata {
    name: Some(package.name.clone()),
    version: Some(package.version.to_string()),
    copyright: config.bundle.copyright.clone(),
    authors: config.bundle.publisher.clone().map(|publisher| vec![publisher]),
    ..Default::default()
  };
  let separator = || PredefinedMenuItem::separator(app);
  // AppKit titles the app menu after the bundle; the title here is not shown.
  let app_menu = Submenu::with_items(
    app,
    package.name.clone(),
    true,
    &[
      &PredefinedMenuItem::about(app, text(|l| &l.about), Some(about))?,
      &separator()?,
      &PredefinedMenuItem::services(app, text(|l| &l.services))?,
      &separator()?,
      &PredefinedMenuItem::hide(app, text(|l| &l.hide))?,
      &PredefinedMenuItem::hide_others(app, text(|l| &l.hide_others))?,
      &PredefinedMenuItem::show_all(app, text(|l| &l.show_all))?,
      &separator()?,
      &PredefinedMenuItem::quit(app, text(|l| &l.quit))?,
    ],
  )?;
  let file = Submenu::with_items(
    app,
    title(|l| &l.file, "File"),
    true,
    &[&PredefinedMenuItem::close_window(app, text(|l| &l.close_window))?],
  )?;
  let edit = Submenu::with_items(
    app,
    title(|l| &l.edit, "Edit"),
    true,
    &[
      &PredefinedMenuItem::undo(app, text(|l| &l.undo))?,
      &PredefinedMenuItem::redo(app, text(|l| &l.redo))?,
      &separator()?,
      &PredefinedMenuItem::cut(app, text(|l| &l.cut))?,
      &PredefinedMenuItem::copy(app, text(|l| &l.copy))?,
      &PredefinedMenuItem::paste(app, text(|l| &l.paste))?,
      &PredefinedMenuItem::select_all(app, text(|l| &l.select_all))?,
    ],
  )?;
  let view = Submenu::with_items(
    app,
    title(|l| &l.view, "View"),
    true,
    &[&PredefinedMenuItem::fullscreen(app, text(|l| &l.fullscreen))?],
  )?;
  let window = Submenu::with_id_and_items(
    app,
    WINDOW_SUBMENU_ID,
    title(|l| &l.window, "Window"),
    true,
    &[
      &PredefinedMenuItem::minimize(app, text(|l| &l.minimize))?,
      &PredefinedMenuItem::maximize(app, text(|l| &l.zoom))?,
      &separator()?,
      &PredefinedMenuItem::close_window(app, text(|l| &l.close_window))?,
    ],
  )?;
  let help = Submenu::with_id_and_items(app, HELP_SUBMENU_ID, title(|l| &l.help, "Help"), true, &[])?;
  Menu::with_items(app, &[&app_menu, &file, &edit, &view, &window, &help])
}
