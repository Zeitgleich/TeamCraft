//! The notetaker's OpenRouter API key (e2ee.md §8.2, admin.md §3.7). Admins store it write-only;
//! it is sealed with the settings key before it reaches the database and handed only to the bot
//! account the signed policy names. Everything else about the notetaker is owner-signed policy or
//! encrypted between members: the server can't turn it on, and the notetaker checks the key it
//! gets against the hash the owners signed.

use rand_core::OsRng;
use serde_json::json;
use teamcraft_proto::{
  AdminNotetakerResponse, ErrorCode, MAX_NOTETAKER_API_KEY_BYTES, NotetakerApiKey,
  NotetakerCredentialsResponse, Permission, PutNotetakerKeyRequest, SecretUpdate, Target, UserKind,
};
use tracing::error;

use super::access::require;
use super::audit;
use super::convert::unix_millis;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

/// Row in `server_settings`; also the associated data of the sealed key.
const SETTING_NAME: &str = "notetaker.openrouter";

/// Whether a key is stored (never the key).
pub async fn get_notetaker(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<AdminNotetakerResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  response(state).await
}

/// Stores, replaces or removes the key.
pub async fn put_key(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: PutNotetakerKeyRequest,
) -> Result<AdminNotetakerResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  match &request.key {
    SecretUpdate::Keep => return response(state).await,
    SecretUpdate::Clear => {
      state.database.delete_server_setting(SETTING_NAME).await?;
    },
    SecretUpdate::Set(secret) => {
      let key = secret.expose_secret().trim();
      if key.is_empty() || key.len() > MAX_NOTETAKER_API_KEY_BYTES || key.chars().any(char::is_whitespace) {
        return Err(ApiError::InvalidRequest(format!(
          "the API key must be 1 to {MAX_NOTETAKER_API_KEY_BYTES} bytes without spaces"
        )));
      }
      let settings_key = state.maybe_settings_key.as_ref().ok_or_else(|| {
        ApiError::Unavailable(
          "API keys can only be stored with a settings key ([secrets] in the configuration)".into(),
        )
      })?;
      let sealed = settings_key
        .seal(SETTING_NAME, key.as_bytes(), &mut OsRng)
        .map_err(|error| ApiError::Internal(error.to_string()))?;
      state
        .database
        .put_server_setting(SETTING_NAME, "{}", Some(&sealed), caller.user.id)
        .await?;
    },
  }
  let action = match request.key {
    SecretUpdate::Clear => "notetaker_key_cleared",
    _ => "notetaker_key_set",
  };
  audit::record(state, Some(caller.user.id), action, None, &json!({})).await?;
  response(state).await
}

/// The key, for the notetaker's bot account only: the account the server's copy of the signed
/// policy names, while the notetaker is on.
pub async fn credentials(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<NotetakerCredentialsResponse, ApiError> {
  let is_notetaker = state.policy.get().notetaker.is_notetaker(caller.user.id);
  if caller.user.kind != UserKind::Bot || !is_notetaker {
    return Err(ApiError::Forbidden(
      "only the notetaker's bot account of an enabled policy gets its key".into(),
    ));
  }
  let not_stored = || ApiError::NotFound {
    code: ErrorCode::NotetakerKeyNotFound,
    what: "notetaker API key",
  };
  let row = state
    .database
    .server_setting(SETTING_NAME)
    .await?
    .ok_or_else(not_stored)?;
  let sealed = row.maybe_sealed_secret.ok_or_else(not_stored)?;
  let settings_key = state.maybe_settings_key.as_ref().ok_or_else(|| {
    error!("a notetaker API key is stored but no settings key is configured ([secrets])");
    ApiError::Unavailable("the server has no settings key to open the API key with".into())
  })?;
  let plain = settings_key.open(SETTING_NAME, &sealed).map_err(|_| {
    error!("the stored notetaker API key cannot be decrypted with this settings key");
    ApiError::Unavailable("the stored API key cannot be opened".into())
  })?;
  let key = String::from_utf8(plain.expose_secret().to_vec())
    .map_err(|_| ApiError::Internal("the stored API key is not text".into()))?;
  Ok(NotetakerCredentialsResponse {
    api_key: NotetakerApiKey::new(key),
  })
}

async fn response(state: &AppState) -> Result<AdminNotetakerResponse, ApiError> {
  let maybe_row = state.database.server_setting(SETTING_NAME).await?;
  Ok(AdminNotetakerResponse {
    is_key_stored: maybe_row
      .as_ref()
      .is_some_and(|row| row.maybe_sealed_secret.is_some()),
    can_store_secrets: state.maybe_settings_key.is_some(),
    maybe_key_updated_at: maybe_row.map(|row| unix_millis(row.updated_at)),
  })
}
