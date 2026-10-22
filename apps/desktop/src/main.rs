//! Desktop shell. It hosts the webview and owns sign-in: the OIDC flow, the keyring and the
//! authenticated API calls live here, so the UI never holds a token. The UI itself is
//! `teamcraft-ui`; client logic will move into `teamcraft-core`.

// Hides the console window on Windows release builds; harmless on macOS and Linux.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod auth;

use anyhow::Context;
use tauri::{WebviewUrl, WebviewWindowBuilder};
use tracing_subscriber::EnvFilter;

use crate::auth::{AuthService, DesktopConfig, KeyringStore};

fn main() -> anyhow::Result<()> {
  tracing_subscriber::fmt()
    .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
    .init();

  let config = DesktopConfig::from_env();
  let store = KeyringStore::new(&config.issuer);
  let service = AuthService::new(config, store).context("could not set up sign-in")?;

  tauri::Builder::default()
    .plugin(tauri_plugin_opener::init())
    .manage(service)
    .setup(|app| {
      create_main_window(app)?;
      Ok(())
    })
    .invoke_handler(tauri::generate_handler![
      auth::commands::auth_restore,
      auth::commands::auth_login,
      auth::commands::auth_cancel,
      auth::commands::auth_logout,
      auth::commands::api_get,
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
