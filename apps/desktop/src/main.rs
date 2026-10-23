//! Desktop shell. It hosts the webview and owns sign-in: the OIDC flow, the keyring and the
//! authenticated API calls live here, so the UI never holds a token. The UI itself is
//! `teamcraft-ui`; client logic will move into `teamcraft-core`.

// Hides the console window on Windows release builds; harmless on macOS and Linux.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod auth;
mod autostart;
mod calls;
mod core;
mod notifications;
#[cfg(windows)]
mod windows_shell;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tauri::path::BaseDirectory;
use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use tracing_subscriber::EnvFilter;

use crate::auth::{AuthService, DesktopConfig, KeyringStore};
use crate::calls::{BUNDLED_MODEL, CALL_CHANGED_EVENT, CallHost, MediaFiles};
use crate::core::TokenCache;
use crate::core::commands::CoreHost;

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
    .plugin(tauri_plugin_window_state::Builder::default().build())
    .manage(service)
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
      create_main_window(app)?;
      #[cfg(windows)]
      windows_shell::forward_deep_links(app.handle());
      Ok(())
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
      autostart::autostart_get,
      autostart::autostart_set,
    ])
    .run(tauri::generate_context!())
    .context("failed to run the TeamCraft desktop shell")
}

/// The window is created here, not in `tauri.conf.json`, so the data source can be chosen per launch
/// with `TEAMCRAFT_BACKEND=server|mock` (the UI reads it from `localStorage`).
fn create_main_window(app: &tauri::App) -> tauri::Result<()> {
  let mut window = WebviewWindowBuilder::new(app, "main", WebviewUrl::default())
    .title("TeamCraft")
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
