use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use teamcraft_db::DbError;
use teamcraft_proto::{DependencyName, ErrorBody, ErrorCode};
use tracing::error;

use crate::auth::AuthError;
use crate::dependencies::CircuitOpen;

/// `Retry-After` of a 503 for a backend that is not reachable (issue #743): long enough that
/// clients don't add load while it recovers, short enough that they notice its return soon.
pub const UNAVAILABLE_RETRY_AFTER_SECS: u64 = 30;
/// `Retry-After` of a 503 for a database that failed for a moment (pool timeout, reset).
pub const DATABASE_RETRY_AFTER_SECS: u64 = 5;

/// How long clients wait before they ask again when the object storage failed (an outage of the
/// storage provider, #742). Clients back off further on their own; this keeps a storm of retries
/// off the server and the storage while it recovers.
const STORAGE_RETRY_AFTER_SECS: u64 = 30;

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
  /// An admin tried to delete their own account.
  #[error("admins can't delete their own account")]
  CannotDeleteSelf,
  /// Deleting the account would leave the workspace without an active admin.
  #[error("the workspace must keep at least one active admin")]
  LastAdmin,
  /// Channel members must belong to the channel's space; the user is not in it (yet).
  #[error("the user is not a member of the space")]
  NotSpaceMember,
  /// A guest tried to start a direct message that `guest_dm_start` doesn't allow.
  #[error("guests may not start this direct message")]
  GuestDmNotAllowed,
  /// The admin's last request to join was declined or withdrawn and would not have expired yet.
  #[error("asked to join this channel recently; ask again once that request would have expired")]
  AdminJoinRequestTooSoon,
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
  /// The account already has the most devices one call allows.
  #[error("the account already has {max_devices} devices in this call")]
  CallDeviceLimit { max_devices: usize },
  /// A needed backend (e.g. attachment storage) is not configured or not reachable.
  #[error("unavailable: {0}")]
  Unavailable(String),
  /// A backend is down (its circuit breaker is open, see `dependencies.rs`): 503 with
  /// `Retry-After` until its next probe.
  #[error("{dependency} is unavailable, retry in {retry_after_secs} seconds")]
  DependencyDown {
    dependency: DependencyName,
    retry_after_secs: u64,
  },
  /// The server is at its limit for this kind of work (e.g. image decoding), or the object storage
  /// is down ([`Self::storage_unavailable`]); retry after this many seconds. 503 with `Retry-After`.
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
  /// The object storage failed or answered "unavailable" while it holds `what`: 503 with
  /// `Retry-After`, so clients retry later instead of at once.
  pub fn storage_unavailable(what: &'static str) -> Self {
    Self::Busy {
      what,
      retry_after_secs: STORAGE_RETRY_AFTER_SECS,
    }
  }

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

  /// The admin's request to join is no longer open (decided, withdrawn or expired).
  pub fn admin_join_request_not_found() -> Self {
    Self::NotFound {
      code: ErrorCode::AdminJoinRequestNotFound,
      what: "admin join request",
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
      Self::Auth(
        AuthError::MissingToken
        | AuthError::InvalidToken(_)
        | AuthError::Expired(_)
        | AuthError::SessionEnded
        | AuthError::SessionTimedOut
        | AuthError::AccountDeleted,
      ) => StatusCode::UNAUTHORIZED,
      Self::Auth(AuthError::NoTeamcraftRole | AuthError::AccountDeactivated)
      | Self::Forbidden(_)
      | Self::GuestDmNotAllowed
      | Self::MfaRequired => StatusCode::FORBIDDEN,
      Self::Auth(AuthError::KeysUnavailable(_)) => StatusCode::SERVICE_UNAVAILABLE,
      Self::NotFound { .. } => StatusCode::NOT_FOUND,
      Self::InvalidRequest(_) | Self::InvalidKeyPackage(_) | Self::LastOwner | Self::NotSpaceMember => {
        StatusCode::BAD_REQUEST
      },
      Self::Conflict(_)
      | Self::AdminJoinRequestTooSoon
      | Self::CallDeviceLimit { .. }
      | Self::CannotDeleteSelf
      | Self::LastAdmin
      | Self::KeyPackageExhausted
      | Self::EpochConflict { .. }
      | Self::VersionConflict { .. } => StatusCode::CONFLICT,
      Self::PayloadTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
      Self::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
      Self::Unavailable(_) | Self::Busy { .. } | Self::DependencyDown { .. } => {
        StatusCode::SERVICE_UNAVAILABLE
      },
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
      Self::Auth(AuthError::SessionEnded) => ErrorCode::SessionEnded,
      Self::Auth(AuthError::SessionTimedOut) => ErrorCode::SessionTimedOut,
      Self::Auth(AuthError::NoTeamcraftRole) => ErrorCode::NoTeamcraftRole,
      Self::Auth(AuthError::AccountDeactivated) => ErrorCode::AccountDeactivated,
      Self::Auth(AuthError::AccountDeleted) => ErrorCode::AccountDeleted,
      Self::Forbidden(_) => ErrorCode::Forbidden,
      Self::GuestDmNotAllowed => ErrorCode::GuestDmNotAllowed,
      Self::Auth(AuthError::KeysUnavailable(_)) => ErrorCode::Unavailable,
      Self::NotFound { code, .. } => code.clone(),
      Self::InvalidRequest(_) => ErrorCode::InvalidRequest,
      Self::MfaRequired => ErrorCode::MfaRequired,
      Self::ShareUnavailable => ErrorCode::ShareUnavailable,
      Self::LocalAccountsDisabled => ErrorCode::LocalAccountsDisabled,
      Self::UnknownHost => ErrorCode::UnknownHost,
      Self::Conflict(_) => ErrorCode::Conflict,
      Self::LastOwner => ErrorCode::LastOwner,
      Self::CannotDeleteSelf => ErrorCode::CannotDeleteSelf,
      Self::LastAdmin => ErrorCode::LastAdmin,
      Self::NotSpaceMember => ErrorCode::NotSpaceMember,
      Self::AdminJoinRequestTooSoon => ErrorCode::AdminJoinRequestTooSoon,
      Self::CallDeviceLimit { .. } => ErrorCode::CallDeviceLimit,
      Self::KeyPackageExhausted => ErrorCode::KeyPackageExhausted,
      Self::InvalidKeyPackage(_) => ErrorCode::InvalidKeyPackage,
      Self::EpochConflict { .. } => ErrorCode::EpochConflict,
      Self::VersionConflict { .. } => ErrorCode::VersionConflict,
      Self::PayloadTooLarge(_) => ErrorCode::PayloadTooLarge,
      Self::RateLimited { .. } => ErrorCode::RateLimited,
      Self::Unavailable(_) | Self::Busy { .. } | Self::DependencyDown { .. } => ErrorCode::Unavailable,
      Self::Database(error) if error.is_transient() => ErrorCode::Unavailable,
      Self::Database(_) | Self::Internal(_) => ErrorCode::Internal,
    }
  }

  /// `Retry-After` in seconds. Every 503 carries one (issue #743), so clients back off instead of
  /// retrying at once while a backend recovers.
  pub fn retry_after_secs(&self) -> Option<u64> {
    match self {
      Self::RateLimited { retry_after_secs }
      | Self::Busy { retry_after_secs, .. }
      | Self::DependencyDown { retry_after_secs, .. } => Some(*retry_after_secs),
      Self::Database(error) if error.is_transient() => Some(DATABASE_RETRY_AFTER_SECS),
      _ if self.status() == StatusCode::SERVICE_UNAVAILABLE => Some(UNAVAILABLE_RETRY_AFTER_SECS),
      _ => None,
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
      Self::DependencyDown { dependency, .. } => format!("{dependency} is unavailable, try again later"),
      other => other.to_string(),
    }
  }
}

impl From<CircuitOpen> for ApiError {
  fn from(open: CircuitOpen) -> Self {
    Self::DependencyDown {
      dependency: open.dependency,
      retry_after_secs: open.retry_after.as_secs().max(1),
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
    let maybe_retry_after_secs = self.retry_after_secs();
    if let Some(seconds) = maybe_retry_after_secs {
      body = body.with_retry_after_secs(seconds);
    }
    match &self {
      Self::EpochConflict { current_epoch } => body = body.with_current_epoch(*current_epoch),
      Self::VersionConflict { current_version } => body = body.with_current_version(*current_version),
      _ => {},
    }
    let mut response = (self.status(), Json(body)).into_response();
    if let Some(seconds) = maybe_retry_after_secs {
      response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
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
        AuthError::SessionEnded,
        StatusCode::UNAUTHORIZED,
        ErrorCode::SessionEnded,
      ),
      (
        AuthError::SessionTimedOut,
        StatusCode::UNAUTHORIZED,
        ErrorCode::SessionTimedOut,
      ),
      (
        AuthError::NoTeamcraftRole,
        StatusCode::FORBIDDEN,
        ErrorCode::NoTeamcraftRole,
      ),
      (
        AuthError::AccountDeactivated,
        StatusCode::FORBIDDEN,
        ErrorCode::AccountDeactivated,
      ),
      (
        AuthError::AccountDeleted,
        StatusCode::UNAUTHORIZED,
        ErrorCode::AccountDeleted,
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
  fn a_storage_outage_answers_503_unavailable_with_retry_after() {
    let error = ApiError::storage_unavailable("attachment storage");
    assert_eq!(error.error_code(), ErrorCode::Unavailable);
    let response = error.into_response();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
      response.headers()[header::RETRY_AFTER],
      STORAGE_RETRY_AFTER_SECS.to_string().as_str()
    );
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
        ApiError::GuestDmNotAllowed,
        StatusCode::FORBIDDEN,
        ErrorCode::GuestDmNotAllowed,
      ),
      (
        ApiError::CannotDeleteSelf,
        StatusCode::CONFLICT,
        ErrorCode::CannotDeleteSelf,
      ),
      (ApiError::LastAdmin, StatusCode::CONFLICT, ErrorCode::LastAdmin),
      (
        ApiError::NotSpaceMember,
        StatusCode::BAD_REQUEST,
        ErrorCode::NotSpaceMember,
      ),
      (
        ApiError::AdminJoinRequestTooSoon,
        StatusCode::CONFLICT,
        ErrorCode::AdminJoinRequestTooSoon,
      ),
      (
        ApiError::CallDeviceLimit { max_devices: 3 },
        StatusCode::CONFLICT,
        ErrorCode::CallDeviceLimit,
      ),
      (
        ApiError::admin_join_request_not_found(),
        StatusCode::NOT_FOUND,
        ErrorCode::AdminJoinRequestNotFound,
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
  async fn every_503_carries_retry_after_in_header_and_body() {
    let cases = [
      (
        ApiError::from(CircuitOpen {
          dependency: DependencyName::Sfu,
          retry_after: std::time::Duration::from_secs(12),
        }),
        12,
      ),
      (
        ApiError::Unavailable("attachment storage".into()),
        UNAVAILABLE_RETRY_AFTER_SECS,
      ),
      (
        ApiError::from(AuthError::KeysUnavailable("x".into())),
        UNAVAILABLE_RETRY_AFTER_SECS,
      ),
      (
        ApiError::Busy {
          what: "image decoding",
          retry_after_secs: 2,
        },
        2,
      ),
    ];
    for (error, seconds) in cases {
      assert_eq!(error.status(), StatusCode::SERVICE_UNAVAILABLE, "{error}");
      assert_eq!(error.error_code(), ErrorCode::Unavailable, "{error}");
      let response = error.into_response();
      assert_eq!(
        response.headers()[header::RETRY_AFTER],
        seconds.to_string().as_str()
      );
      let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
      let body: ErrorBody = serde_json::from_slice(&bytes).unwrap();
      assert_eq!(body.maybe_retry_after_secs, Some(seconds));
    }
  }

  #[test]
  fn other_errors_carry_no_retry_after() {
    let response = ApiError::Conflict("x".into()).into_response();
    assert!(response.headers().get(header::RETRY_AFTER).is_none());
    assert_eq!(ApiError::Internal("x".into()).retry_after_secs(), None);
    assert_eq!(
      ApiError::Database(teamcraft_db::DbError::InvalidStoredValue("x".into())).retry_after_secs(),
      None
    );
  }

  #[test]
  fn a_dependency_outage_names_the_dependency_but_no_details() {
    let error = ApiError::from(CircuitOpen {
      dependency: DependencyName::ObjectStorage,
      retry_after: std::time::Duration::from_millis(10),
    });
    assert_eq!(
      error.public_message(),
      "object_storage is unavailable, try again later"
    );
    assert_eq!(error.retry_after_secs(), Some(1), "at least a second");
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
