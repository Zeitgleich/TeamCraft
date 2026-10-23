//! APNs over HTTP/2 with provider token authentication (a `.p8` key). Message pushes are alerts with
//! `mutable-content`, so the iOS notification service extension can open the sealed wake-up; call
//! pushes use the VoIP topic (e2ee.md §7); withdrawals are silent background pushes the app handles
//! itself. The alert text is generic: Apple never sees content.

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

#[derive(Serialize)]
struct BackgroundPayload<'a> {
  aps: BackgroundAps,
  #[serde(rename = "w")]
  sealed: &'a str,
}

#[derive(Serialize)]
struct BackgroundAps {
  #[serde(rename = "content-available")]
  content_available: u8,
}

/// The parts of one APNs request that depend on the wake-up.
#[derive(Debug, PartialEq, Eq)]
struct ApnsRequest {
  push_type: &'static str,
  topic: String,
  priority: &'static str,
  expiration: String,
  body: Vec<u8>,
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

  /// Calls go through PushKit (VoIP topic, now or never); withdrawals are background pushes, which
  /// Apple requires to have priority 5; everything else is a mutable alert for the extension.
  fn request(&self, message: &PushMessage, now: u64) -> Result<ApnsRequest, PushError> {
    let sealed = URL_SAFE_NO_PAD.encode(&message.sealed);
    let until_ttl = now.saturating_add(message.ttl.as_secs()).to_string();
    let (push_type, topic, priority, expiration, body) = match message.kind {
      PushKind::Call => (
        "voip",
        format!("{}.voip", self.bundle_id),
        "10",
        VOIP_EXPIRATION.to_owned(),
        serde_json::to_vec(&VoipPayload { sealed: &sealed }),
      ),
      PushKind::Dismiss => (
        "background",
        self.bundle_id.clone(),
        "5",
        until_ttl,
        serde_json::to_vec(&BackgroundPayload {
          aps: BackgroundAps { content_available: 1 },
          sealed: &sealed,
        }),
      ),
      PushKind::Message | PushKind::Mention | PushKind::Test | PushKind::Unknown(_) => (
        "alert",
        self.bundle_id.clone(),
        match message.urgency {
          Urgency::High => "10",
          Urgency::Normal => "5",
        },
        until_ttl,
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
      ),
    };
    let body = body.map_err(|_| PushError::Invalid("encoding the APNs payload failed".into()))?;
    Ok(ApnsRequest {
      push_type,
      topic,
      priority,
      expiration,
      body,
    })
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
      let request = self.request(message, now)?;

      let response = self
        .http
        .post(format!("{}/3/device/{}", self.base_url, target.token))
        .header("authorization", authorization_header(&self.provider_token(now)?)?)
        .header("apns-topic", request.topic)
        .header("apns-push-type", request.push_type)
        .header("apns-priority", request.priority)
        .header("apns-expiration", request.expiration)
        .header("content-type", "application/json")
        .body(request.body)
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
/// An APNs device token: hex, as the apps hand it over (also checked for gateway registrations).
pub(crate) fn is_device_token(token: &str) -> bool {
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

  mod requests {
    use super::*;

    // A throwaway P-256 key, only for building a sender in tests.
    const TEST_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg6G/tz4y1fJr77K7z
8ihxQPWlvTEJdzjz7EQQOhyZDCihRANCAATZcK4MntsHsGrLiiYY/R5u+PkPZpuN
dgo8LHX0AxF1j8QPNmXOWNMLAbz+fRtRJGmQiDQP92HZZ1d7Hu/IeczB
-----END PRIVATE KEY-----
";

    #[test]
    fn calls_go_through_pushkit_now_or_never() {
      let request = sender().request(&message(PushKind::Call), 1000).unwrap();
      assert_eq!(
        (
          request.push_type,
          request.topic.as_str(),
          request.priority,
          request.expiration.as_str()
        ),
        ("voip", "com.example.app.voip", "10", "0")
      );
      assert!(!String::from_utf8(request.body).unwrap().contains("aps"));
    }

    #[test]
    fn withdrawals_are_silent_background_pushes() {
      let request = sender().request(&message(PushKind::Dismiss), 1000).unwrap();
      assert_eq!(
        (
          request.push_type,
          request.topic.as_str(),
          request.priority,
          request.expiration.as_str()
        ),
        ("background", "com.example.app", "5", "1060")
      );
      let body = String::from_utf8(request.body).unwrap();
      assert!(body.contains(r#""content-available":1"#), "{body}");
      assert!(
        !body.contains("alert") && !body.contains("mutable-content"),
        "{body}"
      );
    }

    #[test]
    fn messages_and_tests_are_mutable_alerts() {
      for kind in [PushKind::Message, PushKind::Mention, PushKind::Test] {
        let request = sender().request(&message(kind), 1000).unwrap();
        assert_eq!((request.push_type, request.priority), ("alert", "10"));
        let body = String::from_utf8(request.body).unwrap();
        assert!(body.contains(r#""mutable-content":1"#), "{body}");
        assert!(body.contains("New message"), "{body}");
      }
    }

    fn sender() -> ApnsSender {
      ApnsSender::new(ApnsSettings {
        key_pem: TEST_KEY.to_owned().into(),
        key_id: "KEY".into(),
        team_id: "TEAM".into(),
        bundle_id: "com.example.app".into(),
        base_url: "http://127.0.0.1:1".into(),
        alert_text: "New message".into(),
        timeout: Duration::from_secs(1),
      })
      .unwrap()
    }

    fn message(kind: PushKind) -> PushMessage {
      PushMessage {
        sealed: vec![1, 2, 3],
        kind,
        urgency: Urgency::High,
        ttl: Duration::from_secs(60),
      }
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
