//! Tauri commands the webview calls. Errors cross the boundary as `{ kind, message }`.

use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;

use super::{ApiResponse, AuthError, AuthService, KeyringStore};

pub type Service = AuthService<KeyringStore>;

#[derive(Debug, Serialize)]
pub struct CommandError {
  kind: &'static str,
  message: String,
}

impl From<AuthError> for CommandError {
  fn from(error: AuthError) -> Self {
    Self {
      kind: error.kind(),
      message: error.to_string(),
    }
  }
}

#[tauri::command]
pub async fn auth_restore(service: State<'_, Arc<Service>>) -> Result<bool, CommandError> {
  Ok(service.restore().await?)
}

#[tauri::command]
pub async fn auth_login(app: AppHandle, service: State<'_, Arc<Service>>) -> Result<(), CommandError> {
  let open = move |url: &str| {
    app
      .opener()
      .open_url(url, None::<&str>)
      .map_err(|error| error.to_string())
  };
  Ok(service.login(open).await?)
}

#[tauri::command]
pub fn auth_cancel(service: State<'_, Arc<Service>>) {
  service.cancel_login();
}

#[tauri::command]
pub async fn auth_logout(service: State<'_, Arc<Service>>) -> Result<(), CommandError> {
  Ok(service.logout().await?)
}

#[tauri::command]
pub async fn api_get(path: String, service: State<'_, Arc<Service>>) -> Result<ApiResponse, CommandError> {
  Ok(service.api_get(&path).await?)
}
