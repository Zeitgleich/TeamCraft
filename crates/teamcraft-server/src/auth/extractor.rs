use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use teamcraft_db::User;
use teamcraft_proto::Role;

use super::error::AuthError;
use crate::error::ApiError;
use crate::services::accounts;
use crate::state::AppState;

/// The caller of a request: a valid bearer token plus the matching user row.
///
/// Rejects requests without a token, with an invalid token, from users without a TeamCraft role
/// and from deactivated accounts. The role comes from the token on every request, so role changes
/// in the identity provider apply as soon as the access token is renewed.
#[derive(Debug, Clone)]
pub struct AuthenticatedUser {
  pub user: User,
  pub role: Role,
}

impl FromRequestParts<AppState> for AuthenticatedUser {
  type Rejection = ApiError;

  async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
    let token = bearer_token(parts)?;
    let identity = state.validator.validate(token).await?;
    accounts::authenticate(state, identity).await
  }
}

fn bearer_token(parts: &Parts) -> Result<&str, AuthError> {
  let header = parts.headers.get(AUTHORIZATION).ok_or(AuthError::MissingToken)?;
  let value = header
    .to_str()
    .map_err(|_| AuthError::InvalidToken("malformed authorization header".into()))?;
  let (scheme, token) = value.split_once(' ').ok_or(AuthError::MissingToken)?;
  if !scheme.eq_ignore_ascii_case("bearer") || token.trim().is_empty() {
    return Err(AuthError::MissingToken);
  }
  Ok(token.trim())
}

#[cfg(test)]
mod tests {
  use axum::http::Request;

  use super::*;

  #[test]
  fn extracts_bearer_token_case_insensitively() {
    assert_eq!(bearer_token(&parts_with("Bearer abc.def")).unwrap(), "abc.def");
    assert_eq!(bearer_token(&parts_with("bearer abc")).unwrap(), "abc");
  }

  #[test]
  fn rejects_missing_or_other_schemes() {
    assert!(matches!(
      bearer_token(&parts_with("Basic dXNlcjpwdw==")),
      Err(AuthError::MissingToken)
    ));
    assert!(matches!(
      bearer_token(&parts_with("Bearer ")),
      Err(AuthError::MissingToken)
    ));
    assert!(matches!(
      bearer_token(&parts_with("Bearer")),
      Err(AuthError::MissingToken)
    ));
    let (parts, ()) = Request::builder().body(()).unwrap().into_parts();
    assert!(matches!(bearer_token(&parts), Err(AuthError::MissingToken)));
  }

  fn parts_with(authorization: &str) -> Parts {
    let (parts, ()) = Request::builder()
      .header(AUTHORIZATION, authorization)
      .body(())
      .unwrap()
      .into_parts();
    parts
  }
}
