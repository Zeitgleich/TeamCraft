//! Local guest accounts: guests that sign in with a password and a TOTP code instead of the
//! identity provider (PLAN.md §12).

use std::fmt;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{ConversationId, UnixMillis, UserId, UserSummary};

/// `POST /api/v1/spaces/{id}/local-guests`: creates a guest account, adds it to the space as a guest
/// and e-mails the activation link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateLocalGuestRequest {
  pub email: String,
  pub display_name: String,
  /// Channels of the space the guest joins at their first sign-in.
  #[serde(default)]
  pub channel_ids: Vec<ConversationId>,
  /// When the guest expires; the instance default if omitted.
  #[serde(rename = "expires_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_expires_at: Option<UnixMillis>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LocalGuestResponse {
  pub user: UserSummary,
  pub email: String,
  /// The activation link stops working at this time; an admin can send a new one.
  pub activation_expires_at: UnixMillis,
}

/// `POST /api/v1/auth/local/activate`: spends the one-time token from the e-mail and chooses the
/// password.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ActivateLocalAccountRequest {
  pub token: String,
  pub password: String,
}

/// The TOTP secret to put into an authenticator app. The first valid code at sign-in confirms it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ActivateLocalAccountResponse {
  /// RFC 4648 base32.
  pub totp_secret: String,
  /// `otpauth://` URI for QR codes.
  pub totp_uri: String,
}

/// `POST /api/v1/auth/local/login`
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LocalLoginRequest {
  /// The e-mail address of the account.
  pub login: String,
  pub password: String,
  /// The current TOTP code.
  #[serde(rename = "totp_code", default, skip_serializing_if = "Option::is_none")]
  pub maybe_totp_code: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LocalLoginResponse {
  /// Bearer token for the API and the gateway.
  pub access_token: String,
  pub expires_at: UnixMillis,
  pub user_id: UserId,
}

/// `POST /api/v1/auth/local/password`: ends all sessions of the account.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ChangeLocalPasswordRequest {
  pub current_password: String,
  pub new_password: String,
}

// Secrets never show up in logs, not even through a `{:?}` of a request.
macro_rules! redacted_debug {
  ($($type:ty),+ $(,)?) => {$(
    impl fmt::Debug for $type {
      fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(concat!(stringify!($type), " { [redacted] }"))
      }
    }
  )+};
}

redacted_debug!(
  ActivateLocalAccountRequest,
  ActivateLocalAccountResponse,
  LocalLoginRequest,
  LocalLoginResponse,
  ChangeLocalPasswordRequest,
);

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn debug_output_hides_secrets() {
    let request = LocalLoginRequest {
      login: "a@example.com".into(),
      password: "hunter2".into(),
      maybe_totp_code: Some("123456".into()),
    };
    let text = format!("{request:?}");
    assert!(!text.contains("hunter2") && !text.contains("123456") && !text.contains("example.com"));
    let request: LocalLoginRequest =
      serde_json::from_str(r#"{"login":"a@example.com","password":"x"}"#).unwrap();
    assert_eq!(request.maybe_totp_code, None);
  }
}
