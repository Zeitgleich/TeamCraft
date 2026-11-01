use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;

wire_enum! {
  /// Stable machine-readable error identifiers. Values are part of the API and never change.
  pub enum ErrorCode {
    InvalidRequest => "invalid_request",
    Unauthorized => "unauthorized",
    InvalidToken => "invalid_token",
    /// The session was ended before its token ran out (signed out at the identity provider or on
    /// another device). Like `invalid_token`, but the client can say why.
    SessionEnded => "session_ended",
    /// The session policy's idle timeout or longest session ran out.
    SessionTimedOut => "session_timed_out",
    NoTeamcraftRole => "no_teamcraft_role",
    /// The account is deactivated (suspended by an admin or the identity provider); signing in is
    /// refused until it is reactivated.
    AccountDeactivated => "account_deactivated",
    /// The token comes from a sign-in before the account was deleted (admin.md §3.2.1); signing
    /// in again creates a new account.
    AccountDeleted => "account_deleted",
    Forbidden => "forbidden",
    UserNotFound => "user_not_found",
    SpaceNotFound => "space_not_found",
    ChannelNotFound => "channel_not_found",
    DeviceNotFound => "device_not_found",
    DeviceLinkNotFound => "device_link_not_found",
    CategoryNotFound => "category_not_found",
    InviteNotFound => "invite_not_found",
    IdentityKeyNotFound => "identity_key_not_found",
    KeyBagNotFound => "key_bag_not_found",
    DeviceListNotFound => "device_list_not_found",
    /// No member published the MLS group information for the group's current epoch yet.
    MlsGroupInfoNotFound => "mls_group_info_not_found",
    AttachmentNotFound => "attachment_not_found",
    BackgroundNotFound => "background_not_found",
    CallNotFound => "call_not_found",
    MeetingNotFound => "meeting_not_found",
    BotNotFound => "bot_not_found",
    AvatarNotFound => "avatar_not_found",
    SpaceImageNotFound => "space_image_not_found",
    ChannelImageNotFound => "channel_image_not_found",
    AnnouncementNotFound => "announcement_not_found",
    RecoveryRequestNotFound => "recovery_request_not_found",
    /// The admin's request to join a private channel is no longer open: decided (on another
    /// device or by another owner), withdrawn or expired (admin.md §2.4).
    AdminJoinRequestNotFound => "admin_join_request_not_found",
    /// The admin's last request to join the channel was declined or withdrawn; they can ask again
    /// once it would have expired (admin.md §2.4.1).
    AdminJoinRequestTooSoon => "admin_join_request_too_soon",
    ShareUnavailable => "share_unavailable",
    LocalAccountsDisabled => "local_accounts_disabled",
    /// The signed workspace policy lets members protect their key backup only with a recovery kit:
    /// a new or changed recovery passphrase is refused (e2ee.md §6.5.8).
    RecoveryPassphraseNotAllowed => "recovery_passphrase_not_allowed",
    /// The signed workspace policy does not let members replace the name from the identity provider
    /// (`directory.allow_display_name_override`).
    DisplayNameOverrideNotAllowed => "display_name_override_not_allowed",
    /// No OpenRouter API key is stored for the notetaker (admin.md §3.7).
    NotetakerKeyNotFound => "notetaker_key_not_found",
    UserGroupNotFound => "user_group_not_found",
    MfaRequired => "mfa_required",
    Conflict => "conflict",
    EpochConflict => "epoch_conflict",
    VersionConflict => "version_conflict",
    /// A roster change would leave the workspace without an owner (admin.md §2.3).
    LastOwner => "last_owner",
    /// An admin can't delete their own account.
    CannotDeleteSelf => "cannot_delete_self",
    /// Deleting the account would leave the workspace without an active admin.
    LastAdmin => "last_admin",
    /// A guest tried to start a direct message the instance policy doesn't allow them to start
    /// (`guest_dm_start`, PLAN.md §12). Only given to guests about people they can already see.
    GuestDmNotAllowed => "guest_dm_not_allowed",
    /// A channel member must belong to the channel's space first (invite them to the space).
    NotSpaceMember => "not_space_member",
    KeyPackageExhausted => "key_package_exhausted",
    PayloadTooLarge => "payload_too_large",
    RateLimited => "rate_limited",
    InvalidJoinToken => "invalid_join_token",
    AlreadyJoined => "already_joined",
    /// The meeting host locked the call: no new participants.
    CallLocked => "call_locked",
    /// The meeting host removed this user from the call.
    RemovedFromCall => "removed_from_call",
    /// The account already has the most devices one call allows (`max_devices_per_user_per_call`).
    CallDeviceLimit => "call_device_limit",
    ProtocolViolation => "protocol_violation",
    InvalidKeyPackage => "invalid_key_package",
    /// The request arrived on a host name that is not configured (docs/branding.md §3).
    UnknownHost => "unknown_host",
    BrandAssetNotFound => "brand_asset_not_found",
    BrandNotFound => "brand_not_found",
    /// The signed workspace policy doesn't name the caller as the import account, or its import
    /// period is over (admin.md §3.8).
    ImportNotAllowed => "import_not_allowed",
    /// Imported messages must go before the conversation's first live message; no ID is left there.
    ImportWindowFull => "import_window_full",
    Internal => "internal",
    Unavailable => "unavailable",
  }
}

/// Why a client has to sign in again, as far as the server said (issue #667), so the app can tell
/// the user instead of showing a bare sign-in screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignInReason {
  /// Signed out at the identity provider or on another device.
  SessionEnded,
  /// The session policy's idle timeout or longest session ran out.
  SessionTimedOut,
  /// The identity provider refused the app's refresh token (`invalid_grant`): it expired (idle or
  /// session limit of the provider), was revoked, or the user was disabled there (issue #883). The
  /// app says so itself; no server sends it.
  SignInExpired,
  /// The token was refused without a reason (expired, invalid), or by an older server.
  #[default]
  #[serde(other)]
  Unknown,
}

impl SignInReason {
  pub fn from_error_code(code: &ErrorCode) -> Self {
    match code {
      ErrorCode::SessionEnded => Self::SessionEnded,
      ErrorCode::SessionTimedOut => Self::SessionTimedOut,
      _ => Self::Unknown,
    }
  }

  pub fn is_unknown(&self) -> bool {
    *self == Self::Unknown
  }
}

/// Body of every error response: `{ "error_code": "channel_not_found", "message": "..." }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ErrorBody {
  pub error_code: ErrorCode,
  /// Human-readable English text for logs and developers; clients localize by `error_code`.
  pub message: String,
  /// With `epoch_conflict`: the epoch the MLS group is in now, so the client can rebase without
  /// parsing the message.
  #[serde(rename = "current_epoch", default, skip_serializing_if = "Option::is_none")]
  pub maybe_current_epoch: Option<u64>,
  /// With `version_conflict`: the stored version of the versioned object (key bag, roster, policy).
  #[serde(rename = "current_version", default, skip_serializing_if = "Option::is_none")]
  pub maybe_current_version: Option<u64>,
  /// With a 503 or 429: the `Retry-After` header's seconds, also in the body so every client
  /// (including the browser, where a proxy may strip headers) can honour it.
  #[serde(rename = "retry_after_secs", default, skip_serializing_if = "Option::is_none")]
  pub maybe_retry_after_secs: Option<u64>,
}

impl ErrorBody {
  pub fn new(error_code: ErrorCode, message: impl Into<String>) -> Self {
    Self {
      error_code,
      message: message.into(),
      maybe_current_epoch: None,
      maybe_current_version: None,
      maybe_retry_after_secs: None,
    }
  }

  pub fn with_current_epoch(mut self, epoch: u64) -> Self {
    self.maybe_current_epoch = Some(epoch);
    self
  }

  pub fn with_current_version(mut self, version: u64) -> Self {
    self.maybe_current_version = Some(version);
    self
  }

  pub fn with_retry_after_secs(mut self, seconds: u64) -> Self {
    self.maybe_retry_after_secs = Some(seconds);
    self
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn error_body_wire_format_is_stable() {
    let body = ErrorBody::new(ErrorCode::ChannelNotFound, "no such channel");
    assert_eq!(
      serde_json::to_string(&body).unwrap(),
      r#"{"error_code":"channel_not_found","message":"no such channel"}"#
    );
  }

  #[test]
  fn conflicts_carry_structured_fields_that_are_omitted_otherwise() {
    let body = ErrorBody::new(ErrorCode::EpochConflict, "rebase").with_current_epoch(7);
    assert_eq!(
      serde_json::to_string(&body).unwrap(),
      r#"{"error_code":"epoch_conflict","message":"rebase","current_epoch":7}"#
    );
    let version = ErrorBody::new(ErrorCode::VersionConflict, "stale").with_current_version(3);
    assert!(
      serde_json::to_string(&version)
        .unwrap()
        .ends_with(r#""current_version":3}"#)
    );
    let old: ErrorBody = serde_json::from_str(r#"{"error_code":"epoch_conflict","message":"x"}"#).unwrap();
    assert_eq!((old.maybe_current_epoch, old.maybe_current_version), (None, None));
  }

  #[test]
  fn retry_after_is_in_the_body_only_when_set() {
    let body = ErrorBody::new(ErrorCode::Unavailable, "later").with_retry_after_secs(30);
    assert_eq!(
      serde_json::to_string(&body).unwrap(),
      r#"{"error_code":"unavailable","message":"later","retry_after_secs":30}"#
    );
    let old: ErrorBody = serde_json::from_str(r#"{"error_code":"unavailable","message":"x"}"#).unwrap();
    assert_eq!(old.maybe_retry_after_secs, None);
  }

  #[test]
  fn sign_in_reasons_come_from_the_error_code_and_tolerate_new_ones() {
    assert_eq!(
      SignInReason::from_error_code(&ErrorCode::SessionEnded),
      SignInReason::SessionEnded
    );
    assert_eq!(
      SignInReason::from_error_code(&ErrorCode::SessionTimedOut),
      SignInReason::SessionTimedOut
    );
    assert_eq!(
      SignInReason::from_error_code(&ErrorCode::InvalidToken),
      SignInReason::Unknown
    );
    assert_eq!(
      serde_json::to_string(&SignInReason::SignInExpired).unwrap(),
      r#""sign_in_expired""#
    );
    assert_eq!(
      serde_json::from_str::<SignInReason>(r#""from_a_newer_server""#).unwrap(),
      SignInReason::Unknown
    );
  }

  #[test]
  fn unknown_error_code_is_tolerated() {
    let body: ErrorBody = serde_json::from_str(r#"{"error_code":"quota_exceeded","message":"x"}"#).unwrap();
    assert_eq!(body.error_code, ErrorCode::Unknown("quota_exceeded".into()));
  }

  #[test]
  fn all_codes_round_trip() {
    for code in [
      ErrorCode::InvalidRequest,
      ErrorCode::Unauthorized,
      ErrorCode::InvalidToken,
      ErrorCode::SessionEnded,
      ErrorCode::SessionTimedOut,
      ErrorCode::NoTeamcraftRole,
      ErrorCode::AccountDeactivated,
      ErrorCode::AccountDeleted,
      ErrorCode::Forbidden,
      ErrorCode::UserNotFound,
      ErrorCode::SpaceNotFound,
      ErrorCode::ChannelNotFound,
      ErrorCode::DeviceNotFound,
      ErrorCode::CategoryNotFound,
      ErrorCode::InviteNotFound,
      ErrorCode::IdentityKeyNotFound,
      ErrorCode::KeyBagNotFound,
      ErrorCode::DeviceListNotFound,
      ErrorCode::MlsGroupInfoNotFound,
      ErrorCode::AttachmentNotFound,
      ErrorCode::BackgroundNotFound,
      ErrorCode::CallNotFound,
      ErrorCode::MeetingNotFound,
      ErrorCode::BotNotFound,
      ErrorCode::AvatarNotFound,
      ErrorCode::SpaceImageNotFound,
      ErrorCode::ChannelImageNotFound,
      ErrorCode::AnnouncementNotFound,
      ErrorCode::RecoveryRequestNotFound,
      ErrorCode::AdminJoinRequestNotFound,
      ErrorCode::AdminJoinRequestTooSoon,
      ErrorCode::ShareUnavailable,
      ErrorCode::RecoveryPassphraseNotAllowed,
      ErrorCode::DisplayNameOverrideNotAllowed,
      ErrorCode::NotetakerKeyNotFound,
      ErrorCode::UnknownHost,
      ErrorCode::BrandAssetNotFound,
      ErrorCode::UserGroupNotFound,
      ErrorCode::Conflict,
      ErrorCode::EpochConflict,
      ErrorCode::VersionConflict,
      ErrorCode::LastOwner,
      ErrorCode::CannotDeleteSelf,
      ErrorCode::LastAdmin,
      ErrorCode::CallDeviceLimit,
      ErrorCode::GuestDmNotAllowed,
      ErrorCode::KeyPackageExhausted,
      ErrorCode::PayloadTooLarge,
      ErrorCode::RateLimited,
      ErrorCode::Internal,
      ErrorCode::Unavailable,
    ] {
      let json = serde_json::to_string(&code).unwrap();
      assert_eq!(serde_json::from_str::<ErrorCode>(&json).unwrap(), code);
    }
  }
}
