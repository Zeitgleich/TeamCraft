use axum::extract::{FromRequestParts, MatchedPath};
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use teamcraft_db::User;
use teamcraft_proto::Role;

use super::error::AuthError;
use crate::error::ApiError;
use crate::meetings::guest::GUEST_ISSUER;
use crate::meetings::guest_scope;
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
  /// The identity provider's session the token belongs to, if it names one.
  pub maybe_oidc_session: Option<String>,
}

impl FromRequestParts<AppState> for AuthenticatedUser {
  type Rejection = ApiError;

  async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
    let token = bearer_token(parts)?;
    let identity = accounts::validate_token(state, token).await?;
    if identity.issuer == GUEST_ISSUER {
      let maybe_path = parts.extensions.get::<MatchedPath>().map(MatchedPath::as_str);
      if !guest_scope::allows(&parts.method, maybe_path) {
        return Err(ApiError::Forbidden("guests can't do that".into()));
      }
    }
    let caller = accounts::authenticate(state, identity).await?;
    if is_write(&parts.method) {
      state.rate_limits.check_user_write(caller.user.id)?;
    }
    Ok(caller)
  }
}

/// An instance admin (role admin or owner). Rejects everybody else with 403 *before* the request
/// body is parsed, so ordinary users learn nothing about the endpoint's input. The services check
/// the finer permission again.
#[derive(Debug, Clone)]
pub struct AdminUser(pub AuthenticatedUser);

impl FromRequestParts<AppState> for AdminUser {
  type Rejection = ApiError;

  async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
    let caller = AuthenticatedUser::from_request_parts(parts, state).await?;
    if crate::services::access::is_instance_admin(&caller) {
      Ok(Self(caller))
    } else {
      Err(ApiError::Forbidden("administrators only".into()))
    }
  }
}

/// The SFU, authenticated by its registration token (checked before the body is parsed).
#[derive(Debug, Clone, Copy)]
pub struct SfuCaller;

impl FromRequestParts<AppState> for SfuCaller {
  type Rejection = ApiError;

  async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
    crate::services::calls::authenticate_sfu(state, bearer_token(parts).ok())?;
    Ok(Self)
  }
}

/// The inbox handler that forwards RSVP e-mail, authenticated by its token.
#[derive(Debug, Clone, Copy)]
pub struct InboxCaller;

impl FromRequestParts<AppState> for InboxCaller {
  type Rejection = ApiError;

  async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
    crate::services::meetings::authenticate_inbox(state, bearer_token(parts).ok())?;
    Ok(Self)
  }
}

fn is_write(method: &axum::http::Method) -> bool {
  !matches!(
    *method,
    axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS
  )
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
