//! Desktop shell. It hosts the webview and owns sign-in: the OIDC flow, the keyring and the
//! authenticated API calls live here, so the UI never holds a token. The UI itself is
//! `teamcraft-ui`; client logic will move into `teamcraft-core`.

// Hides the console window on Windows release builds; harmless on macOS and Linux.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod attention;
mod auth;
mod autostart;
mod calls;
mod core;
#[cfg(target_os = "linux")]
mod freedesktop;
mod notifications;
mod shell_prefs;
mod tray;
mod updates;
#[cfg(windows)]
mod windows_shell;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tauri::path::BaseDirectory;
use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_window_state::StateFlags;
use tracing_subscriber::EnvFilter;

use crate::auth::{AuthService, DesktopConfig, KeyringStore};
use crate::calls::{BUNDLED_MODEL, CALL_CHANGED_EVENT, CallHost, MediaFiles};
use crate::core::TokenCache;
use crate::core::commands::CoreHost;
use crate::shell_prefs::ShellPrefsState;
use crate::updates::UpdateHost;

/// How often the core's token cache is refreshed from the login session.
const TOKEN_REFRESH: Duration = Duration::from_secs(2);

fn main() -> anyhow::Result<()> {
  tracing_subscriber::fmt()
    .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
    .init();

  let config = DesktopConfig::from_env();
  let store = KeyringStore::new(&config.issuer);
  let service = Arc::new(AuthService::new(config.clone(), store).context("could not set up sign-in")?);
  let tokens = TokenCache::default();
  keep_tokens_fresh(service.clone(), tokens.clone());

  let builder = tauri::Builder::default();
  // Registered first: the single-instance plugin must see a second launch before anything else.
  #[cfg(windows)]
  let builder = windows_shell::plugins(builder);
  builder
    .plugin(tauri_plugin_opener::init())
    .plugin(tauri_plugin_global_shortcut::Builder::new().build())
    .plugin(tauri_plugin_notification::init())
    // The shell drives it (updates.rs); the webview gets no updater permission.
    .plugin(tauri_plugin_updater::Builder::new().build())
    // Size, position and maximized state; visibility is ours (start minimized to the tray).
    .plugin(
      tauri_plugin_window_state::Builder::default()
        .with_state_flags(StateFlags::all() - StateFlags::VISIBLE)
        .build(),
    )
    .manage(service)
    .manage(attention::OsProbe::default())
    .manage(notifications::ShownNotifications::default())
    .manage(calls::push_to_talk::PushToTalkState::default())
    .manage(CoreHost::new(config, tokens))
    .setup(|app| {
      let emitter = app.handle().clone();
      let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        if let Err(error) = emitter.emit(CALL_CHANGED_EVENT, ()) {
          tracing::debug!(%error, "could not notify the webview");
        }
      });
      let files = MediaFiles {
        codec_directory: app.path().app_data_dir()?.join("codecs"),
        maybe_segmentation_model: app.path().resolve(BUNDLED_MODEL, BaseDirectory::Resource).ok(),
      };
      app.manage(Arc::new(CallHost::new(notify, files)));
      app.manage(ShellPrefsState::load(app.handle()));
      app.manage(Arc::new(UpdateHost::load(app.handle())));
      UpdateHost::spawn(app.handle().clone());
      // Without a tray (some Linux desktops) the window must never be hidden.
      if let Err(error) = tray::install(app) {
        tracing::warn!(%error, "no tray icon");
      }
      create_main_window(app)?;
      #[cfg(target_os = "linux")]
      notifications::watch_clicks(app.handle());
      #[cfg(windows)]
      windows_shell::forward_deep_links(app.handle());
      Ok(())
    })
    .on_window_event(|window, event| {
      // Closing keeps the app running in the tray (notifications keep coming) when the user wants it.
      if let WindowEvent::CloseRequested { api, .. } = event
        && window.label() == tray::MAIN_WINDOW
        && window.state::<ShellPrefsState>().get().is_close_to_tray
        && tray::has_tray(window.app_handle())
      {
        api.prevent_close();
        if let Err(error) = window.hide() {
          tracing::debug!(%error, "could not hide the window");
        }
      }
    })
    .invoke_handler(tauri::generate_handler![
      auth::commands::auth_restore,
      auth::commands::auth_login,
      auth::commands::auth_cancel,
      auth::commands::auth_logout,
      auth::commands::api_get,
      auth::commands::api_request,
      auth::commands::public_get,
      core::commands::core_start,
      core::commands::core_status,
      core::commands::core_enroll,
      core::commands::core_snapshot,
      core::commands::core_link_start,
      core::commands::core_link_status,
      core::commands::core_link_cancel,
      core::commands::core_approval_start,
      core::commands::core_approval_confirm,
      core::commands::core_approval_cancel,
      core::commands::core_messages,
      core::commands::core_thread,
      core::commands::core_action,
      core::commands::core_query,
      core::commands::core_stop,
      calls::commands::call_snapshot,
      calls::commands::call_media_devices,
      calls::commands::call_action,
      calls::commands::call_watch_video,
      calls::commands::call_background_preview,
      calls::commands::call_unwatch_video,
      notifications::notify_user,
      notifications::withdraw_notifications,
      attention::os_attention,
      autostart::autostart_get,
      autostart::autostart_set,
      shell_prefs::shell_prefs_get,
      shell_prefs::shell_prefs_set,
      tray::tray_update,
      tray::set_zoom,
      updates::update_status,
      updates::update_check,
      updates::update_set_preference,
      updates::update_download,
      updates::update_restart,
      updates::update_with_package_manager,
    ])
    .build(tauri::generate_context!())
    .context("failed to build the TeamCraft desktop shell")?
    .run(|app, event| match event {
      // A downloaded update is installed when the app quits (docs/updates.md §4.3).
      tauri::RunEvent::Exit => app.state::<Arc<UpdateHost>>().install_on_exit(app),
      // macOS: clicking the dock icon brings back a window that was closed to the tray.
      #[cfg(target_os = "macos")]
      tauri::RunEvent::Reopen { .. } => tray::show_main_window(app),
      _ => {},
    });
  Ok(())
}

/// The window is created here, not in `tauri.conf.json`, so the data source can be chosen per launch
/// with `TEAMCRAFT_BACKEND=server|mock` (the UI reads it from `localStorage`).
fn create_main_window(app: &tauri::App) -> tauri::Result<()> {
  let prefs = app.state::<ShellPrefsState>().get();
  let is_hidden = prefs.is_start_minimized && tray::has_tray(app.handle());
  let mut window = WebviewWindowBuilder::new(app, tray::MAIN_WINDOW, WebviewUrl::default())
    .title("TeamCraft")
    .visible(!is_hidden)
    .inner_size(1360.0, 860.0)
    .min_inner_size(720.0, 520.0);
  if let Ok(mode) = std::env::var("TEAMCRAFT_BACKEND")
    && matches!(mode.as_str(), "server" | "mock")
  {
    window = window.initialization_script(format!(
      "try {{ localStorage.setItem('teamcraft.backend', '{mode}'); }} catch (_) {{}}"
    ));
  }
  window.build()?;
  Ok(())
}

/// The client core reads its access token synchronously; this keeps a copy current (the login code
/// refreshes it before it expires) and clears it on sign-out.
fn keep_tokens_fresh(service: Arc<AuthService<KeyringStore>>, tokens: TokenCache) {
  tauri::async_runtime::spawn(async move {
    loop {
      tokens.set(service.bearer_token().await.ok());
      tokio::time::sleep(TOKEN_REFRESH).await;
    }
  });
}

#[cfg(test)]
mod tests {
  /// Tauri refuses an app command that has no permission once the app has an ACL manifest, and
  /// nothing fails at build time: every registered command must be in `build.rs` and allowed in
  /// the capability.
  #[test]
  fn every_registered_command_is_listed_and_allowed() {
    let main = include_str!("main.rs");
    let build = include_str!("../build.rs");
    let capability: serde_json::Value =
      serde_json::from_str(include_str!("../capabilities/default.json")).unwrap_or_default();
    let allowed: Vec<String> = capability["permissions"]
      .as_array()
      .map(|list| {
        list
          .iter()
          .filter_map(|value| value.as_str())
          .map(str::to_owned)
          .collect()
      })
      .unwrap_or_default();

    let handler_start = main.find("generate_handler![").unwrap_or(0);
    let handler = &main[handler_start..];
    let handler = &handler[..handler.find(']').unwrap_or(handler.len())];
    let registered: Vec<&str> = handler
      .trim_start_matches("generate_handler![")
      .split(',')
      .filter_map(|path| path.trim().rsplit("::").next())
      .filter(|name| !name.is_empty())
      .collect();
    assert!(registered.len() > 10, "could not read the handler list");

    for command in registered {
      assert!(
        build.contains(&format!("\"{command}\"")),
        "{command} is missing from build.rs"
      );
      let permission = format!("allow-{}", command.replace('_', "-"));
      assert!(
        allowed.contains(&permission),
        "{permission} is missing from capabilities/default.json"
      );
    }
  }
}
