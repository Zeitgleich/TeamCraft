//! FCM HTTP v1 with a service account: the server signs a short-lived JWT (RS256), trades it for an
//! OAuth access token and sends a data-only high-priority message that carries just the sealed
//! wake-up (e2ee.md §7). Google sees ciphertext and the device token.

use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use reqwest::StatusCode;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::json;
use teamcraft_proto::PushKind;
use tokio::sync::Mutex;
use tracing::warn;

use super::sender::{PushError, PushMessage, PushSender, PushTarget, Urgency};
use crate::auth::BoxFuture;

pub const FCM_BASE_URL: &str = "https://fcm.googleapis.com";
const SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";
const ASSERTION_LIFETIME_SECS: u64 = 3600;
/// An access token is renewed this long before Google says it expires.
const TOKEN_MARGIN: Duration = Duration::from_secs(120);
const MAX_TOKEN_CHARS: usize = 4096;

/// An FCM registration token as far as it can be checked here: present and not absurdly long.
pub(crate) fn is_registration_token(token: &str) -> bool {
  !token.is_empty() && token.len() <= MAX_TOKEN_CHARS
}

/// The fields of a Google service account key file that sending needs.
#[derive(Deserialize)]
pub struct ServiceAccount {
  pub client_email: String,
  pub private_key: SecretString,
  pub project_id: String,
  pub token_uri: String,
}

impl ServiceAccount {
  pub fn from_json(json: &str) -> Result<Self, PushError> {
    serde_json::from_str(json).map_err(|e| {
      PushError::Invalid(format!(
        "not a Google service account key: {}",
        super::web_push::json_error(&e)
      ))
    })
  }
}

pub struct FcmSettings {
  pub account: ServiceAccount,
  /// `https://fcm.googleapis.com`, or a fake in tests.
  pub base_url: String,
  pub timeout: Duration,
}

pub struct FcmSender {
  client_email: String,
  key: EncodingKey,
  token_uri: String,
  send_url: String,
  http: reqwest::Client,
  access_token: Mutex<Option<AccessToken>>,
}

struct AccessToken {
  value: SecretString,
  renew_at: Instant,
}

#[derive(Serialize)]
struct AssertionClaims<'a> {
  iss: &'a str,
  scope: &'static str,
  aud: &'a str,
  iat: u64,
  exp: u64,
}

#[derive(Deserialize)]
struct TokenResponse {
  access_token: SecretString,
  expires_in: u64,
}

#[derive(Deserialize)]
struct FcmErrorBody {
  error: Option<FcmError>,
}

#[derive(Deserialize)]
struct FcmError {
  status: Option<String>,
}

impl FcmSender {
  pub fn new(settings: FcmSettings) -> Result<Self, PushError> {
    let account = settings.account;
    let key = EncodingKey::from_rsa_pem(account.private_key.expose_secret().as_bytes())
      .map_err(|_| PushError::Invalid("the service account private key is not an RSA PEM key".into()))?;
    let http = reqwest::Client::builder()
      .timeout(settings.timeout)
      .redirect(reqwest::redirect::Policy::none())
      .build()
      .map_err(|e| PushError::Unavailable(e.without_url().to_string()))?;
    Ok(Self {
      client_email: account.client_email,
      key,
      token_uri: account.token_uri,
      send_url: format!(
        "{}/v1/projects/{}/messages:send",
        settings.base_url.trim_end_matches('/'),
        account.project_id
      ),
      http,
      access_token: Mutex::new(None),
    })
  }

  /// The cached OAuth token, or a fresh one. Holding the lock across the request makes concurrent
  /// sends share one token request.
  async fn bearer(&self) -> Result<SecretString, PushError> {
    let mut cached = self.access_token.lock().await;
    if let Some(token) = cached.as_ref()
      && Instant::now() < token.renew_at
    {
      return Ok(token.value.clone());
    }
    let now = jsonwebtoken::get_current_timestamp();
    let claims = AssertionClaims {
      iss: &self.client_email,
      scope: SCOPE,
      aud: &self.token_uri,
      iat: now,
      exp: now + ASSERTION_LIFETIME_SECS,
    };
    let assertion = jsonwebtoken::encode(&Header::new(Algorithm::RS256), &claims, &self.key)
      .map_err(|_| PushError::Invalid("signing the service account assertion failed".into()))?;
    let response = self
      .http
      .post(&self.token_uri)
      .form(&[
        ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
        ("assertion", assertion.as_str()),
      ])
      .send()
      .await
      .map_err(|e| PushError::Unavailable(e.without_url().to_string()))?;
    let status = response.status();
    if !status.is_success() {
      warn!(%status, "the Google token endpoint refused the service account");
      // Bad credentials are the operator's to fix; a server error is Google's.
      return Err(
        if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
          PushError::Unavailable(format!("token endpoint status {status}"))
        } else {
          PushError::Rejected(status.as_u16())
        },
      );
    }
    let token: TokenResponse = response
      .json()
      .await
      .map_err(|_| PushError::Unavailable("unreadable token response".into()))?;
    let lifetime = Duration::from_secs(token.expires_in).saturating_sub(TOKEN_MARGIN);
    let value = token.access_token.clone();
    *cached = Some(AccessToken {
      value: token.access_token,
      renew_at: Instant::now() + lifetime,
    });
    Ok(value)
  }

  async fn forget_access_token(&self) {
    *self.access_token.lock().await = None;
  }
}

impl PushSender for FcmSender {
  fn send<'a>(
    &'a self,
    target: &'a PushTarget,
    message: &'a PushMessage,
  ) -> BoxFuture<'a, Result<(), PushError>> {
    Box::pin(async move {
      if !is_registration_token(&target.token) {
        return Err(PushError::Invalid("not an FCM registration token".into()));
      }
      let priority = android_priority(message);
      let body = json!({
        "message": {
          "token": target.token,
          "data": { "w": URL_SAFE_NO_PAD.encode(&message.sealed) },
          "android": {
            "priority": priority,
            "ttl": format!("{}s", message.ttl.as_secs()),
          },
        }
      });
      let response = self
        .http
        .post(&self.send_url)
        // bearer_auth marks the header sensitive.
        .bearer_auth(self.bearer().await?.expose_secret())
        .json(&body)
        .send()
        .await
        .map_err(|e| PushError::Unavailable(e.without_url().to_string()))?;
      let status = response.status();
      if status.is_success() {
        return Ok(());
      }
      let error_status = response
        .json::<FcmErrorBody>()
        .await
        .ok()
        .and_then(|body| body.error)
        .and_then(|error| error.status)
        .unwrap_or_default();
      if status == StatusCode::UNAUTHORIZED {
        self.forget_access_token().await;
      }
      classify(status, &error_status)
    })
  }
}

/// Everything the user should see must wake a dozing phone at once (Doze holds `NORMAL` messages
/// for a maintenance window); silent withdrawals may wait for it. Android lowers the priority of apps
/// whose high-priority messages show nothing, so `dismiss` never goes out as `HIGH`.
fn android_priority(message: &PushMessage) -> &'static str {
  if message.kind.is_visible() && (message.urgency == Urgency::High || message.kind == PushKind::Call) {
    "HIGH"
  } else {
    "NORMAL"
  }
}

/// Maps an FCM v1 error (<https://firebase.google.com/docs/reference/fcm/rest/v1/ErrorCode>).
fn classify(status: StatusCode, error_status: &str) -> Result<(), PushError> {
  match (status.as_u16(), error_status) {
    // The registration token is unknown or expired.
    (404, _) | (_, "UNREGISTERED") => Err(PushError::Gone),
    (429, _) | (500..=599, _) => Err(PushError::Unavailable(format!("status {status}"))),
    (code, reason) => {
      warn!(status = code, %reason, "FCM rejected the push");
      Err(PushError::Rejected(code))
    },
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_malformed_key_file_is_not_echoed() {
    // A key file encoded twice is a JSON string: serde would quote it, private key included.
    let doubled = serde_json::to_string(r#"{"private_key": "-----BEGIN PRIVATE KEY-----SECRET"}"#).unwrap();
    let Err(error) = ServiceAccount::from_json(&doubled) else {
      panic!("a JSON string is not a key file");
    };
    assert!(!error.to_string().contains("SECRET"), "{error}");
  }

  #[test]
  fn error_responses_are_classified() {
    let class =
      |status: u16, reason: &str| classify(StatusCode::from_u16(status).unwrap(), reason).unwrap_err();
    assert!(matches!(class(404, "NOT_FOUND"), PushError::Gone));
    assert!(matches!(class(400, "UNREGISTERED"), PushError::Gone));
    assert!(matches!(class(400, "INVALID_ARGUMENT"), PushError::Rejected(400)));
    assert!(matches!(
      class(403, "SENDER_ID_MISMATCH"),
      PushError::Rejected(403)
    ));
    assert!(matches!(class(429, "QUOTA_EXCEEDED"), PushError::Unavailable(_)));
    assert!(matches!(class(503, "UNAVAILABLE"), PushError::Unavailable(_)));
  }

  #[test]
  fn visible_pushes_are_high_priority_and_withdrawals_normal() {
    let message = |kind: PushKind, urgency: Urgency| PushMessage {
      sealed: Vec::new(),
      kind,
      urgency,
      ttl: Duration::from_secs(60),
    };
    for kind in [PushKind::Message, PushKind::Mention, PushKind::Test] {
      assert_eq!(android_priority(&message(kind, Urgency::High)), "HIGH");
    }
    assert_eq!(
      android_priority(&message(PushKind::Call, Urgency::Normal)),
      "HIGH"
    );
    assert_eq!(
      android_priority(&message(PushKind::Dismiss, Urgency::High)),
      "NORMAL"
    );
    assert_eq!(
      android_priority(&message(PushKind::Message, Urgency::Normal)),
      "NORMAL"
    );
  }

  #[test]
  fn service_account_keys_need_the_sending_fields() {
    assert!(ServiceAccount::from_json("{}").is_err());
    assert!(ServiceAccount::from_json("not json").is_err());
  }
}
