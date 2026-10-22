//! Windows shell integration: `teamcraft://` links, a single running instance and the "start at
//! login" option.
//!
//! The installers register the `teamcraft` scheme from `tauri.windows.conf.json`; Windows then
//! starts the app with the link as an argument. A second launch hands its arguments to the running
//! instance (single-instance plugin, which forwards links to the deep-link plugin) and exits.
//! Links are forwarded to the webview as the [`DEEP_LINK_EVENT`] event; the UI decides what to do
//! with them. "Start at login" is the autostart plugin's registry entry, toggled from the UI through
//! the plugin's JS commands (permissions in `capabilities/windows.json`).

use tauri::{AppHandle, Emitter, Manager, Wry};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_deep_link::DeepLinkExt;
use tracing::{debug, warn};

/// Event with the link (a string) that opened or re-opened the app.
pub const DEEP_LINK_EVENT: &str = "teamcraft://deep-link";

/// Adds the Windows plugins. The single-instance plugin must be registered first.
pub fn plugins(builder: tauri::Builder<Wry>) -> tauri::Builder<Wry> {
  builder
    .plugin(tauri_plugin_single_instance::init(
      |app, args, _working_directory| {
        // The deep-link plugin has already consumed any link in `args` (the `deep-link` feature).
        debug!(
          argument_count = args.len(),
          "second instance started; focusing the first"
        );
        focus_main_window(app);
      },
    ))
    .plugin(tauri_plugin_deep_link::init())
    .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
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
    focus_main_window(&emitter);
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

fn focus_main_window(app: &AppHandle) {
  let Some(window) = app.get_webview_window("main") else {
    return;
  };
  if let Err(error) = window.unminimize().and_then(|()| window.set_focus()) {
    debug!(%error, "could not bring the window to the front");
  }
}
