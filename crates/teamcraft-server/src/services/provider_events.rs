//! Identity provider events (docs/provisioning.md §4): the provider posts "something changed" with
//! a bearer secret, and the sync runs soon. The body is never read for its meaning: the run reads
//! the provider itself, so a forged or replayed event can at most start a run.

use serde_json::json;
use subtle::ConstantTimeEq;
use teamcraft_crypto::account::{digest_token, new_one_time_token};
use teamcraft_db::ProviderEventHook;
use teamcraft_proto::{
  CreateProviderEventSecretResponse, Permission, ProviderEventHookResponse, Target, WebhookSecretText,
};
use tracing::{debug, warn};

use super::access::require;
use super::audit;
use super::convert::unix_millis;
use crate::auth::{AuthError, AuthenticatedUser};
use crate::error::ApiError;
use crate::state::AppState;

/// How the hook stands.
pub async fn status(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<ProviderEventHookResponse, ApiError> {
  require(state, caller, Permission::ManageUsers, &Target::Instance)?;
  let maybe_hook = state.database.provider_event_hook().await?;
  Ok(response(state, maybe_hook.as_ref()))
}

/// A new secret, replacing the old one at once. Shown this once; only its hash is kept.
pub async fn create_secret(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<CreateProviderEventSecretResponse, ApiError> {
  require(state, caller, Permission::ManageUsers, &Target::Instance)?;
  let secret = WebhookSecretText::new(new_one_time_token(&mut rand_core::OsRng));
  state
    .database
    .put_provider_event_secret(&digest_token(secret.expose()), caller.user.id)
    .await?;
  audit::record(
    state,
    Some(caller.user.id),
    "provider_event_secret_created",
    None,
    &json!({}),
  )
  .await?;
  let maybe_hook = state.database.provider_event_hook().await?;
  Ok(CreateProviderEventSecretResponse {
    secret,
    hook: response(state, maybe_hook.as_ref()),
  })
}

/// Turns the endpoint off.
pub async fn delete(state: &AppState, caller: &AuthenticatedUser) -> Result<(), ApiError> {
  require(state, caller, Permission::ManageUsers, &Target::Instance)?;
  if state.database.delete_provider_event_hook().await? {
    audit::record(
      state,
      Some(caller.user.id),
      "provider_event_secret_deleted",
      None,
      &json!({}),
    )
    .await?;
  }
  Ok(())
}

/// An event from the provider: checks the secret and asks the sync for a run. 401 without a secret
/// set or with a wrong one (the same answer, so the endpoint says nothing about the setup), 503
/// while the sync doesn't run.
pub async fn receive(state: &AppState, maybe_bearer: Option<&str>) -> Result<(), ApiError> {
  let Some(presented) = maybe_bearer else {
    return Err(AuthError::MissingToken.into());
  };
  let Some(hook) = state.database.provider_event_hook().await? else {
    return Err(AuthError::InvalidToken("identity provider event secret".into()).into());
  };
  let is_match: bool = digest_token(presented)
    .ct_eq(hook.secret_sha256.as_slice())
    .into();
  if !is_match {
    warn!("an identity provider event with a wrong secret was refused");
    return Err(AuthError::InvalidToken("identity provider event secret".into()).into());
  }
  if state.settings.get().maybe_oidc_sync.is_none() {
    return Err(ApiError::Unavailable("identity provider sync (oidc.sync)".into()));
  }
  // Noting the time is for the console; the run matters more.
  if let Err(error) = state.database.note_provider_event().await {
    warn!(%error, "noting an identity provider event failed");
  }
  debug!("identity provider event: a sync run is requested");
  state.oidc_sync.request_run();
  Ok(())
}

fn response(state: &AppState, maybe_hook: Option<&ProviderEventHook>) -> ProviderEventHookResponse {
  ProviderEventHookResponse {
    is_configured: maybe_hook.is_some(),
    maybe_created_at: maybe_hook.map(|hook| unix_millis(hook.created_at)),
    maybe_last_event_at: maybe_hook.and_then(|hook| hook.maybe_last_event_at.map(unix_millis)),
    is_sync_active: state.settings.get().maybe_oidc_sync.is_some(),
  }
}
