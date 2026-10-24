use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;

wire_enum! {
  /// Stable machine-readable error identifiers. Values are part of the API and never change.
  pub enum ErrorCode {
    InvalidRequest => "invalid_request",
    Unauthorized => "unauthorized",
    InvalidToken => "invalid_token",
    NoTeamcraftRole => "no_teamcraft_role",
    Forbidden => "forbidden",
    UserNotFound => "user_not_found",
    SpaceNotFound => "space_not_found",
    ChannelNotFound => "channel_not_found",
    DeviceNotFound => "device_not_found",
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
    AnnouncementNotFound => "announcement_not_found",
    RecoveryRequestNotFound => "recovery_request_not_found",
    ShareUnavailable => "share_unavailable",
    LocalAccountsDisabled => "local_accounts_disabled",
    UserGroupNotFound => "user_group_not_found",
    MfaRequired => "mfa_required",
    Conflict => "conflict",
    EpochConflict => "epoch_conflict",
    VersionConflict => "version_conflict",
    /// A roster change would leave the workspace without an owner (admin.md §2.3).
    LastOwner => "last_owner",
    KeyPackageExhausted => "key_package_exhausted",
    PayloadTooLarge => "payload_too_large",
    RateLimited => "rate_limited",
    InvalidJoinToken => "invalid_join_token",
    AlreadyJoined => "already_joined",
    /// The meeting host locked the call: no new participants.
    CallLocked => "call_locked",
    /// The meeting host removed this user from the call.
    RemovedFromCall => "removed_from_call",
    ProtocolViolation => "protocol_violation",
    InvalidKeyPackage => "invalid_key_package",
    /// The request arrived on a host name that is not configured (docs/branding.md §3).
    UnknownHost => "unknown_host",
    BrandAssetNotFound => "brand_asset_not_found",
    BrandNotFound => "brand_not_found",
    Internal => "internal",
    Unavailable => "unavailable",
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
}

impl ErrorBody {
  pub fn new(error_code: ErrorCode, message: impl Into<String>) -> Self {
    Self {
      error_code,
      message: message.into(),
      maybe_current_epoch: None,
      maybe_current_version: None,
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
      ErrorCode::NoTeamcraftRole,
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
      ErrorCode::AnnouncementNotFound,
      ErrorCode::RecoveryRequestNotFound,
      ErrorCode::ShareUnavailable,
      ErrorCode::UnknownHost,
      ErrorCode::BrandAssetNotFound,
      ErrorCode::UserGroupNotFound,
      ErrorCode::Conflict,
      ErrorCode::EpochConflict,
      ErrorCode::VersionConflict,
      ErrorCode::LastOwner,
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
