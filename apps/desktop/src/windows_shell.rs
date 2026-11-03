//! Windows shell integration: `teamcraft://` links and the "start at login" option.
//!
//! The installers register the `teamcraft` scheme from `tauri.windows.conf.json`; Windows then
//! starts the app with the link as an argument. A second launch hands its arguments to the running
//! instance (`single_instance.rs`, whose plugin forwards links to the deep-link plugin) and exits.
//! Links are forwarded to the webview as the [`DEEP_LINK_EVENT`] event; the UI decides what to do
//! with them. "Start at login" is the autostart plugin's registry entry, toggled through the shell's own
//! `autostart_get`/`autostart_set` commands (`autostart.rs`); the plugin's commands are not exposed.

use tauri::{AppHandle, Emitter, Wry};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_deep_link::DeepLinkExt;
use tracing::{debug, warn};

use crate::tray;

/// Event with the link (a string) that opened or re-opened the app; the UI listens for it.
pub use teamcraft_client_api::remote::DEEP_LINK_EVENT;

/// Adds the Windows plugins, after the single-instance plugin.
pub fn plugins(builder: tauri::Builder<Wry>) -> tauri::Builder<Wry> {
  builder
    .plugin(tauri_plugin_deep_link::init())
    .plugin(tauri_plugin_autostart::init(
      MacosLauncher::LaunchAgent,
      Some(vec![crate::autostart::AUTOSTART_ARG]),
    ))
}

/// Forwards links to the webview, including the one the app was started with.
pub fn forward_deep_links(app: &AppHandle) {
  let deep_links = app.deep_link();
  match deep_links.get_current() {
    Ok(Some(urls)) => emit_links(app, urls.iter().map(url::Url::as_str)),
    Ok(None) => {},
    Err(error) => warn!(%error, "could not read the link the app was started with"),
  }
  let emitter = app.clone();
  deep_links.on_open_url(move |event| {
    emit_links(&emitter, event.urls().iter().map(url::Url::as_str));
    tray::show_main_window(&emitter);
  });
}

fn emit_links<'a>(app: &AppHandle, links: impl Iterator<Item = &'a str>) {
  for link in links {
    // Links are never logged: they can carry key material (device linking).
    if let Err(error) = app.emit(DEEP_LINK_EVENT, link) {
      debug!(%error, "could not forward a link to the webview");
    }
  }
}
