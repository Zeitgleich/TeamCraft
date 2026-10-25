//! Tauri commands the webview calls. Errors cross the boundary as `{ kind, message }`.

use std::sync::Arc;

use secrecy::SecretString;
use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;
use teamcraft_client_api::brand::BrandView;
use teamcraft_client_api::remote::ApiMethod;
use teamcraft_client_api::server_entry::{ServerOrigin, ServerProfile, is_provider_endpoint};
use teamcraft_client_api::sign_in_look::SignInLook;
use teamcraft_proto::{AvatarContentType, BrandResponse, UserId};

use base64::Engine;

use super::avatars::MAX_AVATAR_BYTES;
use super::servers::ServerState;
use super::{
  ApiBytesResponse, ApiResponse, AuthError, AuthService, DesktopConfig, HTTP_POLICY, KeyringStore,
};
use teamcraft_client_api::oidc_login::LoginRequest;

pub type Service = AuthService<KeyringStore>;

#[derive(Debug, Serialize)]
pub struct CommandError {
  kind: &'static str,
  message: String,
}

impl CommandError {
  fn new(kind: &'static str, message: String) -> Self {
    Self { kind, message }
  }
}

impl From<AuthError> for CommandError {
  fn from(error: AuthError) -> Self {
    Self::new(error.kind(), error.to_string())
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
  /// The brand image as a `data:` URL, for the light theme.
  #[serde(rename = "logo")]
  pub maybe_logo: Option<String>,
  /// The brand's separate image for the dark theme.
  #[serde(rename = "logo_dark")]
  pub maybe_logo_dark: Option<String>,
}

#[tauri::command(rename_all = "snake_case")]
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
#[tauri::command(rename_all = "snake_case")]
pub async fn server_check(
  address: String,
  servers: State<'_, Arc<ServerState>>,
  service: State<'_, Arc<Service>>,
) -> Result<ServerCheckView, CommandError> {
  let origin = ServerOrigin::parse(&address, HTTP_POLICY)
    .map_err(|error| CommandError::new(error.i18n_key(), error.to_string()))?;
  if !servers.managed.allows(&origin) {
    return Err(CommandError::new(
      "server-entry-locked",
      "the managed configuration allows another server only".to_owned(),
    ));
  }
  let checked = service
    .probe_server(&origin)
    .await
    .map_err(|error| CommandError::new(error.i18n_key(), error.to_string()))?;
  let (maybe_logo, maybe_logo_dark) = brand_logos(&service, &checked.profile.origin, &checked.brand).await;
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
    maybe_logo,
    maybe_logo_dark,
  })
}

/// "Did you mean …?" for an address that does not resolve: looks up the same name under other
/// common top-level domains, only when the user asked for it (a button on the server entry).
/// Answers the host (and port) of the first one that resolves; it is only suggested, never used.
#[tauri::command(rename_all = "snake_case")]
pub async fn server_suggest(
  address: String,
  servers: State<'_, Arc<ServerState>>,
  service: State<'_, Arc<Service>>,
) -> Result<Option<String>, CommandError> {
  let origin = ServerOrigin::parse(&address, HTTP_POLICY)
    .map_err(|error| CommandError::new(error.i18n_key(), error.to_string()))?;
  // A managed computer allows one server only; nothing else to suggest.
  if servers.managed.is_locked {
    return Ok(None);
  }
  Ok(
    service
      .suggest_server(&origin)
      .await
      .map(|suggestion| suggestion.host().to_owned()),
  )
}

/// The checked server's brand image for its confirmation, light and (when different) dark, as
/// `data:` URLs: chosen like the UI's brand header (`BrandView::header_image`, which keeps only
/// asset paths of the brand endpoint). An image that fails is left out; the name still shows.
async fn brand_logos(
  service: &Service,
  origin: &ServerOrigin,
  brand: &BrandResponse,
) -> (Option<String>, Option<String>) {
  let Some(brand) = BrandView::from_response(brand.clone(), "") else {
    return (None, None);
  };
  let maybe_light = brand.header_image(false).map(str::to_owned);
  let maybe_dark = brand
    .header_image(true)
    .filter(|dark| Some(*dark) != maybe_light.as_deref())
    .map(str::to_owned);
  let fetch = |maybe_path: Option<String>| async move {
    let path = maybe_path?;
    match service.brand_asset(origin.as_str(), &path).await {
      Ok(image) if image.status == 200 => image
        .content_type
        .map(|content_type| format!("data:{content_type};base64,{}", image.body_base64)),
      Ok(image) => {
        tracing::debug!(status = image.status, "the server has no brand image");
        None
      },
      Err(error) => {
        tracing::debug!(%error, "could not load the brand image");
        None
      },
    }
  };
  tokio::join!(fetch(maybe_light), fetch(maybe_dark))
}

/// Makes the checked server with this origin the active account's server.
#[tauri::command(rename_all = "snake_case")]
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
    .ok_or_else(|| {
      CommandError::new(
        "server-entry-not-checked",
        "this server was not checked".to_owned(),
      )
    })?;
  // A stored token of this server with other sign-in settings must not be used any more (the
  // settings that sign-in uses: origin, issuer, client and scopes; not the login hint).
  if let Some(previous) = servers.directory.profile(&profile.origin)
    && DesktopConfig::from_profile(&previous) != DesktopConfig::from_profile(&profile)
  {
    service
      .forget_tokens(&DesktopConfig::from_profile(&previous))
      .await
      .map_err(CommandError::from)?;
  }
  servers
    .directory
    .activate(profile.clone())
    .map_err(|error| CommandError::new("server-entry-not-saved", error.to_string()))?;
  service
    .set_server(Some(DesktopConfig::from_profile(&profile)))
    .await;
  Ok(view(&profile))
}

/// Leaves the active server (the user is signed out already): back to the server entry.
#[tauri::command(rename_all = "snake_case")]
pub async fn server_forget(
  servers: State<'_, Arc<ServerState>>,
  service: State<'_, Arc<Service>>,
) -> Result<(), CommandError> {
  if servers.managed.is_locked {
    return Err(CommandError::new(
      "server-entry-locked",
      "the managed configuration does not allow changing the server".to_owned(),
    ));
  }
  refuse_while_signed_in(&service).await?;
  if let Some(active) = servers.directory.active() {
    service
      .forget_tokens(&DesktopConfig::from_profile(&active))
      .await
      .map_err(CommandError::from)?;
  }
  servers
    .directory
    .forget_active()
    .map_err(|error| CommandError::new("server-entry-not-saved", error.to_string()))?;
  service.set_server(None).await;
  Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn auth_restore(service: State<'_, Arc<Service>>) -> Result<bool, CommandError> {
  Ok(service.restore().await?)
}

#[tauri::command(rename_all = "snake_case")]
/// Signs in through the system browser. `look` (language, theme, brand) styles the page the
/// browser shows afterwards; an older UI sends none.
pub async fn auth_login(
  look: Option<SignInLook>,
  app: AppHandle,
  servers: State<'_, Arc<ServerState>>,
  service: State<'_, Arc<Service>>,
) -> Result<(), CommandError> {
  let look = look.unwrap_or_default();
  let maybe_profile = servers.directory.active();
  // The sign-in screen's fresh answer wins over the profile saved at the last server check, so a
  // changed admin setting applies to installed apps too.
  let request = LoginRequest {
    is_login_page_shown: look.maybe_login_page_always_shown.unwrap_or_else(|| {
      maybe_profile
        .as_ref()
        .is_none_or(|profile| profile.is_login_page_always_shown)
    }),
    maybe_login_hint: maybe_profile
      .as_ref()
      .and_then(|profile| profile.maybe_login_hint.as_deref()),
    maybe_locale: look.maybe_locale.as_deref(),
  };
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
  let signed_in = service.login(&look, &request, open).await?;
  if let (Some(profile), Some(hint)) = (maybe_profile, signed_in.maybe_login_hint)
    && let Err(error) = servers
      .directory
      .set_login_hint(&profile.origin, &profile.issuer, hint)
  {
    tracing::warn!(%error, "could not remember the account for the next sign-in");
  }
  Ok(())
}

/// Signs in a local account. The password goes straight into a secret and is never logged; errors
/// carry the kinds `refused`, `disabled`, `rate_limited` and `network`.
#[tauri::command(rename_all = "snake_case")]
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

#[tauri::command(rename_all = "snake_case")]
pub fn auth_cancel(service: State<'_, Arc<Service>>) {
  service.cancel_login();
}

#[tauri::command(rename_all = "snake_case")]
pub async fn auth_logout(service: State<'_, Arc<Service>>) -> Result<(), CommandError> {
  Ok(service.logout().await?)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn api_get(path: String, service: State<'_, Arc<Service>>) -> Result<ApiResponse, CommandError> {
  Ok(service.api_get(&path).await?)
}

/// A request without credentials (the brand of the server, shown before sign-in).
#[tauri::command(rename_all = "snake_case")]
pub async fn public_get(path: String, service: State<'_, Arc<Service>>) -> Result<ApiResponse, CommandError> {
  Ok(service.public_get(&path).await?)
}

/// A brand image of the active server as bytes (the webview shows it as a `data:` URL).
#[tauri::command(rename_all = "snake_case")]
pub async fn public_asset(
  path: String,
  service: State<'_, Arc<Service>>,
) -> Result<ApiBytesResponse, CommandError> {
  Ok(service.public_asset(&path).await?)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn api_request(
  method: ApiMethod,
  path: String,
  body: Option<String>,
  service: State<'_, Arc<Service>>,
) -> Result<ApiResponse, CommandError> {
  Ok(service.api_request(method, &path, body).await?)
}

/// Like `api_request` with a binary body (`content_type` and base64 `body_base64` together).
#[tauri::command(rename_all = "snake_case")]
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
        .map_err(|_| CommandError::new("invalid_response", "the body is not base64".to_owned()))?;
      Some((content_type, bytes))
    },
    _ => None,
  };
  Ok(service.api_request_bytes(method, &path, maybe_body).await?)
}

/// The picture of a user, fetched by the shell from the presigned URL the API names (the
/// webview's content security policy keeps object storage out).
#[tauri::command(rename_all = "snake_case")]
pub async fn avatar_download(
  user_id: UserId,
  service: State<'_, Arc<Service>>,
) -> Result<ApiBytesResponse, CommandError> {
  Ok(service.avatar_download(user_id).await?)
}

/// Makes the picture (base64 `body_base64`) the signed-in user's avatar: announce, upload to the
/// presigned URL and complete, all in the shell. Answers like `api_request`.
#[tauri::command(rename_all = "snake_case")]
pub async fn avatar_upload(
  content_type: AvatarContentType,
  body_base64: String,
  service: State<'_, Arc<Service>>,
) -> Result<ApiResponse, CommandError> {
  // Refused before decoding: base64 is 4 characters per 3 bytes.
  if body_base64.len() as u64 > MAX_AVATAR_BYTES.div_ceil(3) * 4 {
    return Err(CommandError::new(
      "unsupported",
      "the picture is too large".to_owned(),
    ));
  }
  let bytes = base64::engine::general_purpose::STANDARD
    .decode(body_base64)
    .map_err(|_| CommandError::new("invalid_response", "the body is not base64".to_owned()))?;
  Ok(service.avatar_upload(content_type, bytes).await?)
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
    return Err(CommandError::new(
      "server-entry-signed-in",
      "sign out before changing the server".to_owned(),
    ));
  }
  Ok(())
}
