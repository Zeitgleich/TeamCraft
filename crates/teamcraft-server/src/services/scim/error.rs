//! Errors of the SCIM endpoint, answered as SCIM error bodies (RFC 7644 §3.12) with TeamCraft's
//! stable `error_code` next to `scimType`.

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use teamcraft_proto::scim::{SCIM_MEDIA_TYPE, ScimErrorBody, ScimErrorCode, ScimInputError, ScimType};
use tracing::{error, warn};

use crate::error::{ApiError, DatabaseOutage};

#[derive(Debug, thiserror::Error)]
pub enum ScimError {
  /// No token, a malformed one, or one no client has.
  #[error("invalid or missing SCIM token")]
  InvalidToken,
  #[error("too many requests")]
  RateLimited { retry_after_secs: u64 },
  #[error("no such resource")]
  NotFound,
  #[error("invalid JSON: {0}")]
  InvalidSyntax(String),
  #[error(transparent)]
  Input(#[from] ScimInputError),
  #[error("invalid filter: {0}")]
  InvalidFilter(String),
  /// A filter no index narrows would read more resources than allowed.
  #[error("the filter matches too many resources to read; filter by userName, externalId or displayName")]
  FilterTooBroad,
  #[error("the body is larger than {max_bytes} bytes")]
  BodyTooLarge { max_bytes: usize },
  #[error("{0}")]
  Uniqueness(&'static str),
  #[error("the account belongs to another SCIM client")]
  LinkedElsewhere,
  #[error("an admin deleted the account of this identity; it comes back at its own next sign-in")]
  IdentityReleased,
  #[error("{0}")]
  IdentityImmutable(String),
  #[error("members that are no users of this client: {0}")]
  UnknownMembers(String),
  #[error("a group may have at most {0} members")]
  TooManyMembers(usize),
  #[error("the resource changed: If-Match names another version")]
  VersionMismatch,
  /// Anything the rest of the server answers (database, internal failures).
  #[error(transparent)]
  Api(#[from] ApiError),
}

impl From<teamcraft_db::DbError> for ScimError {
  fn from(error: teamcraft_db::DbError) -> Self {
    Self::Api(ApiError::Database(error))
  }
}

impl ScimError {
  fn parts(&self) -> (StatusCode, Option<ScimType>, ScimErrorCode) {
    match self {
      Self::InvalidToken => (StatusCode::UNAUTHORIZED, None, ScimErrorCode::InvalidToken),
      Self::RateLimited { .. } => (StatusCode::TOO_MANY_REQUESTS, None, ScimErrorCode::RateLimited),
      Self::NotFound => (StatusCode::NOT_FOUND, None, ScimErrorCode::NotFound),
      Self::InvalidSyntax(_) => (
        StatusCode::BAD_REQUEST,
        Some(ScimType::InvalidSyntax),
        ScimErrorCode::InvalidSyntax,
      ),
      Self::Input(input) => input_parts(input),
      Self::InvalidFilter(_) => (
        StatusCode::BAD_REQUEST,
        Some(ScimType::InvalidFilter),
        ScimErrorCode::InvalidFilter,
      ),
      Self::FilterTooBroad | Self::TooManyMembers(_) => (
        StatusCode::BAD_REQUEST,
        Some(ScimType::TooMany),
        ScimErrorCode::TooMany,
      ),
      Self::BodyTooLarge { .. } => (StatusCode::PAYLOAD_TOO_LARGE, None, ScimErrorCode::BodyTooLarge),
      Self::Uniqueness(_) => (
        StatusCode::CONFLICT,
        Some(ScimType::Uniqueness),
        ScimErrorCode::Uniqueness,
      ),
      Self::LinkedElsewhere => (
        StatusCode::CONFLICT,
        Some(ScimType::Uniqueness),
        ScimErrorCode::LinkedElsewhere,
      ),
      Self::IdentityReleased => (
        StatusCode::CONFLICT,
        Some(ScimType::Uniqueness),
        ScimErrorCode::IdentityReleased,
      ),
      Self::IdentityImmutable(_) => (
        StatusCode::BAD_REQUEST,
        Some(ScimType::Mutability),
        ScimErrorCode::IdentityImmutable,
      ),
      Self::UnknownMembers(_) => (
        StatusCode::BAD_REQUEST,
        Some(ScimType::InvalidValue),
        ScimErrorCode::UnknownMember,
      ),
      Self::VersionMismatch => (
        StatusCode::PRECONDITION_FAILED,
        None,
        ScimErrorCode::VersionMismatch,
      ),
      Self::Api(api) => match api.status() {
        StatusCode::SERVICE_UNAVAILABLE => {
          (StatusCode::SERVICE_UNAVAILABLE, None, ScimErrorCode::Unavailable)
        },
        StatusCode::TOO_MANY_REQUESTS => (StatusCode::TOO_MANY_REQUESTS, None, ScimErrorCode::RateLimited),
        StatusCode::NOT_FOUND => (StatusCode::NOT_FOUND, None, ScimErrorCode::NotFound),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, None, ScimErrorCode::Internal),
      },
    }
  }

  fn retry_after_secs(&self) -> Option<u64> {
    match self {
      Self::RateLimited { retry_after_secs } => Some(*retry_after_secs),
      Self::Api(api) => api.retry_after_secs(),
      _ => None,
    }
  }

  /// The detail clients see: never internal error text.
  fn detail(&self) -> String {
    match self {
      Self::Api(api) => api.public_message(),
      other => other.to_string(),
    }
  }
}

fn input_parts(input: &ScimInputError) -> (StatusCode, Option<ScimType>, ScimErrorCode) {
  let bad = StatusCode::BAD_REQUEST;
  match input {
    ScimInputError::NotAnObject | ScimInputError::TooDeep | ScimInputError::UnknownOperation => {
      (bad, Some(ScimType::InvalidSyntax), ScimErrorCode::InvalidSyntax)
    },
    ScimInputError::Missing(_) | ScimInputError::InvalidValue(_) | ScimInputError::TooLong { .. } => {
      (bad, Some(ScimType::InvalidValue), ScimErrorCode::InvalidValue)
    },
    ScimInputError::TooMany(_) => (bad, Some(ScimType::TooMany), ScimErrorCode::TooMany),
    ScimInputError::InvalidPath(_) => (bad, Some(ScimType::InvalidPath), ScimErrorCode::InvalidPath),
    ScimInputError::NoTarget => (bad, Some(ScimType::NoTarget), ScimErrorCode::InvalidPath),
    ScimInputError::InvalidFilter(_) => (bad, Some(ScimType::InvalidFilter), ScimErrorCode::InvalidFilter),
  }
}

impl IntoResponse for ScimError {
  fn into_response(self) -> Response {
    let (status, maybe_scim_type, code) = self.parts();
    if status.is_server_error() {
      if status == StatusCode::SERVICE_UNAVAILABLE {
        warn!(error = %self, "a SCIM request could not be served now");
      } else {
        error!(error = %self, "a SCIM request failed");
      }
    }
    let body = ScimErrorBody::new(status.as_u16(), maybe_scim_type, &code, self.detail());
    let mut response = (status, axum::Json(body)).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(SCIM_MEDIA_TYPE));
    if let Some(seconds) = self.retry_after_secs() {
      headers.insert(header::RETRY_AFTER, HeaderValue::from(seconds));
    }
    if status == StatusCode::UNAUTHORIZED {
      headers.insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    }
    if matches!(&self, Self::Api(ApiError::Database(error)) if error.is_outage()) {
      response.extensions_mut().insert(DatabaseOutage);
    }
    response
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn every_error_has_its_status_and_code() {
    let cases: Vec<(ScimError, u16, &str)> = vec![
      (ScimError::InvalidToken, 401, "invalid_token"),
      (
        ScimError::RateLimited { retry_after_secs: 3 },
        429,
        "rate_limited",
      ),
      (ScimError::NotFound, 404, "scim_not_found"),
      (ScimError::InvalidSyntax("x".into()), 400, "scim_invalid_syntax"),
      (
        ScimError::Input(ScimInputError::Missing("userName")),
        400,
        "scim_invalid_value",
      ),
      (ScimError::Input(ScimInputError::TooMany(1)), 400, "scim_too_many"),
      (
        ScimError::Input(ScimInputError::NoTarget),
        400,
        "scim_invalid_path",
      ),
      (ScimError::InvalidFilter("x".into()), 400, "scim_invalid_filter"),
      (ScimError::FilterTooBroad, 400, "scim_too_many"),
      (
        ScimError::BodyTooLarge { max_bytes: 1 },
        413,
        "scim_body_too_large",
      ),
      (ScimError::Uniqueness("taken"), 409, "scim_uniqueness"),
      (ScimError::LinkedElsewhere, 409, "scim_linked_elsewhere"),
      (ScimError::IdentityReleased, 409, "scim_identity_released"),
      (
        ScimError::IdentityImmutable("x".into()),
        400,
        "scim_identity_immutable",
      ),
      (ScimError::UnknownMembers("1".into()), 400, "scim_unknown_member"),
      (ScimError::TooManyMembers(1), 400, "scim_too_many"),
      (ScimError::VersionMismatch, 412, "scim_version_mismatch"),
      (
        ScimError::Api(ApiError::Internal("secret detail".into())),
        500,
        "internal",
      ),
      (
        ScimError::Api(ApiError::RateLimited { retry_after_secs: 1 }),
        429,
        "rate_limited",
      ),
    ];
    for (error, status, code) in cases {
      let (actual_status, _, actual_code) = error.parts();
      assert_eq!(
        (actual_status.as_u16(), actual_code.as_str()),
        (status, code),
        "{error:?}"
      );
    }
  }

  #[test]
  fn internal_details_are_not_answered() {
    let error = ScimError::Api(ApiError::Internal("secret detail".into()));
    assert!(!error.detail().contains("secret"));
  }
}
