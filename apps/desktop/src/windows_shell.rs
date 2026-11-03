//! Windows shell integration: the "start at login" option, as the autostart plugin's registry entry,
//! toggled through the shell's own `autostart_get`/`autostart_set` commands (`autostart.rs`); the
//! plugin's commands are not exposed. Links are `deep_links.rs`, as on the other systems.

use tauri::Wry;
use tauri_plugin_autostart::MacosLauncher;

/// Adds the Windows plugins, after the single-instance plugin.
pub fn plugins(builder: tauri::Builder<Wry>) -> tauri::Builder<Wry> {
  builder.plugin(tauri_plugin_autostart::init(
    MacosLauncher::LaunchAgent,
    Some(vec![crate::autostart::AUTOSTART_ARG]),
  ))
}
