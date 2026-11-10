//! Who a SCIM request comes from: a client's token `tcscim_<client id>_<secret>`. The client is
//! looked up by its ID and the SHA-256 of the whole token compared in constant time, so a wrong
//! token costs one indexed read and learns nothing from timing. Missing, malformed, unknown and
//! wrong tokens all get the same 401, which the request guard counts against the address like any
//! failed authentication. The token is never logged.

use std::time::Instant;

use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use subtle::ConstantTimeEq;
use teamcraft_crypto::account::digest_token;
use teamcraft_proto::{ScimClientId, ScimIdentityAttribute};
use tracing::{debug, warn};

use super::error::ScimError;
use crate::error::ApiError;
use crate::state::AppState;

/// The prefix of every SCIM token, so a leaked one is recognisable (secret scanners).
pub(super) const TOKEN_PREFIX: &str = "tcscim_";

/// An authenticated SCIM client.
#[derive(Debug, Clone)]
pub struct ScimCaller {
  pub client: ScimClientId,
  pub identity_attribute: ScimIdentityAttribute,
}

impl FromRequestParts<AppState> for ScimCaller {
  type Rejection = ScimError;

  async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
    let maybe_token = parts
      .headers
      .get(AUTHORIZATION)
      .and_then(|value| value.to_str().ok())
      .and_then(|value| value.split_once(' '))
      .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
      .map(|(_, token)| token.trim());
    let caller = authenticate(state, maybe_token).await?;
    state
      .rate_limits
      .check_scim(
        caller.client,
        state.settings.get().scim.requests_per_minute,
        Instant::now(),
      )
      .map_err(|error| match error {
        ApiError::RateLimited { retry_after_secs } => ScimError::RateLimited { retry_after_secs },
        other => ScimError::Api(other),
      })?;
    // For the console only: a failure here must not refuse the request.
    if let Err(error) = state.database.note_scim_client_used(caller.client).await {
      warn!(%error, "noting that a SCIM client was used failed");
    }
    Ok(caller)
  }
}

async fn authenticate(state: &AppState, maybe_token: Option<&str>) -> Result<ScimCaller, ScimError> {
  let token = maybe_token.ok_or(ScimError::InvalidToken)?;
  let client = client_of(token).ok_or(ScimError::InvalidToken)?;
  let Some(credentials) = state.database.scim_client_credentials(client).await? else {
    debug!("a SCIM request named no existing client");
    return Err(ScimError::InvalidToken);
  };
  let is_match: bool = digest_token(token)
    .ct_eq(credentials.token_sha256.as_slice())
    .into();
  if !is_match {
    warn!(scim_client = %client, "a SCIM request with a wrong token was refused");
    return Err(ScimError::InvalidToken);
  }
  Ok(ScimCaller {
    client: credentials.id,
    identity_attribute: ScimIdentityAttribute::from(credentials.identity_attribute),
  })
}

/// The client ID a token names, if it has the shape of one.
pub(super) fn client_of(token: &str) -> Option<ScimClientId> {
  let (id, secret) = token.strip_prefix(TOKEN_PREFIX)?.split_once('_')?;
  if secret.is_empty() {
    return None;
  }
  id.parse().ok()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_client_is_read_from_the_token() {
    let token = "tcscim_123_secret_with_underscores-and-dashes";
    assert_eq!(client_of(token).map(|id| id.to_string()), Some("123".into()));
    for malformed in [
      "",
      "tcscim_",
      "tcscim_123",
      "tcscim_123_",
      "tcscim_abc_x",
      "Bearer x",
      "xtcscim_1_a",
    ] {
      assert_eq!(client_of(malformed), None, "{malformed}");
    }
  }
}
