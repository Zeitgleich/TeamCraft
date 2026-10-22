//! Push gateway client (docs/push-gateway.md): sends the sealed wake-ups for APNs and FCM
//! registrations to a gateway that holds the credentials of the published store apps. The gateway
//! sees the device token and ciphertext, never a user, device or key. The server proves who it is
//! with a signature of its own key pair, registered once at the gateway.

use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand_core::{OsRng, RngCore};
use teamcraft_crypto::push_gateway::{GatewayRequest, GatewayServerKey};
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{
  Base64Bytes, GATEWAY_PUSH_PATH, GATEWAY_REGISTER_PATH, GATEWAY_REQUEST_ID_LEN, GATEWAY_SERVER_ID_HEADER,
  GATEWAY_SIGNATURE_HEADER, GATEWAY_TIMESTAMP_HEADER, GatewayErrorBody, GatewayErrorCode, GatewayPushRequest,
  GatewayRegisterRequest, GatewayUrgency, PushProvider,
};
use teamcraft_push_providers::{BoxFuture, ProviderError, PushMessage, TokenSender, Urgency};
use tracing::{info, warn};
use zeroize::Zeroizing;

/// What the client needs to reach a gateway.
pub struct GatewaySettings {
  /// Base URL, e.g. `https://gateway.example.com`. Plain `http://` only in tests.
  pub url: String,
  pub key: GatewayServerKey,
  pub timeout: Duration,
}

/// One server's connection to one gateway; the APNs and FCM senders share it.
pub struct GatewayClient {
  base_url: String,
  key: GatewayServerKey,
  server_id: String,
  http: reqwest::Client,
}

/// The sender of one provider (APNs or FCM) behind a [`GatewayClient`].
pub struct GatewaySender {
  client: Arc<GatewayClient>,
  provider: PushProvider,
}

impl GatewayClient {
  pub fn new(settings: GatewaySettings) -> Result<Arc<Self>, ProviderError> {
    let http = reqwest::Client::builder()
      .timeout(settings.timeout)
      .redirect(reqwest::redirect::Policy::none())
      .build()
      .map_err(|e| ProviderError::Unavailable(e.without_url().to_string()))?;
    Ok(Arc::new(Self {
      base_url: settings.url.trim_end_matches('/').to_owned(),
      server_id: settings.key.public_key().server_id(),
      key: settings.key,
      http,
    }))
  }

  /// The sender for APNs or FCM registrations.
  pub fn sender(self: &Arc<Self>, provider: PushProvider) -> GatewaySender {
    GatewaySender {
      client: self.clone(),
      provider,
    }
  }

  /// The ID the gateway knows this server by (safe to log: it is a hash of the public key).
  pub fn server_id(&self) -> &str {
    &self.server_id
  }

  /// Makes the gateway know this server's public key. Idempotent; safe to repeat.
  pub async fn register(&self) -> Result<(), ProviderError> {
    let body = serde_json::to_vec(&GatewayRegisterRequest {
      public_key: Base64Bytes::new(self.key.public_key().as_bytes().to_vec()),
    })
    .map_err(|_| ProviderError::Invalid("encoding the registration failed".into()))?;
    self.post(GATEWAY_REGISTER_PATH, body).await?;
    info!(server_id = %self.server_id, "registered at the push gateway");
    Ok(())
  }

  async fn push(&self, request: &GatewayPushRequest) -> Result<(), ProviderError> {
    let body = serde_json::to_vec(request)
      .map_err(|_| ProviderError::Invalid("encoding the gateway request failed".into()))?;
    match self.post(GATEWAY_PUSH_PATH, body.clone()).await {
      // The gateway lost us (new database) or this is the first push: register and try once more.
      Err(GatewayFailure::UnknownServer) => {
        self.register().await?;
        self.post(GATEWAY_PUSH_PATH, body).await.map_err(Into::into)
      },
      other => other.map_err(Into::into),
    }
  }

  /// One signed POST; the response body is not needed on success.
  async fn post(&self, path: &str, body: Vec<u8>) -> Result<(), GatewayFailure> {
    let timestamp_ms = now_millis();
    let signature = self.key.sign(&GatewayRequest {
      method: "POST",
      path,
      timestamp_ms,
      body: &body,
    });
    let response = self
      .http
      .post(format!("{}{path}", self.base_url))
      .header("content-type", "application/json")
      .header(GATEWAY_SERVER_ID_HEADER, &self.server_id)
      .header(GATEWAY_TIMESTAMP_HEADER, timestamp_ms.to_string())
      .header(GATEWAY_SIGNATURE_HEADER, URL_SAFE_NO_PAD.encode(signature))
      .body(body)
      .send()
      .await
      .map_err(|e| GatewayFailure::Provider(ProviderError::Unavailable(e.without_url().to_string())))?;
    let status = response.status();
    if status.is_success() {
      return Ok(());
    }
    let maybe_code = response
      .json::<GatewayErrorBody>()
      .await
      .ok()
      .map(|body| body.error_code);
    Err(classify(status.as_u16(), maybe_code))
  }
}

impl TokenSender for GatewaySender {
  fn send<'a>(
    &'a self,
    token: &'a str,
    message: &'a PushMessage,
  ) -> BoxFuture<'a, Result<(), ProviderError>> {
    Box::pin(async move {
      let mut request_id = [0u8; GATEWAY_REQUEST_ID_LEN];
      OsRng.fill_bytes(&mut request_id);
      self
        .client
        .push(&GatewayPushRequest {
          request_id: Base64Bytes::new(request_id.to_vec()),
          provider: self.provider.clone(),
          kind: message.kind.clone(),
          urgency: match message.urgency {
            Urgency::Normal => GatewayUrgency::Normal,
            Urgency::High => GatewayUrgency::High,
          },
          ttl_secs: u32::try_from(message.ttl.as_secs()).unwrap_or(u32::MAX),
          token: token.to_owned(),
          sealed: Base64Bytes::new(message.sealed.clone()),
        })
        .await
    })
  }
}

/// Reads the server's gateway key from a base64url string (32 bytes).
pub fn parse_gateway_key(encoded: &str) -> Result<GatewayServerKey, ProviderError> {
  let bytes = Zeroizing::new(
    URL_SAFE_NO_PAD
      .decode(encoded.trim())
      .map_err(|_| ProviderError::Invalid("the gateway key is not base64url".into()))?,
  );
  let secret = SymmetricKey::from_slice(&bytes)
    .ok_or_else(|| ProviderError::Invalid("the gateway key must be 32 bytes".into()))?;
  Ok(GatewayServerKey::from_secret(&secret))
}

/// Loads the key stored at `path`, or creates the file (owner-only) with a fresh key. The ID the
/// gateway knows the server by follows from the key, so the file is the server's identity there.
pub fn load_or_create_gateway_key(path: &Path) -> std::io::Result<GatewayServerKey> {
  match std::fs::read_to_string(path) {
    Ok(text) => {
      return parse_gateway_key(&Zeroizing::new(text))
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()));
    },
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
    Err(error) => return Err(error),
  }
  let key = GatewayServerKey::generate(&mut OsRng);
  let encoded = Zeroizing::new(URL_SAFE_NO_PAD.encode(key.to_secret().expose_secret()));
  let mut options = std::fs::OpenOptions::new();
  options.write(true).create_new(true);
  #[cfg(unix)]
  std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
  options.open(path)?.write_all(encoded.as_bytes())?;
  Ok(key)
}

enum GatewayFailure {
  /// The gateway does not know our key (yet).
  UnknownServer,
  Provider(ProviderError),
}

impl From<GatewayFailure> for ProviderError {
  fn from(failure: GatewayFailure) -> Self {
    match failure {
      // Registering did not help: the gateway keeps refusing us.
      GatewayFailure::UnknownServer => ProviderError::Rejected(401),
      GatewayFailure::Provider(error) => error,
    }
  }
}

/// Maps a gateway error response. Only `token_gone` makes the server forget a registration; a
/// refusal of the server itself (blocked, bad signature, over quota) must never delete devices.
fn classify(status: u16, maybe_code: Option<GatewayErrorCode>) -> GatewayFailure {
  let failure = match (status, &maybe_code) {
    (_, Some(GatewayErrorCode::UnknownServer)) => return GatewayFailure::UnknownServer,
    (410, _) | (_, Some(GatewayErrorCode::TokenGone)) => ProviderError::Gone,
    (
      _,
      Some(
        GatewayErrorCode::RateLimited
        | GatewayErrorCode::QuotaExceeded
        | GatewayErrorCode::TokenRateLimited
        | GatewayErrorCode::ProviderUnavailable,
      ),
    )
    | (429 | 500..=599, _) => ProviderError::Unavailable(format!("gateway status {status}")),
    _ => {
      warn!(status, code = ?maybe_code.as_ref().map(GatewayErrorCode::as_str), "the push gateway refused the push");
      ProviderError::Rejected(status)
    },
  };
  GatewayFailure::Provider(failure)
}

fn now_millis() -> u64 {
  SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  fn class(status: u16, code: Option<GatewayErrorCode>) -> ProviderError {
    match classify(status, code) {
      GatewayFailure::Provider(error) => error,
      GatewayFailure::UnknownServer => ProviderError::Rejected(0),
    }
  }

  #[test]
  fn only_a_dead_token_makes_the_server_forget_the_device() {
    assert!(matches!(
      class(410, Some(GatewayErrorCode::TokenGone)),
      ProviderError::Gone
    ));
    for refusal in [
      GatewayErrorCode::ServerBlocked,
      GatewayErrorCode::Unauthorized,
      GatewayErrorCode::ProviderRejected,
      GatewayErrorCode::InvalidRequest,
    ] {
      assert!(matches!(class(403, Some(refusal)), ProviderError::Rejected(403)));
    }
  }

  #[test]
  fn limits_and_outages_are_retryable() {
    for (status, code) in [
      (429, Some(GatewayErrorCode::RateLimited)),
      (429, Some(GatewayErrorCode::QuotaExceeded)),
      (429, Some(GatewayErrorCode::TokenRateLimited)),
      (502, Some(GatewayErrorCode::ProviderUnavailable)),
      (503, None),
    ] {
      assert!(matches!(class(status, code), ProviderError::Unavailable(_)));
    }
  }

  #[test]
  fn the_key_file_is_created_once_and_read_back() {
    let path = std::env::temp_dir().join(format!("teamcraft-gateway-key-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let created = load_or_create_gateway_key(&path).unwrap();
    let loaded = load_or_create_gateway_key(&path).unwrap();
    assert_eq!(created.public_key(), loaded.public_key());
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt as _;
      assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
      );
    }
    std::fs::write(&path, "not a key").unwrap();
    assert!(load_or_create_gateway_key(&path).is_err());
    std::fs::remove_file(&path).unwrap();
  }

  #[test]
  fn keys_of_the_wrong_length_are_refused() {
    assert!(parse_gateway_key(&URL_SAFE_NO_PAD.encode([1u8; 31])).is_err());
    assert!(parse_gateway_key("%%%").is_err());
  }

  #[test]
  fn an_unknown_server_triggers_registration() {
    assert!(matches!(
      classify(401, Some(GatewayErrorCode::UnknownServer)),
      GatewayFailure::UnknownServer
    ));
  }
}
