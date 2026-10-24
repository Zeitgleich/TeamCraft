//! Local accounts (e-mail, password and one-time code instead of the identity provider): the
//! server issues a bearer token without a refresh token. It is kept in memory for the API and the
//! client core, and in the keyring as its own entry per server so the session survives a restart
//! until it expires.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use teamcraft_proto::{ErrorBody, ErrorCode, LocalLoginResponse};

use super::AuthError;

pub const LOCAL_LOGIN_PATH: &str = "/api/v1/auth/local/login";
pub const LOCAL_LOGOUT_PATH: &str = "/api/v1/auth/local/logout";
/// A token this close to its expiry counts as expired: the server decides, and a request with a
/// token that runs out on the way fails anyway.
const EXPIRY_MARGIN_MS: i64 = 30_000;

/// The session of a local account.
pub struct LocalSession {
  pub access: SecretString,
  /// Unix milliseconds, as the server sent it.
  pub expires_at_ms: i64,
}

/// Why the server refused a local sign-in, as the UI tells the user. One answer for wrong e-mail,
/// password or code: the server does not say which, and neither does the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalRefusal {
  Refused,
  /// Local accounts are turned off or not set up on this server.
  Disabled,
  RateLimited,
}

/// What the keyring entry holds: the token and its expiry as JSON.
#[derive(Deserialize)]
struct StoredSession {
  token: SecretString,
  expires_at_ms: i64,
}

#[derive(Serialize)]
struct StoredSessionRef<'a> {
  token: &'a str,
  expires_at_ms: i64,
}

#[derive(Serialize)]
struct LoginBody<'a> {
  login: &'a str,
  password: &'a str,
  #[serde(rename = "totp_code", skip_serializing_if = "Option::is_none")]
  maybe_totp_code: Option<&'a str>,
}

impl LocalSession {
  /// Reads the `200` answer of `POST /api/v1/auth/local/login`.
  pub fn from_response(body: &[u8]) -> Result<Self, AuthError> {
    let granted: LocalLoginResponse =
      serde_json::from_slice(body).map_err(|error| AuthError::InvalidResponse(error.to_string()))?;
    Ok(Self {
      access: SecretString::from(granted.access_token),
      expires_at_ms: granted.expires_at.0,
    })
  }

  /// The stored form; `None` for an entry that is not ours or is damaged.
  pub fn decode(stored: &SecretString) -> Option<Self> {
    let stored: StoredSession = serde_json::from_str(stored.expose_secret()).ok()?;
    (!stored.token.expose_secret().is_empty()).then_some(Self {
      access: stored.token,
      expires_at_ms: stored.expires_at_ms,
    })
  }

  pub fn encode(&self) -> Result<SecretString, AuthError> {
    let stored = StoredSessionRef {
      token: self.access.expose_secret(),
      expires_at_ms: self.expires_at_ms,
    };
    serde_json::to_string(&stored)
      .map(SecretString::from)
      .map_err(|error| AuthError::InvalidResponse(error.to_string()))
  }

  pub fn is_live(&self, now_ms: i64) -> bool {
    self.expires_at_ms - EXPIRY_MARGIN_MS > now_ms
  }

  /// How long the token may still be sent (expiry minus the margin); `None` once that has passed.
  pub fn remaining(&self, now_ms: i64) -> Option<Duration> {
    let left_ms = self.expires_at_ms - EXPIRY_MARGIN_MS - now_ms;
    u64::try_from(left_ms)
      .ok()
      .filter(|left_ms| *left_ms > 0)
      .map(Duration::from_millis)
  }

  /// A copy of the token for one request.
  pub fn token(&self) -> SecretString {
    SecretString::from(self.access.expose_secret().to_owned())
  }
}

impl LocalRefusal {
  /// The command error kind the UI maps to its messages.
  pub fn kind(self) -> &'static str {
    match self {
      Self::Refused => "refused",
      Self::Disabled => "disabled",
      Self::RateLimited => "rate_limited",
    }
  }

  /// Reads a failed answer of the login endpoint. `None`: not a refusal (a server error), shown as a
  /// failure with details.
  pub fn from_answer(status: u16, body: &[u8]) -> Option<Self> {
    let maybe_code = serde_json::from_slice::<ErrorBody>(body)
      .ok()
      .map(|body| body.error_code);
    match maybe_code {
      // A missing code is a plain refusal too; older servers asked for it separately.
      Some(ErrorCode::Unauthorized | ErrorCode::MfaRequired | ErrorCode::InvalidRequest) => {
        Some(Self::Refused)
      },
      Some(ErrorCode::LocalAccountsDisabled) => Some(Self::Disabled),
      Some(ErrorCode::RateLimited) => Some(Self::RateLimited),
      _ => match status {
        400 | 401 => Some(Self::Refused),
        404 | 503 => Some(Self::Disabled),
        429 => Some(Self::RateLimited),
        _ => None,
      },
    }
  }
}

/// The JSON body of the login request. It holds the password, so it stays a secret.
pub fn login_body(
  login: &str,
  password: &SecretString,
  maybe_totp_code: Option<&str>,
) -> Result<SecretString, AuthError> {
  let body = LoginBody {
    login,
    password: password.expose_secret(),
    maybe_totp_code,
  };
  serde_json::to_string(&body)
    .map(SecretString::from)
    .map_err(|error| AuthError::InvalidResponse(error.to_string()))
}

/// Unix milliseconds of this computer: only used to drop a stored token early; the server checks
/// expiry itself.
pub fn now_ms() -> i64 {
  SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| {
    i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  const NOW_MS: i64 = 1_800_000_000_000;

  #[test]
  fn stored_sessions_round_trip_and_damaged_ones_are_dropped() {
    let session = LocalSession {
      access: SecretString::from("token-a"),
      expires_at_ms: NOW_MS + 3_600_000,
    };
    let decoded = LocalSession::decode(&session.encode().unwrap()).unwrap();
    assert_eq!(decoded.access.expose_secret(), "token-a");
    assert_eq!(decoded.expires_at_ms, session.expires_at_ms);
    for damaged in ["", "refresh-token", "{}", r#"{"token":"","expires_at_ms":1}"#] {
      assert!(
        LocalSession::decode(&SecretString::from(damaged)).is_none(),
        "{damaged}"
      );
    }
  }

  #[test]
  fn sessions_expire_with_a_margin() {
    let session = |expires_at_ms| LocalSession {
      access: SecretString::from("token"),
      expires_at_ms,
    };
    assert!(session(NOW_MS + 60_000).is_live(NOW_MS));
    assert!(!session(NOW_MS + 10_000).is_live(NOW_MS));
    assert!(!session(NOW_MS - 1).is_live(NOW_MS));
  }

  #[test]
  fn the_login_answer_is_read() {
    let body = br#"{"access_token":"granted","expires_at":1800000000000,"user_id":"42"}"#;
    let session = LocalSession::from_response(body).unwrap();
    assert_eq!(session.access.expose_secret(), "granted");
    assert_eq!(session.expires_at_ms, NOW_MS);
    assert!(matches!(
      LocalSession::from_response(b"<html>"),
      Err(AuthError::InvalidResponse(_))
    ));
  }

  #[test]
  fn refusals_map_to_what_the_user_is_told() {
    let body = |code: &str| format!(r#"{{"error_code":"{code}","message":"x"}}"#).into_bytes();
    let cases = [
      (401, body("unauthorized"), Some(LocalRefusal::Refused)),
      (401, body("mfa_required"), Some(LocalRefusal::Refused)),
      (403, body("local_accounts_disabled"), Some(LocalRefusal::Disabled)),
      (429, body("rate_limited"), Some(LocalRefusal::RateLimited)),
      (429, b"slow down".to_vec(), Some(LocalRefusal::RateLimited)),
      (503, b"".to_vec(), Some(LocalRefusal::Disabled)),
      (403, body("forbidden"), None),
      (500, body("internal"), None),
      (502, b"<html>".to_vec(), None),
    ];
    for (status, body, expected) in cases {
      assert_eq!(LocalRefusal::from_answer(status, &body), expected, "{status}");
    }
    assert_eq!(LocalRefusal::Refused.kind(), "refused");
    assert_eq!(LocalRefusal::Disabled.kind(), "disabled");
    assert_eq!(LocalRefusal::RateLimited.kind(), "rate_limited");
  }

  #[test]
  fn the_code_is_sent_only_when_given() {
    let password = SecretString::from("pw\"1");
    let with = login_body("a@example.com", &password, Some("123456")).unwrap();
    let value: serde_json::Value = serde_json::from_str(with.expose_secret()).unwrap();
    assert_eq!(
      value,
      serde_json::json!({"login": "a@example.com", "password": "pw\"1", "totp_code": "123456"})
    );
    let without = login_body("a@example.com", &password, None).unwrap();
    assert!(!without.expose_secret().contains("totp_code"));
  }
}
