use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
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
  /// The thing doesn't exist or the caller may not know that it exists.
  #[error("{what} not found")]
  NotFound { code: ErrorCode, what: &'static str },
  #[error("forbidden: {0}")]
  Forbidden(String),
  #[error("invalid request: {0}")]
  InvalidRequest(String),
  #[error("conflict: {0}")]
  Conflict(String),
  /// The MLS group left the epoch the request was created for (first commit wins).
  #[error("the group is in epoch {current_epoch}; fetch the backlog and rebase")]
  EpochConflict { current_epoch: u64 },
  /// Compare-and-swap on a versioned object failed.
  #[error("the stored version is {current_version}, not the expected one")]
  VersionConflict { current_version: u64 },
  /// A roster without an owner: nobody could ever sign a roster or policy again (admin.md §2.3).
  #[error("the roster must keep at least one owner")]
  LastOwner,
  /// Channel members must belong to the channel's space; the user is not in it (yet).
  #[error("the user is not a member of the space")]
  NotSpaceMember,
  #[error("the user has no key package left")]
  KeyPackageExhausted,
  /// An uploaded key package doesn't parse, isn't validly signed, or is (nearly) expired.
  #[error("invalid key package: {0}")]
  InvalidKeyPackage(String),
  #[error("payload too large: {0}")]
  PayloadTooLarge(String),
  /// Too many requests; retry after this many seconds.
  #[error("too many requests, retry in {retry_after_secs} seconds")]
  RateLimited { retry_after_secs: u64 },
  /// A needed backend (e.g. attachment storage) is not configured or not reachable.
  #[error("unavailable: {0}")]
  Unavailable(String),
  /// The server is at its limit for this kind of work (e.g. image decoding); retry after this many
  /// seconds. 503 with `Retry-After`.
  #[error("busy: {what}, retry in {retry_after_secs} seconds")]
  Busy {
    what: &'static str,
    retry_after_secs: u64,
  },
  /// The workspace policy requires a second factor and the sign-in had none.
  #[error("multi-factor authentication is required")]
  MfaRequired,
  /// A share link that is unknown, revoked, expired or used up (one answer for all, so links cannot be probed).
  #[error("this link is not available")]
  ShareUnavailable,
  /// An admin switched local accounts off: no sign-in with e-mail and password.
  #[error("sign-in with e-mail and password is turned off on this server")]
  LocalAccountsDisabled,
  /// The request arrived on a host name no brand is configured for and `unknown_host = "reject"`.
  #[error("this host name is not served here")]
  UnknownHost,
  #[error("internal error: {0}")]
  Internal(String),
}

impl ApiError {
  pub fn space_not_found() -> Self {
    Self::NotFound {
      code: ErrorCode::SpaceNotFound,
      what: "space",
    }
  }

  pub fn space_image_not_found() -> Self {
    Self::NotFound {
      code: ErrorCode::SpaceImageNotFound,
      what: "space image",
    }
  }

  pub fn channel_image_not_found() -> Self {
    Self::NotFound {
      code: ErrorCode::ChannelImageNotFound,
      what: "channel image",
    }
  }

  pub fn conversation_not_found() -> Self {
    Self::NotFound {
      code: ErrorCode::ChannelNotFound,
      what: "conversation",
    }
  }

  pub fn user_not_found() -> Self {
    Self::NotFound {
      code: ErrorCode::UserNotFound,
      what: "user",
    }
  }

  pub fn device_not_found() -> Self {
    Self::NotFound {
      code: ErrorCode::DeviceNotFound,
      what: "device",
    }
  }

  pub fn device_link_not_found() -> Self {
    Self::NotFound {
      code: ErrorCode::DeviceLinkNotFound,
      what: "link request",
    }
  }

  pub fn category_not_found() -> Self {
    Self::NotFound {
      code: ErrorCode::CategoryNotFound,
      what: "category",
    }
  }

  pub fn invite_not_found() -> Self {
    Self::NotFound {
      code: ErrorCode::InviteNotFound,
      what: "invite",
    }
  }

  pub fn background_not_found() -> Self {
    Self::NotFound {
      code: ErrorCode::BackgroundNotFound,
      what: "background",
    }
  }

  pub fn attachment_not_found() -> Self {
    Self::NotFound {
      code: ErrorCode::AttachmentNotFound,
      what: "attachment",
    }
  }

  pub fn call_not_found() -> Self {
    Self::NotFound {
      code: ErrorCode::CallNotFound,
      what: "call",
    }
  }

  pub fn bot_not_found() -> Self {
    Self::NotFound {
      code: ErrorCode::BotNotFound,
      what: "bot",
    }
  }

  pub fn recovery_request_not_found() -> Self {
    Self::NotFound {
      code: ErrorCode::RecoveryRequestNotFound,
      what: "recovery request",
    }
  }

  pub fn announcement_not_found() -> Self {
    Self::NotFound {
      code: ErrorCode::AnnouncementNotFound,
      what: "announcement",
    }
  }

  pub fn avatar_not_found() -> Self {
    Self::NotFound {
      code: ErrorCode::AvatarNotFound,
      what: "avatar",
    }
  }

  pub fn user_group_not_found() -> Self {
    Self::NotFound {
      code: ErrorCode::UserGroupNotFound,
      what: "user group",
    }
  }

  pub fn meeting_not_found() -> Self {
    Self::NotFound {
      code: ErrorCode::MeetingNotFound,
      what: "meeting",
    }
  }

  pub fn key_bag_not_found() -> Self {
    Self::NotFound {
      code: ErrorCode::KeyBagNotFound,
      what: "key bag",
    }
  }

  pub fn device_list_not_found() -> Self {
    Self::NotFound {
      code: ErrorCode::DeviceListNotFound,
      what: "device list",
    }
  }

  pub fn identity_key_not_found() -> Self {
    Self::NotFound {
      code: ErrorCode::IdentityKeyNotFound,
      what: "identity key",
    }
  }

  pub fn brand_not_found() -> Self {
    Self::NotFound {
      code: ErrorCode::BrandNotFound,
      what: "brand",
    }
  }

  pub fn brand_asset_not_found() -> Self {
    Self::NotFound {
      code: ErrorCode::BrandAssetNotFound,
      what: "brand asset",
    }
  }

  pub fn status(&self) -> StatusCode {
    match self {
      Self::Auth(AuthError::MissingToken | AuthError::InvalidToken(_) | AuthError::Expired(_)) => {
        StatusCode::UNAUTHORIZED
      },
      Self::Auth(AuthError::NoTeamcraftRole | AuthError::AccountDeactivated)
      | Self::Forbidden(_)
      | Self::MfaRequired => StatusCode::FORBIDDEN,
      Self::Auth(AuthError::KeysUnavailable(_)) => StatusCode::SERVICE_UNAVAILABLE,
      Self::NotFound { .. } => StatusCode::NOT_FOUND,
      Self::InvalidRequest(_) | Self::InvalidKeyPackage(_) | Self::LastOwner | Self::NotSpaceMember => {
        StatusCode::BAD_REQUEST
      },
      Self::Conflict(_)
      | Self::KeyPackageExhausted
      | Self::EpochConflict { .. }
      | Self::VersionConflict { .. } => StatusCode::CONFLICT,
      Self::PayloadTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
      Self::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
      Self::Unavailable(_) | Self::Busy { .. } => StatusCode::SERVICE_UNAVAILABLE,
      Self::Database(error) if error.is_transient() => StatusCode::SERVICE_UNAVAILABLE,
      Self::ShareUnavailable => StatusCode::GONE,
      Self::LocalAccountsDisabled => StatusCode::FORBIDDEN,
      Self::UnknownHost => StatusCode::MISDIRECTED_REQUEST,
      Self::Database(_) | Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
  }

  pub fn error_code(&self) -> ErrorCode {
    match self {
      Self::Auth(AuthError::MissingToken) => ErrorCode::Unauthorized,
      Self::Auth(AuthError::InvalidToken(_) | AuthError::Expired(_)) => ErrorCode::InvalidToken,
      Self::Auth(AuthError::NoTeamcraftRole) => ErrorCode::NoTeamcraftRole,
      Self::Auth(AuthError::AccountDeactivated) | Self::Forbidden(_) => ErrorCode::Forbidden,
      Self::Auth(AuthError::KeysUnavailable(_)) => ErrorCode::Unavailable,
      Self::NotFound { code, .. } => code.clone(),
      Self::InvalidRequest(_) => ErrorCode::InvalidRequest,
      Self::MfaRequired => ErrorCode::MfaRequired,
      Self::ShareUnavailable => ErrorCode::ShareUnavailable,
      Self::LocalAccountsDisabled => ErrorCode::LocalAccountsDisabled,
      Self::UnknownHost => ErrorCode::UnknownHost,
      Self::Conflict(_) => ErrorCode::Conflict,
      Self::LastOwner => ErrorCode::LastOwner,
      Self::NotSpaceMember => ErrorCode::NotSpaceMember,
      Self::KeyPackageExhausted => ErrorCode::KeyPackageExhausted,
      Self::InvalidKeyPackage(_) => ErrorCode::InvalidKeyPackage,
      Self::EpochConflict { .. } => ErrorCode::EpochConflict,
      Self::VersionConflict { .. } => ErrorCode::VersionConflict,
      Self::PayloadTooLarge(_) => ErrorCode::PayloadTooLarge,
      Self::RateLimited { .. } => ErrorCode::RateLimited,
      Self::Unavailable(_) | Self::Busy { .. } => ErrorCode::Unavailable,
      Self::Database(error) if error.is_transient() => ErrorCode::Unavailable,
      Self::Database(_) | Self::Internal(_) => ErrorCode::Internal,
    }
  }

  /// Client-facing text. Internal details stay in the logs.
  pub fn public_message(&self) -> String {
    match self {
      Self::Auth(AuthError::KeysUnavailable(_)) => "authentication is temporarily unavailable".into(),
      Self::Database(error) if error.is_transient() => "the service is busy, try again".into(),
      Self::Database(_) | Self::Internal(_) => "internal server error".into(),
      Self::Unavailable(_) => "the service is not available".into(),
      Self::Busy { .. } => "the service is busy, try again".into(),
      other => other.to_string(),
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
    let mut body = ErrorBody::new(self.error_code(), self.public_message());
    match &self {
      Self::EpochConflict { current_epoch } => body = body.with_current_epoch(*current_epoch),
      Self::VersionConflict { current_version } => body = body.with_current_version(*current_version),
      _ => {},
    }
    let mut response = (self.status(), Json(body)).into_response();
    if let Self::RateLimited { retry_after_secs } | Self::Busy { retry_after_secs, .. } = &self {
      response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from(*retry_after_secs));
    }
    if self.status() == StatusCode::UNAUTHORIZED {
      response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
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
        AuthError::Expired("x".into()),
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
  fn a_busy_server_answers_503_with_retry_after() {
    let response = ApiError::Busy {
      what: "image decoding",
      retry_after_secs: 2,
    }
    .into_response();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()[header::RETRY_AFTER], "2");
  }

  #[test]
  fn domain_errors_map_to_status_and_code() {
    let cases = [
      (
        ApiError::space_image_not_found(),
        StatusCode::NOT_FOUND,
        ErrorCode::SpaceImageNotFound,
      ),
      (
        ApiError::channel_image_not_found(),
        StatusCode::NOT_FOUND,
        ErrorCode::ChannelImageNotFound,
      ),
      (
        ApiError::InvalidKeyPackage("expired".into()),
        StatusCode::BAD_REQUEST,
        ErrorCode::InvalidKeyPackage,
      ),
      (
        ApiError::UnknownHost,
        StatusCode::MISDIRECTED_REQUEST,
        ErrorCode::UnknownHost,
      ),
      (
        ApiError::space_not_found(),
        StatusCode::NOT_FOUND,
        ErrorCode::SpaceNotFound,
      ),
      (
        ApiError::conversation_not_found(),
        StatusCode::NOT_FOUND,
        ErrorCode::ChannelNotFound,
      ),
      (
        ApiError::user_not_found(),
        StatusCode::NOT_FOUND,
        ErrorCode::UserNotFound,
      ),
      (
        ApiError::device_not_found(),
        StatusCode::NOT_FOUND,
        ErrorCode::DeviceNotFound,
      ),
      (
        ApiError::category_not_found(),
        StatusCode::NOT_FOUND,
        ErrorCode::CategoryNotFound,
      ),
      (
        ApiError::invite_not_found(),
        StatusCode::NOT_FOUND,
        ErrorCode::InviteNotFound,
      ),
      (
        ApiError::Forbidden("x".into()),
        StatusCode::FORBIDDEN,
        ErrorCode::Forbidden,
      ),
      (
        ApiError::InvalidRequest("x".into()),
        StatusCode::BAD_REQUEST,
        ErrorCode::InvalidRequest,
      ),
      (
        ApiError::Conflict("x".into()),
        StatusCode::CONFLICT,
        ErrorCode::Conflict,
      ),
      (ApiError::LastOwner, StatusCode::BAD_REQUEST, ErrorCode::LastOwner),
      (
        ApiError::NotSpaceMember,
        StatusCode::BAD_REQUEST,
        ErrorCode::NotSpaceMember,
      ),
      (
        ApiError::PayloadTooLarge("x".into()),
        StatusCode::PAYLOAD_TOO_LARGE,
        ErrorCode::PayloadTooLarge,
      ),
      (
        ApiError::Internal("x".into()),
        StatusCode::INTERNAL_SERVER_ERROR,
        ErrorCode::Internal,
      ),
      (
        ApiError::Database(teamcraft_db::DbError::InvalidStoredValue("x".into())),
        StatusCode::INTERNAL_SERVER_ERROR,
        ErrorCode::Internal,
      ),
    ];
    for (error, status, code) in cases {
      assert_eq!((error.status(), error.error_code()), (status, code), "{error}");
    }
  }

  #[tokio::test]
  async fn conflicts_expose_the_current_epoch_and_version_as_fields() {
    async fn body_of(error: ApiError) -> ErrorBody {
      let response = error.into_response();
      let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
      serde_json::from_slice(&bytes).unwrap()
    }
    let epoch = body_of(ApiError::EpochConflict { current_epoch: 4 }).await;
    assert_eq!(
      (epoch.maybe_current_epoch, epoch.maybe_current_version),
      (Some(4), None)
    );
    let version = body_of(ApiError::VersionConflict { current_version: 9 }).await;
    assert_eq!(
      (version.maybe_current_epoch, version.maybe_current_version),
      (None, Some(9))
    );
    let other = body_of(ApiError::Conflict("x".into())).await;
    assert_eq!(
      (other.maybe_current_epoch, other.maybe_current_version),
      (None, None)
    );
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
