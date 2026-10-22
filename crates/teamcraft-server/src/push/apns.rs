//! APNs over HTTP/2 with provider token authentication (a `.p8` key). Message pushes are alerts with
//! `mutable-content`, so the iOS notification service extension can open the sealed wake-up; call
//! pushes use the VoIP topic (e2ee.md §7). The alert text is generic: Apple never sees content.

use std::sync::Mutex;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use reqwest::StatusCode;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use teamcraft_proto::PushKind;
use tracing::warn;

use super::sender::{PushError, PushMessage, PushSender, PushTarget, Urgency};
use crate::auth::BoxFuture;

pub const APNS_PRODUCTION_URL: &str = "https://api.push.apple.com";
pub const APNS_SANDBOX_URL: &str = "https://api.sandbox.push.apple.com";

/// Apple wants a provider token refreshed at most every 20 minutes and rejects ones older than an hour.
const TOKEN_LIFETIME_SECS: u64 = 40 * 60;
/// Calls ring now or not at all. The sealed wake-up sits under the key `w` (same for FCM), which is
/// what the apps read.
const VOIP_EXPIRATION: &str = "0";

/// Everything the APNs sender needs. The key is the contents of the `.p8` file.
pub struct ApnsSettings {
  pub key_pem: SecretString,
  pub key_id: String,
  pub team_id: String,
  /// The app's bundle ID; VoIP pushes go to `<bundle_id>.voip`.
  pub bundle_id: String,
  /// `https://api.push.apple.com`, the sandbox, or a fake in tests. Plain `http://` uses HTTP/2
  /// without TLS (tests only).
  pub base_url: String,
  /// Visible text of the generic alert the extension replaces.
  pub alert_text: String,
  pub timeout: Duration,
}

pub struct ApnsSender {
  key: EncodingKey,
  key_id: String,
  team_id: String,
  bundle_id: String,
  base_url: String,
  alert_text: String,
  http: reqwest::Client,
  cached_token: Mutex<Option<CachedToken>>,
}

struct CachedToken {
  jwt: SecretString,
  issued_at: u64,
}

#[derive(Serialize)]
struct ProviderClaims<'a> {
  iss: &'a str,
  iat: u64,
}

#[derive(Serialize)]
struct AlertPayload<'a> {
  aps: Aps<'a>,
  #[serde(rename = "w")]
  sealed: &'a str,
}

#[derive(Serialize)]
struct Aps<'a> {
  alert: AlertBody<'a>,
  sound: &'static str,
  #[serde(rename = "mutable-content")]
  mutable_content: u8,
}

#[derive(Serialize)]
struct AlertBody<'a> {
  body: &'a str,
}

#[derive(Serialize)]
struct VoipPayload<'a> {
  #[serde(rename = "w")]
  sealed: &'a str,
}

#[derive(Deserialize)]
struct ApnsError {
  reason: Option<String>,
}

impl ApnsSender {
  pub fn new(settings: ApnsSettings) -> Result<Self, PushError> {
    let key = EncodingKey::from_ec_pem(settings.key_pem.expose_secret().as_bytes())
      .map_err(|_| PushError::Invalid("the APNs key is not a PKCS#8 EC private key (.p8)".into()))?;
    let mut builder = reqwest::Client::builder()
      .timeout(settings.timeout)
      .redirect(reqwest::redirect::Policy::none());
    if settings.base_url.starts_with("http://") {
      builder = builder.http2_prior_knowledge();
    }
    let http = builder
      .build()
      .map_err(|e| PushError::Unavailable(e.without_url().to_string()))?;
    Ok(Self {
      key,
      key_id: settings.key_id,
      team_id: settings.team_id,
      bundle_id: settings.bundle_id,
      base_url: settings.base_url.trim_end_matches('/').to_owned(),
      alert_text: settings.alert_text,
      http,
      cached_token: Mutex::new(None),
    })
  }

  /// The provider token, signed again when the cached one is old.
  fn provider_token(&self, now: u64) -> Result<SecretString, PushError> {
    let mut cached = self.cached_token.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(token) = cached.as_ref()
      && now.saturating_sub(token.issued_at) < TOKEN_LIFETIME_SECS
    {
      return Ok(token.jwt.clone());
    }
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some(self.key_id.clone());
    let claims = ProviderClaims {
      iss: &self.team_id,
      iat: now,
    };
    let jwt: SecretString = jsonwebtoken::encode(&header, &claims, &self.key)
      .map_err(|_| PushError::Invalid("signing the APNs provider token failed".into()))?
      .into();
    *cached = Some(CachedToken {
      jwt: jwt.clone(),
      issued_at: now,
    });
    Ok(jwt)
  }

  fn forget_provider_token(&self) {
    *self.cached_token.lock().unwrap_or_else(|p| p.into_inner()) = None;
  }
}

/// `bearer <token>`, marked sensitive so it never shows in debug output.
fn authorization_header(token: &SecretString) -> Result<reqwest::header::HeaderValue, PushError> {
  let mut value = reqwest::header::HeaderValue::try_from(format!("bearer {}", token.expose_secret()))
    .map_err(|_| PushError::Invalid("the APNs provider token is not a header value".into()))?;
  value.set_sensitive(true);
  Ok(value)
}

impl PushSender for ApnsSender {
  fn send<'a>(
    &'a self,
    target: &'a PushTarget,
    message: &'a PushMessage,
  ) -> BoxFuture<'a, Result<(), PushError>> {
    Box::pin(async move {
      if !is_device_token(&target.token) {
        return Err(PushError::Invalid("not an APNs device token".into()));
      }
      let now = jsonwebtoken::get_current_timestamp();
      let is_call = message.kind == PushKind::Call;
      let sealed = URL_SAFE_NO_PAD.encode(&message.sealed);
      let (push_type, topic, priority, expiration, body) = if is_call {
        (
          "voip",
          format!("{}.voip", self.bundle_id),
          "10",
          VOIP_EXPIRATION.to_owned(),
          serde_json::to_vec(&VoipPayload { sealed: &sealed }),
        )
      } else {
        (
          "alert",
          self.bundle_id.clone(),
          match message.urgency {
            Urgency::High => "10",
            Urgency::Normal => "5",
          },
          now.saturating_add(message.ttl.as_secs()).to_string(),
          serde_json::to_vec(&AlertPayload {
            aps: Aps {
              alert: AlertBody {
                body: &self.alert_text,
              },
              sound: "default",
              mutable_content: 1,
            },
            sealed: &sealed,
          }),
        )
      };
      let body = body.map_err(|_| PushError::Invalid("encoding the APNs payload failed".into()))?;

      let response = self
        .http
        .post(format!("{}/3/device/{}", self.base_url, target.token))
        .header("authorization", authorization_header(&self.provider_token(now)?)?)
        .header("apns-topic", topic)
        .header("apns-push-type", push_type)
        .header("apns-priority", priority)
        .header("apns-expiration", expiration)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(|e| PushError::Unavailable(e.without_url().to_string()))?;
      let status = response.status();
      if status.is_success() {
        return Ok(());
      }
      let reason = response
        .json::<ApnsError>()
        .await
        .ok()
        .and_then(|error| error.reason)
        .unwrap_or_default();
      classify(status, &reason, || self.forget_provider_token())
    })
  }
}

/// Maps an APNs error response (<https://developer.apple.com/documentation/usernotifications>).
fn classify(status: StatusCode, reason: &str, on_bad_credentials: impl FnOnce()) -> Result<(), PushError> {
  match (status.as_u16(), reason) {
    // The token is dead: unregistered, or made for another app or environment.
    (410, _) | (400, "BadDeviceToken" | "DeviceTokenNotForTopic") => Err(PushError::Gone),
    (403, "ExpiredProviderToken" | "InvalidProviderToken") => {
      on_bad_credentials();
      warn!(%reason, "APNs rejected the provider token");
      Err(PushError::Rejected(403))
    },
    (429, _) | (500..=599, _) => Err(PushError::Unavailable(format!("status {status}"))),
    (code, _) => {
      warn!(status = code, %reason, "APNs rejected the push");
      Err(PushError::Rejected(code))
    },
  }
}

/// APNs device tokens are hex; anything else never goes into the request path.
fn is_device_token(token: &str) -> bool {
  (32..=200).contains(&token.len()) && token.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_hex_tokens_are_sent() {
    assert!(is_device_token(&"ab12".repeat(16)));
    for bad in ["", "short", "../../x", &"zz".repeat(20), &"a".repeat(201)] {
      assert!(!is_device_token(bad), "{bad}");
    }
  }

  #[test]
  fn error_responses_are_classified() {
    let class =
      |status: u16, reason: &str| classify(StatusCode::from_u16(status).unwrap(), reason, || {}).unwrap_err();
    assert!(matches!(class(410, "Unregistered"), PushError::Gone));
    assert!(matches!(class(400, "BadDeviceToken"), PushError::Gone));
    assert!(matches!(class(400, "BadTopic"), PushError::Rejected(400)));
    assert!(matches!(
      class(403, "InvalidProviderToken"),
      PushError::Rejected(403)
    ));
    assert!(matches!(class(429, "TooManyRequests"), PushError::Unavailable(_)));
    assert!(matches!(
      class(503, "ServiceUnavailable"),
      PushError::Unavailable(_)
    ));
  }
}
