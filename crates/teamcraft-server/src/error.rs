use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use teamcraft_db::DbError;
use teamcraft_proto::{DependencyName, ErrorBody, ErrorCode};
use tracing::error;

use crate::auth::{AuthError, DpopRefusal};
use crate::dependencies::CircuitOpen;
use crate::storage::StorageError;

/// `Retry-After` of a 503 for a backend that is not reachable (issue #743): long enough that
/// clients don't add load while it recovers, short enough that they notice its return soon.
pub const UNAVAILABLE_RETRY_AFTER_SECS: u64 = 30;
/// `Retry-After` of a 503 for a database that failed for a moment (pool timeout, reset).
pub const DATABASE_RETRY_AFTER_SECS: u64 = 5;

/// How long clients wait before they ask again when the object storage failed (an outage of the
/// storage provider, #742). Clients back off further on their own; this keeps a storm of retries
/// off the server and the storage while it recovers.
const STORAGE_RETRY_AFTER_SECS: u64 = 30;

/// `Retry-After` of a 429 for an account at its share of the DPoP replay cache: entries expire over
/// a few minutes, and some room is free again well before all of them have.
const DPOP_USER_LIMIT_RETRY_AFTER_SECS: u64 = 30;

/// In a response's extensions: the answer failed because the database looks down, for the database's
/// breaker around the request (`outage_gate.rs`).
#[derive(Debug, Clone, Copy)]
pub struct DatabaseOutage;

/// In a response's extensions: the 401 only asks for a proof with the current nonce. Everybody
/// meets it after a restart.
#[derive(Debug, Clone, Copy)]
pub struct DpopNonceChallenge;

/// In a response's extensions: the token was valid and the 401 is about its proof or its binding, so
/// nothing was guessed (`AuthError::is_dpop_refusal`). No failed authentication (`security.rs`): with
/// a wrong public origin it would block the whole address, bearer users behind the same NAT included.
#[derive(Debug, Clone, Copy)]
pub struct DpopProofRefusal;

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
  /// The workspace doesn't allow scheduling messages.
  #[error("scheduling messages is turned off in this workspace")]
  ScheduledMessagesDisabled,
  /// The caller holds as many scheduled messages as the workspace allows.
  #[error("at most {max} scheduled messages")]
  ScheduledMessageLimit { max: u32 },
  /// The time of a scheduled message has passed or lies beyond the horizon.
  #[error("invalid time: {0}")]
  ScheduledTimeInvalid(String),
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
  #[error("make another member channel admin first, or delete the channel")]
  LastChannelAdmin,
  #[error("nobody else is in this channel: leaving deletes it")]
  LastMember,
  #[error("an import account holds this channel")]
  ImportInProgress,
  #[error("the space holds private channels you aren't in")]
  SpaceHasHiddenChannels,
  #[error("assign a channel admin before restoring this channel")]
  ChannelNeedsAdmin,
  #[error("not allowed for DMs and group DMs")]
  NotAChannel,
  /// Channel members must belong to the channel's space; the user is not in it (yet).
  #[error("the user is not a member of the space")]
  NotSpaceMember,
  /// The signed policy doesn't name the caller as the importer, or the import period is over.
  #[error("this account may not import")]
  ImportNotAllowed,
  /// No ID is left for imported messages below the conversation's first live message.
  #[error("no room for imported messages before the conversation's first message")]
  ImportWindowFull,
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
  /// The signed policy lets members protect their key backup only with a recovery kit: a new or
  /// changed recovery passphrase is refused (e2ee.md §6.5.8).
  #[error("the workspace policy allows only a recovery kit, no new recovery passphrase")]
  RecoveryPassphraseNotAllowed,
  /// The signed policy does not let members replace the name from the identity provider.
  #[error("the workspace policy does not allow changing the display name")]
  DisplayNameOverrideNotAllowed,
  /// An invitation for the address is open in the space already (one per address and space).
  #[error("an invitation for this address is open; revoke it first")]
  InviteAlreadyOpen,
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

  /// What the client hears when a storage call failed: until the next probe when the breaker is
  /// open, else [`Self::storage_unavailable`]. Never the storage's own answer.
  pub fn storage_failed(what: &'static str, error: StorageError) -> Self {
    match error {
      StorageError::Unavailable(open) => open.into(),
      _ => Self::storage_unavailable(what),
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

  pub fn meeting_admitters_not_found() -> Self {
    Self::NotFound {
      code: ErrorCode::MeetingAdmittersNotFound,
      what: "meeting admitters",
    }
  }

  pub fn scheduled_message_not_found() -> Self {
    Self::NotFound {
      code: ErrorCode::ScheduledMessageNotFound,
      what: "scheduled message",
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

  /// See [`DpopNonceChallenge`].
  pub fn is_dpop_nonce_challenge(&self) -> bool {
    matches!(self, Self::Auth(AuthError::Dpop(DpopRefusal::UseNonce)))
  }

  pub fn status(&self) -> StatusCode {
    match self {
      Self::Auth(
        AuthError::MissingToken
        | AuthError::InvalidToken(_)
        | AuthError::Expired(_)
        | AuthError::SessionEnded
        | AuthError::SessionTimedOut
        | AuthError::AccountDeleted
        | AuthError::Dpop(_),
      ) => StatusCode::UNAUTHORIZED,
      Self::Auth(AuthError::NoTeamcraftRole | AuthError::AccountDeactivated)
      | Self::Forbidden(_)
      | Self::ScheduledMessagesDisabled
      | Self::GuestDmNotAllowed
      | Self::ImportNotAllowed
      | Self::RecoveryPassphraseNotAllowed
      | Self::DisplayNameOverrideNotAllowed
      | Self::MfaRequired => StatusCode::FORBIDDEN,
      Self::Auth(AuthError::KeysUnavailable(_) | AuthError::DpopReplayCacheFull) => {
        StatusCode::SERVICE_UNAVAILABLE
      },
      Self::Auth(AuthError::DpopUserLimit) => StatusCode::TOO_MANY_REQUESTS,
      Self::NotFound { .. } => StatusCode::NOT_FOUND,
      Self::NotAChannel
      | Self::InvalidRequest(_)
      | Self::ScheduledTimeInvalid(_)
      | Self::InvalidKeyPackage(_)
      | Self::LastOwner
      | Self::NotSpaceMember => StatusCode::BAD_REQUEST,
      Self::Conflict(_)
      | Self::AdminJoinRequestTooSoon
      | Self::CallDeviceLimit { .. }
      | Self::CannotDeleteSelf
      | Self::LastAdmin
      | Self::LastChannelAdmin
      | Self::LastMember
      | Self::ImportInProgress
      | Self::SpaceHasHiddenChannels
      | Self::ChannelNeedsAdmin
      | Self::ImportWindowFull
      | Self::KeyPackageExhausted
      | Self::ScheduledMessageLimit { .. }
      | Self::EpochConflict { .. }
      | Self::VersionConflict { .. }
      | Self::InviteAlreadyOpen => StatusCode::CONFLICT,
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
      Self::ImportNotAllowed => ErrorCode::ImportNotAllowed,
      Self::ImportWindowFull => ErrorCode::ImportWindowFull,
      Self::Auth(AuthError::KeysUnavailable(_) | AuthError::DpopReplayCacheFull) => ErrorCode::Unavailable,
      Self::Auth(AuthError::DpopUserLimit) => ErrorCode::RateLimited,
      Self::Auth(AuthError::Dpop(refusal)) => refusal.error_code(),
      Self::NotFound { code, .. } => code.clone(),
      Self::InvalidRequest(_) => ErrorCode::InvalidRequest,
      Self::MfaRequired => ErrorCode::MfaRequired,
      Self::ShareUnavailable => ErrorCode::ShareUnavailable,
      Self::LocalAccountsDisabled => ErrorCode::LocalAccountsDisabled,
      Self::RecoveryPassphraseNotAllowed => ErrorCode::RecoveryPassphraseNotAllowed,
      Self::DisplayNameOverrideNotAllowed => ErrorCode::DisplayNameOverrideNotAllowed,
      Self::InviteAlreadyOpen => ErrorCode::InviteAlreadyOpen,
      Self::UnknownHost => ErrorCode::UnknownHost,
      Self::Conflict(_) => ErrorCode::Conflict,
      Self::LastOwner => ErrorCode::LastOwner,
      Self::CannotDeleteSelf => ErrorCode::CannotDeleteSelf,
      Self::LastAdmin => ErrorCode::LastAdmin,
      Self::LastChannelAdmin => ErrorCode::LastChannelAdmin,
      Self::LastMember => ErrorCode::LastMember,
      Self::ImportInProgress => ErrorCode::ImportInProgress,
      Self::SpaceHasHiddenChannels => ErrorCode::SpaceHasHiddenChannels,
      Self::ChannelNeedsAdmin => ErrorCode::ChannelNeedsAdmin,
      Self::NotAChannel => ErrorCode::NotAChannel,
      Self::NotSpaceMember => ErrorCode::NotSpaceMember,
      Self::AdminJoinRequestTooSoon => ErrorCode::AdminJoinRequestTooSoon,
      Self::CallDeviceLimit { .. } => ErrorCode::CallDeviceLimit,
      Self::KeyPackageExhausted => ErrorCode::KeyPackageExhausted,
      Self::InvalidKeyPackage(_) => ErrorCode::InvalidKeyPackage,
      Self::ScheduledMessagesDisabled => ErrorCode::ScheduledMessagesDisabled,
      Self::ScheduledMessageLimit { .. } => ErrorCode::ScheduledMessageLimit,
      Self::ScheduledTimeInvalid(_) => ErrorCode::ScheduledTimeInvalid,
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
      Self::Auth(AuthError::DpopUserLimit) => Some(DPOP_USER_LIMIT_RETRY_AFTER_SECS),
      Self::Database(error) if error.is_transient() => Some(DATABASE_RETRY_AFTER_SECS),
      _ if self.status() == StatusCode::SERVICE_UNAVAILABLE => Some(UNAVAILABLE_RETRY_AFTER_SECS),
      _ => None,
    }
  }

  /// The body clients see, over HTTP and in the gateway's error frame.
  pub fn body(&self) -> ErrorBody {
    let mut body = ErrorBody::new(self.error_code(), self.public_message());
    if let Some(seconds) = self.retry_after_secs() {
      body = body.with_retry_after_secs(seconds);
    }
    match self {
      Self::EpochConflict { current_epoch } => body.with_current_epoch(*current_epoch),
      Self::VersionConflict { current_version } => body.with_current_version(*current_version),
      _ => body,
    }
  }

  /// Client-facing text. Internal details stay in the logs.
  pub fn public_message(&self) -> String {
    match self {
      Self::Auth(AuthError::KeysUnavailable(_) | AuthError::DpopReplayCacheFull) => {
        "authentication is temporarily unavailable".into()
      },
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
    // An open breaker rejects many requests at once, and said so once when it opened.
    if matches!(
      self.status(),
      StatusCode::INTERNAL_SERVER_ERROR | StatusCode::SERVICE_UNAVAILABLE
    ) && !matches!(
      self,
      Self::DependencyDown { .. } | Self::Auth(AuthError::DpopReplayCacheFull)
    ) {
      error!(error = %self, "request failed");
    }
    let mut response = (self.status(), Json(self.body())).into_response();
    if let Some(seconds) = self.retry_after_secs() {
      response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
    }
    if matches!(&self, Self::Database(error) if error.is_outage()) {
      response.extensions_mut().insert(DatabaseOutage);
    }
    if self.status() == StatusCode::UNAUTHORIZED {
      let challenge = match &self {
        Self::Auth(AuthError::Dpop(refusal)) => refusal.challenge(),
        _ => HeaderValue::from_static("Bearer"),
      };
      response.headers_mut().insert(header::WWW_AUTHENTICATE, challenge);
    }
    if self.is_dpop_nonce_challenge() {
      response.extensions_mut().insert(DpopNonceChallenge);
    }
    if matches!(&self, Self::Auth(auth) if auth.is_dpop_refusal()) {
      response.extensions_mut().insert(DpopProofRefusal);
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
  fn a_storage_failure_names_the_breaker_s_time_when_it_is_open() {
    let open = ApiError::storage_failed(
      "picture storage",
      StorageError::Unavailable(CircuitOpen {
        dependency: DependencyName::ObjectStorage,
        retry_after: std::time::Duration::from_secs(7),
      }),
    );
    assert_eq!(open.retry_after_secs(), Some(7));
    let failed = ApiError::storage_failed("picture storage", StorageError::TooLarge);
    assert_eq!(failed.retry_after_secs(), Some(STORAGE_RETRY_AFTER_SECS));
    assert_eq!(failed.status(), StatusCode::SERVICE_UNAVAILABLE);
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
        ApiError::ImportNotAllowed,
        StatusCode::FORBIDDEN,
        ErrorCode::ImportNotAllowed,
      ),
      (
        ApiError::ImportWindowFull,
        StatusCode::CONFLICT,
        ErrorCode::ImportWindowFull,
      ),
      (
        ApiError::RecoveryPassphraseNotAllowed,
        StatusCode::FORBIDDEN,
        ErrorCode::RecoveryPassphraseNotAllowed,
      ),
      (
        ApiError::DisplayNameOverrideNotAllowed,
        StatusCode::FORBIDDEN,
        ErrorCode::DisplayNameOverrideNotAllowed,
      ),
      (
        ApiError::InviteAlreadyOpen,
        StatusCode::CONFLICT,
        ErrorCode::InviteAlreadyOpen,
      ),
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

  /// The gateway sends the same body: a client that can't connect waits as it would over HTTP.
  #[test]
  fn the_body_for_the_gateway_carries_retry_after() {
    let body = ApiError::Database(teamcraft_db::DbError::Query(refused())).body();
    assert_eq!(body.error_code, ErrorCode::Unavailable);
    assert_eq!(body.maybe_retry_after_secs, Some(DATABASE_RETRY_AFTER_SECS));
  }

  /// Only an outage feeds the database's breaker: a lost race between transactions is not one.
  #[test]
  fn only_a_database_outage_is_marked_for_the_breaker() {
    let marked = |error: ApiError| {
      error
        .into_response()
        .extensions()
        .get::<DatabaseOutage>()
        .is_some()
    };
    let database = |error| ApiError::Database(teamcraft_db::DbError::Query(error));
    assert!(marked(database(refused())));
    assert!(marked(database(sqlx::Error::PoolTimedOut)));
    assert!(!marked(database(sqlx::Error::RowNotFound)));
    assert!(!marked(ApiError::Conflict("x".into())));
  }

  fn refused() -> sqlx::Error {
    sqlx::Error::Io(std::io::Error::from(std::io::ErrorKind::ConnectionRefused))
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

  #[test]
  fn scheduled_message_errors_have_their_status_and_code() {
    for (error, status, code) in [
      (
        ApiError::scheduled_message_not_found(),
        StatusCode::NOT_FOUND,
        ErrorCode::ScheduledMessageNotFound,
      ),
      (
        ApiError::ScheduledMessagesDisabled,
        StatusCode::FORBIDDEN,
        ErrorCode::ScheduledMessagesDisabled,
      ),
      (
        ApiError::ScheduledMessageLimit { max: 3 },
        StatusCode::CONFLICT,
        ErrorCode::ScheduledMessageLimit,
      ),
      (
        ApiError::ScheduledTimeInvalid("x".into()),
        StatusCode::BAD_REQUEST,
        ErrorCode::ScheduledTimeInvalid,
      ),
    ] {
      assert_eq!(error.status(), status, "{error}");
      assert_eq!(error.error_code(), code, "{error}");
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
