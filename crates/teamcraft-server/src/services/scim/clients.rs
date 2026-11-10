//! SCIM clients in the admin console: made, rotated and revoked by admins with `manage_users`.
//! A token is `tcscim_<client id>_<32 random bytes, base64url>`, shown once; only its SHA-256 is
//! stored (high entropy, so no slow hash is needed), and it is never logged.

use serde_json::json;
use teamcraft_crypto::account::{digest_token, new_one_time_token};
use teamcraft_db::{NewScimClient, ScimClientCreation, ScimClientRow};
use teamcraft_proto::scim::SCIM_BASE_PATH;
use teamcraft_proto::{
  CreateScimClientRequest, MAX_SCIM_CLIENT_NAME_CHARS, MAX_SCIM_CLIENTS, Permission, ScimClientId,
  ScimClientResponse, ScimClientTokenResponse, ScimClientsResponse, ScimIdentityAttribute, Target,
  WebhookSecretText,
};

use super::auth::TOKEN_PREFIX;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::access::require;
use crate::services::audit;
use crate::services::convert::unix_millis;
use crate::state::AppState;

pub async fn list_clients(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<ScimClientsResponse, ApiError> {
  require(state, caller, Permission::ManageUsers, &Target::Instance)?;
  let clients = state.database.list_scim_clients().await?;
  let base = state
    .settings
    .get()
    .mail
    .public_base_url
    .trim_end_matches('/')
    .to_owned();
  Ok(ScimClientsResponse {
    clients: clients.iter().map(response).collect(),
    maybe_base_url: (!base.is_empty()).then(|| format!("{base}{SCIM_BASE_PATH}")),
  })
}

pub async fn create_client(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: CreateScimClientRequest,
) -> Result<ScimClientTokenResponse, ApiError> {
  require(state, caller, Permission::ManageUsers, &Target::Instance)?;
  let name = request.name.trim();
  if name.is_empty()
    || name.chars().count() > MAX_SCIM_CLIENT_NAME_CHARS
    || name.chars().any(char::is_control)
  {
    return Err(ApiError::InvalidRequest(format!(
      "the name must have 1 to {MAX_SCIM_CLIENT_NAME_CHARS} characters"
    )));
  }
  if matches!(request.identity_attribute, ScimIdentityAttribute::Unknown(_)) {
    return Err(ApiError::InvalidRequest("unknown identity attribute".into()));
  }
  let id: ScimClientId = state.next_id()?;
  let token = new_token(id);
  let outcome = state
    .database
    .create_scim_client(
      &NewScimClient {
        id,
        name,
        token_sha256: &digest_token(token.expose()),
        identity_attribute: request.identity_attribute.as_str(),
        created_by: caller.user.id,
      },
      MAX_SCIM_CLIENTS,
    )
    .await?;
  if outcome == ScimClientCreation::TooMany {
    return Err(ApiError::Conflict(format!(
      "at most {MAX_SCIM_CLIENTS} SCIM clients"
    )));
  }
  audit::record(
    state,
    Some(caller.user.id),
    "scim_client_created",
    None,
    &json!({"scim_client": id.to_string(), "name": name, "identity_attribute": request.identity_attribute.as_str()}),
  )
  .await?;
  Ok(ScimClientTokenResponse {
    token,
    client: find(state, id).await?,
  })
}

/// A new token for the client; the old one stops working at once.
pub async fn rotate_token(
  state: &AppState,
  caller: &AuthenticatedUser,
  client: ScimClientId,
) -> Result<ScimClientTokenResponse, ApiError> {
  require(state, caller, Permission::ManageUsers, &Target::Instance)?;
  let token = new_token(client);
  if !state
    .database
    .rotate_scim_client_token(client, &digest_token(token.expose()))
    .await?
  {
    return Err(not_found());
  }
  audit::record(
    state,
    Some(caller.user.id),
    "scim_client_token_rotated",
    None,
    &json!({"scim_client": client.to_string()}),
  )
  .await?;
  Ok(ScimClientTokenResponse {
    token,
    client: find(state, client).await?,
  })
}

/// Revokes the client: its token stops working and its groups go. The accounts it made stay.
pub async fn delete_client(
  state: &AppState,
  caller: &AuthenticatedUser,
  client: ScimClientId,
) -> Result<(), ApiError> {
  require(state, caller, Permission::ManageUsers, &Target::Instance)?;
  if !state.database.delete_scim_client(client).await? {
    return Err(not_found());
  }
  audit::record(
    state,
    Some(caller.user.id),
    "scim_client_deleted",
    None,
    &json!({"scim_client": client.to_string()}),
  )
  .await?;
  Ok(())
}

fn new_token(client: ScimClientId) -> WebhookSecretText {
  WebhookSecretText::new(format!(
    "{TOKEN_PREFIX}{client}_{}",
    new_one_time_token(&mut rand_core::OsRng)
  ))
}

async fn find(state: &AppState, client: ScimClientId) -> Result<ScimClientResponse, ApiError> {
  state
    .database
    .list_scim_clients()
    .await?
    .iter()
    .find(|row| row.id == client)
    .map(response)
    .ok_or_else(not_found)
}

fn not_found() -> ApiError {
  ApiError::scim_client_not_found()
}

fn response(row: &ScimClientRow) -> ScimClientResponse {
  ScimClientResponse {
    id: row.id,
    name: row.name.clone(),
    identity_attribute: ScimIdentityAttribute::from(row.identity_attribute.clone()),
    created_at: unix_millis(row.created_at),
    token_rotated_at: unix_millis(row.token_rotated_at),
    maybe_last_used_at: row.maybe_last_used_at.map(unix_millis),
    user_count: u32::try_from(row.user_count).unwrap_or(u32::MAX),
    group_count: u32::try_from(row.group_count).unwrap_or(u32::MAX),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_token_names_its_client() {
    let id: ScimClientId = "42".parse().unwrap();
    let token = new_token(id);
    assert_eq!(super::super::auth::client_of(token.expose()), Some(id));
    assert!(token.expose().len() > 40);
    assert!(!format!("{token:?}").contains(token.expose()));
  }
}
