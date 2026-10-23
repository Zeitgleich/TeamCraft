//! Tauri commands the webview calls. Errors cross the boundary as `{ kind, message }`.

use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;
use teamcraft_client_api::remote::ApiMethod;

use base64::Engine;

use super::{ApiBytesResponse, ApiResponse, AuthError, AuthService, KeyringStore};

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

/// A request without credentials (the brand of the server, shown before sign-in).
#[tauri::command]
pub async fn public_get(path: String, service: State<'_, Arc<Service>>) -> Result<ApiResponse, CommandError> {
  Ok(service.public_get(&path).await?)
}

#[tauri::command]
pub async fn api_request(
  method: ApiMethod,
  path: String,
  body: Option<String>,
  service: State<'_, Arc<Service>>,
) -> Result<ApiResponse, CommandError> {
  Ok(service.api_request(method, &path, body).await?)
}

/// Like `api_request` with a binary body (`content_type` and base64 `body_base64` together).
#[tauri::command]
pub async fn api_request_bytes(
  method: ApiMethod,
  path: String,
  content_type: Option<String>,
  body_base64: Option<String>,
  service: State<'_, Arc<Service>>,
) -> Result<ApiBytesResponse, CommandError> {
  let maybe_body = match (content_type, body_base64) {
    (Some(content_type), Some(encoded)) => {
      let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| CommandError {
          kind: "invalid_response",
          message: "the body is not base64".to_owned(),
        })?;
      Some((content_type, bytes))
    },
    _ => None,
  };
  Ok(service.api_request_bytes(method, &path, maybe_body).await?)
}
