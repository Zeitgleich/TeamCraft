//! Tauri commands the webview calls. Errors cross the boundary as `{ kind, message }`.

use std::sync::Arc;

use secrecy::SecretString;
use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;
use teamcraft_client_api::remote::ApiMethod;
use teamcraft_client_api::server_entry::{ServerOrigin, ServerProfile, is_provider_endpoint};
use teamcraft_proto::BrandResponse;

use base64::Engine;

use super::servers::ServerState;
use super::{
  ApiBytesResponse, ApiResponse, AuthError, AuthService, DesktopConfig, HTTP_POLICY, KeyringStore,
};

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

/// The account's server as the UI shows it.
#[derive(Debug, Serialize)]
pub struct ServerView {
  pub origin: String,
  pub host: String,
  pub display_name: String,
  pub has_local_accounts: bool,
}

/// Where the server entry stands at startup.
#[derive(Debug, Serialize)]
pub struct ServerStateView {
  #[serde(rename = "active")]
  pub maybe_active: Option<ServerView>,
  /// A server the computer's configuration names, offered without typing.
  #[serde(rename = "managed_address")]
  pub maybe_managed_address: Option<String>,
  /// The user may not change the server.
  pub is_locked: bool,
}

/// A checked server waiting for confirmation, with its brand and identity provider.
#[derive(Debug, Serialize)]
pub struct ServerCheckView {
  pub server: ServerView,
  pub brand: BrandResponse,
  /// Where the password will be entered; the confirmation shows it.
  pub identity_provider_host: String,
  /// The provider lies outside the server's domain: the confirmation warns.
  pub is_identity_provider_elsewhere: bool,
}

#[tauri::command]
pub fn server_state(
  servers: State<'_, Arc<ServerState>>,
  service: State<'_, Arc<Service>>,
) -> ServerStateView {
  let maybe_active = service
    .active_server()
    .and_then(|_| servers.directory.active())
    .map(|profile| view(&profile));
  ServerStateView {
    maybe_active,
    maybe_managed_address: servers.managed.maybe_address.clone(),
    is_locked: servers.managed.is_locked,
  }
}

/// Normalises what the user typed, asks that server how to sign in and keeps the answer for
/// [`server_confirm`]. Errors carry the i18n key of their message as `kind`.
#[tauri::command]
pub async fn server_check(
  address: String,
  servers: State<'_, Arc<ServerState>>,
  service: State<'_, Arc<Service>>,
) -> Result<ServerCheckView, CommandError> {
  let origin = ServerOrigin::parse(&address, HTTP_POLICY).map_err(|error| CommandError {
    kind: error.i18n_key(),
    message: error.to_string(),
  })?;
  if !servers.managed.allows(&origin) {
    return Err(CommandError {
      kind: "server-entry-locked",
      message: "the managed configuration allows another server only".to_owned(),
    });
  }
  let checked = service
    .probe_server(&origin)
    .await
    .map_err(|error| CommandError {
      kind: error.i18n_key(),
      message: error.to_string(),
    })?;
  let server = view(&checked.profile);
  *servers
    .pending
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(checked.profile);
  Ok(ServerCheckView {
    server,
    brand: checked.brand,
    identity_provider_host: checked.identity_provider_host,
    is_identity_provider_elsewhere: checked.is_identity_provider_elsewhere,
  })
}

/// Makes the checked server with this origin the active account's server.
#[tauri::command]
pub async fn server_confirm(
  origin: String,
  servers: State<'_, Arc<ServerState>>,
  service: State<'_, Arc<Service>>,
) -> Result<ServerView, CommandError> {
  refuse_while_signed_in(&service).await?;
  let profile = servers
    .pending
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .take()
    .filter(|profile| profile.origin.as_str() == origin)
    .ok_or_else(|| CommandError {
      kind: "server-entry-not-checked",
      message: "this server was not checked".to_owned(),
    })?;
  // A stored token of this server with other sign-in settings must not be used any more.
  if let Some(previous) = servers.directory.profile(&profile.origin)
    && previous != profile
  {
    service
      .forget_tokens(&DesktopConfig::from_profile(&previous))
      .await
      .map_err(CommandError::from)?;
  }
  servers
    .directory
    .activate(profile.clone())
    .map_err(|error| CommandError {
      kind: "server-entry-not-saved",
      message: error.to_string(),
    })?;
  service
    .set_server(Some(DesktopConfig::from_profile(&profile)))
    .await;
  Ok(view(&profile))
}

/// Leaves the active server (the user is signed out already): back to the server entry.
#[tauri::command]
pub async fn server_forget(
  servers: State<'_, Arc<ServerState>>,
  service: State<'_, Arc<Service>>,
) -> Result<(), CommandError> {
  if servers.managed.is_locked {
    return Err(CommandError {
      kind: "server-entry-locked",
      message: "the managed configuration does not allow changing the server".to_owned(),
    });
  }
  refuse_while_signed_in(&service).await?;
  if let Some(active) = servers.directory.active() {
    service
      .forget_tokens(&DesktopConfig::from_profile(&active))
      .await
      .map_err(CommandError::from)?;
  }
  servers.directory.forget_active().map_err(|error| CommandError {
    kind: "server-entry-not-saved",
    message: error.to_string(),
  })?;
  service.set_server(None).await;
  Ok(())
}

#[tauri::command]
pub async fn auth_restore(service: State<'_, Arc<Service>>) -> Result<bool, CommandError> {
  Ok(service.restore().await?)
}

#[tauri::command]
pub async fn auth_login(app: AppHandle, service: State<'_, Arc<Service>>) -> Result<(), CommandError> {
  let open = move |url: &str| {
    // Only web addresses reach the OS opener (never `file:`, `search-ms:` and the like).
    if !is_provider_endpoint(url, HTTP_POLICY) {
      return Err("the sign-in address is not https".to_owned());
    }
    app
      .opener()
      .open_url(url, None::<&str>)
      .map_err(|error| error.to_string())
  };
  Ok(service.login(open).await?)
}

/// Signs in a local account. The password goes straight into a secret and is never logged; errors
/// carry the kinds `refused`, `disabled`, `rate_limited` and `network`.
#[tauri::command]
pub async fn auth_login_local(
  login: String,
  password: String,
  totp_code: Option<String>,
  service: State<'_, Arc<Service>>,
) -> Result<(), CommandError> {
  let password = SecretString::from(password);
  let maybe_totp_code = totp_code
    .as_deref()
    .map(str::trim)
    .filter(|code| !code.is_empty());
  Ok(
    service
      .login_local(login.trim(), &password, maybe_totp_code)
      .await?,
  )
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

/// A brand image of the active server as bytes (the webview shows it as a `data:` URL).
#[tauri::command]
pub async fn public_asset(
  path: String,
  service: State<'_, Arc<Service>>,
) -> Result<ApiBytesResponse, CommandError> {
  Ok(service.public_asset(&path).await?)
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

fn view(profile: &ServerProfile) -> ServerView {
  ServerView {
    origin: profile.origin.as_str().to_owned(),
    host: profile.origin.host().to_owned(),
    display_name: profile.display_name.clone(),
    has_local_accounts: profile.has_local_accounts,
  }
}

/// The server can only change while nobody is signed in to it (sign out first).
async fn refuse_while_signed_in(service: &Service) -> Result<(), CommandError> {
  if service.is_signed_in().await {
    return Err(CommandError {
      kind: "server-entry-signed-in",
      message: "sign out before changing the server".to_owned(),
    });
  }
  Ok(())
}
