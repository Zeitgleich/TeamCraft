use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use teamcraft_db::DbError;
use teamcraft_proto::{ErrorBody, ErrorCode};
use tracing::error;

use crate::auth::AuthError;

/// Every error a handler can return; maps to a status code and the stable `error_code` body.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
  #[error(transparent)]
  Auth(#[from] AuthError),
  #[error(transparent)]
  Database(#[from] DbError),
  #[error("internal error: {0}")]
  Internal(String),
}

impl ApiError {
  pub fn status(&self) -> StatusCode {
    match self {
      Self::Auth(AuthError::MissingToken | AuthError::InvalidToken(_)) => StatusCode::UNAUTHORIZED,
      Self::Auth(AuthError::NoTeamcraftRole | AuthError::AccountDeactivated) => StatusCode::FORBIDDEN,
      Self::Auth(AuthError::KeysUnavailable(_)) => StatusCode::SERVICE_UNAVAILABLE,
      Self::Database(_) | Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
  }

  pub fn error_code(&self) -> ErrorCode {
    match self {
      Self::Auth(AuthError::MissingToken) => ErrorCode::Unauthorized,
      Self::Auth(AuthError::InvalidToken(_)) => ErrorCode::InvalidToken,
      Self::Auth(AuthError::NoTeamcraftRole) => ErrorCode::NoTeamcraftRole,
      Self::Auth(AuthError::AccountDeactivated) => ErrorCode::Forbidden,
      Self::Auth(AuthError::KeysUnavailable(_)) => ErrorCode::Unavailable,
      Self::Database(_) | Self::Internal(_) => ErrorCode::Internal,
    }
  }

  /// Client-facing text. Internal details stay in the logs.
  fn public_message(&self) -> String {
    match self {
      Self::Auth(AuthError::KeysUnavailable(_)) => "authentication is temporarily unavailable".into(),
      Self::Database(_) | Self::Internal(_) => "internal server error".into(),
      Self::Auth(other) => other.to_string(),
    }
  }
}

impl IntoResponse for ApiError {
  fn into_response(self) -> Response {
    if matches!(
      self.status(),
      StatusCode::INTERNAL_SERVER_ERROR | StatusCode::SERVICE_UNAVAILABLE
    ) {
      error!(error = %self, "request failed");
    }
    let body = ErrorBody::new(self.error_code(), self.public_message());
    let mut response = (self.status(), Json(body)).into_response();
    if self.status() == StatusCode::UNAUTHORIZED {
      response.headers_mut().insert(
        axum::http::header::WWW_AUTHENTICATE,
        axum::http::HeaderValue::from_static("Bearer"),
      );
    }
    response
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn auth_errors_map_to_status_and_code() {
    let cases = [
      (
        AuthError::MissingToken,
        StatusCode::UNAUTHORIZED,
        ErrorCode::Unauthorized,
      ),
      (
        AuthError::InvalidToken("x".into()),
        StatusCode::UNAUTHORIZED,
        ErrorCode::InvalidToken,
      ),
      (
        AuthError::NoTeamcraftRole,
        StatusCode::FORBIDDEN,
        ErrorCode::NoTeamcraftRole,
      ),
      (
        AuthError::AccountDeactivated,
        StatusCode::FORBIDDEN,
        ErrorCode::Forbidden,
      ),
      (
        AuthError::KeysUnavailable("x".into()),
        StatusCode::SERVICE_UNAVAILABLE,
        ErrorCode::Unavailable,
      ),
    ];
    for (auth, status, code) in cases {
      let error = ApiError::from(auth);
      assert_eq!((error.status(), error.error_code()), (status, code));
    }
  }

  #[test]
  fn internal_details_are_not_exposed() {
    let error = ApiError::Internal("password=secret".into());
    assert_eq!(error.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(error.public_message(), "internal server error");
    let unavailable = ApiError::from(AuthError::KeysUnavailable("http://internal-host/jwks".into()));
    assert!(!unavailable.public_message().contains("internal-host"));
  }

  #[test]
  fn unauthorized_responses_carry_www_authenticate() {
    let response = ApiError::from(AuthError::MissingToken).into_response();
    assert_eq!(response.headers().get("www-authenticate").unwrap(), "Bearer");
  }
}
