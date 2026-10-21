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
    RateLimited => "rate_limited",
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
      ErrorCode::RateLimited,
      ErrorCode::Internal,
      ErrorCode::Unavailable,
    ] {
      let json = serde_json::to_string(&code).unwrap();
      assert_eq!(serde_json::from_str::<ErrorCode>(&json).unwrap(), code);
    }
  }
}
