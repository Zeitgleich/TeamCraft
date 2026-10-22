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
    AttachmentNotFound => "attachment_not_found",
    Conflict => "conflict",
    EpochConflict => "epoch_conflict",
    VersionConflict => "version_conflict",
    KeyPackageExhausted => "key_package_exhausted",
    PayloadTooLarge => "payload_too_large",
    RateLimited => "rate_limited",
    InvalidJoinToken => "invalid_join_token",
    AlreadyJoined => "already_joined",
    ProtocolViolation => "protocol_violation",
    InvalidKeyPackage => "invalid_key_package",
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
}

impl ErrorBody {
  pub fn new(error_code: ErrorCode, message: impl Into<String>) -> Self {
    Self {
      error_code,
      message: message.into(),
    }
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
      ErrorCode::AttachmentNotFound,
      ErrorCode::Conflict,
      ErrorCode::EpochConflict,
      ErrorCode::VersionConflict,
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
