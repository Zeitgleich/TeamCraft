//! Push gateway client (docs/push-gateway.md): sends the sealed wake-ups for APNs and FCM
//! registrations to a gateway that holds the credentials of the published store apps. The gateway
//! sees the device token and ciphertext, never a user, device or key. The server proves who it is
//! with a signature of its own key pair, registered once at the gateway.

use std::io::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand_core::{OsRng, RngCore};
use teamcraft_crypto::push_gateway::{GatewayRequest, GatewayServerKey};
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{
  BOUND_TOKEN_PREFIX, Base64Bytes, GATEWAY_PUSH_PATH, GATEWAY_REGISTER_PATH, GATEWAY_REQUEST_ID_LEN,
  GATEWAY_SERVER_ID_HEADER, GATEWAY_SIGNATURE_HEADER, GATEWAY_TIMESTAMP_HEADER, GatewayErrorBody,
  GatewayErrorCode, GatewayPushRequest, GatewayRegisterRequest, GatewayUrgency, PushProvider,
};
use tracing::{info, warn};
use zeroize::Zeroizing;

use super::sender::{PushError, PushMessage, PushSender, PushTarget, Urgency};
use crate::auth::BoxFuture;

/// How long to hold back after the gateway said "too many" without saying how long.
const DEFAULT_BACKOFF: Duration = Duration::from_secs(60);
/// The longest pause a gateway's answer can impose (its daily quota resets within a day).
const MAX_BACKOFF: Duration = Duration::from_secs(24 * 60 * 60);
/// How long to wait before registering again after the gateway refused to register us.
const REGISTER_RETRY: Duration = Duration::from_secs(5 * 60);
/// Largest sealed token a registration may carry (the gateway refuses more).
const MAX_BOUND_TOKEN_BYTES: usize = 4096;
/// Most of an error response that is read.
const MAX_ERROR_BODY_BYTES: usize = 4096;

/// What the client needs to reach a gateway.
pub struct GatewaySettings {
  /// Base URL, e.g. `https://gateway.example.com`. Plain `http://` only in tests.
  pub url: String,
  pub key: GatewayServerKey,
  pub timeout: Duration,
  /// Also send device tokens that the apps did not seal (a transition for old apps; the gateway
  /// must accept them too). Off by default.
  pub accept_unbound_tokens: bool,
}

/// One server's connection to one gateway; the APNs and FCM senders share it.
pub struct GatewayClient {
  base_url: String,
  accept_unbound_tokens: bool,
  key: GatewayServerKey,
  server_id: String,
  http: reqwest::Client,
  /// The gateway asked this server to wait (rate limit, daily quota): no pushes are sent before.
  paused_until: Mutex<Option<Instant>>,
  /// One registration at a time; after a refusal, none before the time stored.
  registration: tokio::sync::Mutex<Option<Instant>>,
}

/// The sender of one provider (APNs or FCM) behind a [`GatewayClient`].
pub struct GatewaySender {
  client: Arc<GatewayClient>,
  provider: PushProvider,
}

impl GatewayClient {
  pub fn new(settings: GatewaySettings) -> Result<Arc<Self>, PushError> {
    let http = reqwest::Client::builder()
      .timeout(settings.timeout)
      .connect_timeout(settings.timeout.min(Duration::from_secs(5)))
      .redirect(reqwest::redirect::Policy::none())
      .build()
      .map_err(|e| PushError::Unavailable(e.without_url().to_string()))?;
    Ok(Arc::new(Self {
      base_url: settings.url.trim_end_matches('/').to_owned(),
      accept_unbound_tokens: settings.accept_unbound_tokens,
      server_id: settings.key.public_key().server_id(),
      key: settings.key,
      http,
      paused_until: Mutex::new(None),
      registration: tokio::sync::Mutex::new(None),
    }))
  }

  /// The sender for APNs or FCM registrations.
  pub fn sender(self: &Arc<Self>, provider: PushProvider) -> GatewaySender {
    GatewaySender {
      client: self.clone(),
      provider,
    }
  }

  /// The gateway's base URL.
  pub fn url(&self) -> &str {
    &self.base_url
  }

  /// Whether registrations may carry device tokens that the app did not seal.
  pub fn accepts_unbound_tokens(&self) -> bool {
    self.accept_unbound_tokens
  }

  /// The ID the gateway knows this server by (safe to log: it is a hash of the public key).
  pub fn server_id(&self) -> &str {
    &self.server_id
  }

  /// Makes the gateway know this server's public key. Idempotent; safe to repeat.
  pub async fn register(&self) -> Result<(), PushError> {
    let body = serde_json::to_vec(&GatewayRegisterRequest {
      public_key: Base64Bytes::new(self.key.public_key().as_bytes().to_vec()),
    })
    .map_err(|_| PushError::Invalid("encoding the registration failed".into()))?;
    self.post(GATEWAY_REGISTER_PATH, body).await?;
    info!(server_id = %self.server_id, "registered at the push gateway");
    Ok(())
  }

  async fn push(&self, request: &GatewayPushRequest) -> Result<(), PushError> {
    if let Some(until) = *lock(&self.paused_until)
      && Instant::now() < until
    {
      // Not even sent: the gateway asked us to wait, and ignoring that gets a server blocked.
      return Err(PushError::Unavailable("the push gateway asked to wait".into()));
    }
    let body = serde_json::to_vec(request)
      .map_err(|_| PushError::Invalid("encoding the gateway request failed".into()))?;
    let first = self.post(GATEWAY_PUSH_PATH, body.clone()).await;
    let result = match first {
      // The gateway lost us (new database) or this is the first push: register and try once more.
      Err(GatewayFailure::UnknownServer) => match self.register_once().await {
        Ok(()) => self.post(GATEWAY_PUSH_PATH, body).await,
        Err(error) => Err(GatewayFailure::Provider(error)),
      },
      other => other,
    };
    if let Err(GatewayFailure::Throttled { retry_after, .. }) = &result {
      let until = Instant::now() + (*retry_after).min(MAX_BACKOFF);
      warn!(
        seconds = retry_after.as_secs(),
        "the push gateway asked this server to wait"
      );
      *lock(&self.paused_until) = Some(until);
    }
    result.map_err(Into::into)
  }

  /// Registers once for all pushes that found the gateway didn't know us: the others wait for
  /// that registration instead of each sending their own. After a refusal, pushes fail fast for a
  /// while instead of costing two requests each.
  async fn register_once(&self) -> Result<(), PushError> {
    let started = Instant::now();
    let mut retry_at = self.registration.lock().await;
    if let Some(at) = *retry_at
      && at > started
    {
      return Err(PushError::Rejected(401));
    }
    match self.register().await {
      Ok(()) => {
        *retry_at = None;
        Ok(())
      },
      Err(error) => {
        *retry_at = Some(Instant::now() + REGISTER_RETRY);
        Err(error)
      },
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
      .map_err(|e| GatewayFailure::Provider(PushError::Unavailable(e.without_url().to_string())))?;
    let status = response.status();
    if status.is_success() {
      return Ok(());
    }
    let maybe_header_wait = response
      .headers()
      .get(reqwest::header::RETRY_AFTER)
      .and_then(|value| value.to_str().ok())
      .and_then(|value| value.trim().parse::<u64>().ok())
      .map(Duration::from_secs);
    let maybe_body = read_error_body(response).await;
    let maybe_body_wait = maybe_body
      .as_ref()
      .and_then(|body| body.maybe_retry_after_secs)
      .map(|secs| Duration::from_secs(u64::from(secs)));
    let retry_after = maybe_header_wait.or(maybe_body_wait).unwrap_or(DEFAULT_BACKOFF);
    Err(classify(
      status.as_u16(),
      maybe_body.map(|body| body.error_code),
      retry_after,
    ))
  }
}

impl PushSender for GatewaySender {
  fn requires_bound_token(&self) -> bool {
    !self.client.accept_unbound_tokens
  }

  fn is_gateway(&self) -> bool {
    true
  }

  fn send<'a>(
    &'a self,
    target: &'a PushTarget,
    message: &'a PushMessage,
  ) -> BoxFuture<'a, Result<(), PushError>> {
    Box::pin(async move {
      let (maybe_token, maybe_bound_token) = self.token_fields(&target.token)?;
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
          maybe_token,
          maybe_bound_token,
          sealed: Base64Bytes::new(message.sealed.clone()),
        })
        .await
    })
  }
}

impl GatewaySender {
  /// The token of a registration as the gateway wants it: the sealed blob of a token the app bound
  /// to this server (`tcgw1:...`), or the raw token while unbound ones are accepted. A raw token
  /// of a registration made before binding is dead weight: the app has to register again, sealed.
  fn token_fields(&self, token: &str) -> Result<(Option<String>, Option<Base64Bytes>), PushError> {
    if let Some(encoded) = token.strip_prefix(BOUND_TOKEN_PREFIX) {
      let blob = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| PushError::Invalid("not a sealed push token".into()))?;
      if blob.is_empty() || blob.len() > MAX_BOUND_TOKEN_BYTES {
        return Err(PushError::Invalid("not a sealed push token".into()));
      }
      return Ok((None, Some(Base64Bytes::new(blob))));
    }
    if !self.client.accept_unbound_tokens {
      return Err(PushError::Gone);
    }
    // Checked like the direct senders do: a junk registration never reaches the gateway.
    let is_token = match self.provider {
      PushProvider::Apns => super::apns::is_device_token(token),
      PushProvider::Fcm => super::fcm::is_registration_token(token),
      _ => false,
    };
    if is_token {
      Ok((Some(token.to_owned()), None))
    } else {
      Err(PushError::Invalid("not a push token of this provider".into()))
    }
  }
}

/// Reads the server's gateway key from a base64url string (32 bytes).
pub fn parse_gateway_key(encoded: &str) -> Result<GatewayServerKey, PushError> {
  let bytes = Zeroizing::new(
    URL_SAFE_NO_PAD
      .decode(encoded.trim())
      .map_err(|_| PushError::Invalid("the gateway key is not base64url".into()))?,
  );
  let secret = SymmetricKey::from_slice(&bytes)
    .ok_or_else(|| PushError::Invalid("the gateway key must be 32 bytes".into()))?;
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
  /// This server is over its rate or daily quota: wait this long before the next push.
  Throttled {
    retry_after: Duration,
    status: u16,
  },
  Provider(PushError),
}

impl From<GatewayFailure> for PushError {
  fn from(failure: GatewayFailure) -> Self {
    match failure {
      // Registering did not help: the gateway keeps refusing us.
      GatewayFailure::UnknownServer => PushError::Rejected(401),
      GatewayFailure::Throttled { status, .. } => PushError::Unavailable(format!("gateway status {status}")),
      GatewayFailure::Provider(error) => error,
    }
  }
}

/// Maps a gateway error response. Only the gateway's token codes (`token_gone` and the sealed-token
/// refusals below) make the server forget a registration (a bare 410 may come from a proxy in
/// front of the gateway); a refusal of the server itself
/// (blocked, bad signature, over quota) must never delete devices. Problems of one token
/// (`invalid_request`, `token_rate_limited`) say nothing about the gateway and don't count
/// against its health.
fn classify(status: u16, maybe_code: Option<GatewayErrorCode>, retry_after: Duration) -> GatewayFailure {
  let failure = match (status, &maybe_code) {
    (_, Some(GatewayErrorCode::UnknownServer)) => return GatewayFailure::UnknownServer,
    // The registration is no use for this server any more (the push service says so, or the sealed
    // token is expired, revoked, bound to another server, unopenable or not sealed at all): the app
    // registers again.
    (
      _,
      Some(
        GatewayErrorCode::TokenGone
        | GatewayErrorCode::TokenUnbound
        | GatewayErrorCode::TokenExpired
        | GatewayErrorCode::TokenInvalid
        | GatewayErrorCode::TokenRevoked
        | GatewayErrorCode::TokenServerMismatch,
      ),
    ) => PushError::Gone,
    (_, Some(GatewayErrorCode::RateLimited | GatewayErrorCode::QuotaExceeded)) => {
      return GatewayFailure::Throttled { retry_after, status };
    },
    (_, Some(GatewayErrorCode::InvalidRequest)) => {
      PushError::Invalid("the push gateway refused the request".into())
    },
    (_, Some(GatewayErrorCode::TokenRateLimited)) => {
      PushError::Invalid("too many pushes to this device".into())
    },
    // The gateway's own credentials or setup: the operator's to fix; shows as failing, deletes nothing.
    (_, Some(GatewayErrorCode::ProviderRejected | GatewayErrorCode::ProviderNotConfigured)) => {
      warn!(
        status,
        "the push gateway cannot reach the push service with its credentials"
      );
      PushError::Rejected(status)
    },
    (_, Some(GatewayErrorCode::ProviderUnavailable)) | (429 | 500..=599, _) => {
      PushError::Unavailable(format!("gateway status {status}"))
    },
    _ => {
      warn!(status, code = ?maybe_code.as_ref().map(GatewayErrorCode::as_str), "the push gateway refused the push");
      PushError::Rejected(status)
    },
  };
  GatewayFailure::Provider(failure)
}

/// The error body, read up to [`MAX_ERROR_BODY_BYTES`]; `None` if absent or unreadable.
async fn read_error_body(mut response: reqwest::Response) -> Option<GatewayErrorBody> {
  let mut bytes = Vec::new();
  while let Ok(Some(chunk)) = response.chunk().await {
    bytes.extend_from_slice(&chunk);
    if bytes.len() > MAX_ERROR_BODY_BYTES {
      return None;
    }
  }
  serde_json::from_slice(&bytes).ok()
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
  // A poisoned lock only means another task panicked while holding it; the value stays usable.
  mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn now_millis() -> u64 {
  SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  fn class(status: u16, code: Option<GatewayErrorCode>) -> PushError {
    match classify(status, code, DEFAULT_BACKOFF) {
      GatewayFailure::Provider(error) => error,
      failure @ GatewayFailure::Throttled { .. } => failure.into(),
      GatewayFailure::UnknownServer => PushError::Rejected(0),
    }
  }

  #[test]
  fn only_a_dead_token_makes_the_server_forget_the_device() {
    assert!(matches!(
      class(410, Some(GatewayErrorCode::TokenGone)),
      PushError::Gone
    ));
    // A bare 410 (a proxy, a wrong URL) deletes nothing.
    assert!(!matches!(class(410, None), PushError::Gone));
    for refusal in [
      GatewayErrorCode::ServerBlocked,
      GatewayErrorCode::Unauthorized,
      GatewayErrorCode::ProviderRejected,
    ] {
      assert!(matches!(class(403, Some(refusal)), PushError::Rejected(403)));
    }
  }

  #[test]
  fn problems_of_one_token_do_not_count_against_the_gateway() {
    for code in [
      GatewayErrorCode::InvalidRequest,
      GatewayErrorCode::TokenRateLimited,
    ] {
      assert!(matches!(class(429, Some(code)), PushError::Invalid(_)));
    }
  }

  #[test]
  fn limits_of_the_server_make_it_wait_and_outages_are_retryable() {
    for code in [GatewayErrorCode::RateLimited, GatewayErrorCode::QuotaExceeded] {
      assert!(matches!(
        classify(429, Some(code), Duration::from_secs(30)),
        GatewayFailure::Throttled { retry_after, .. } if retry_after == Duration::from_secs(30)
      ));
    }
    for (status, code) in [
      (502, Some(GatewayErrorCode::ProviderUnavailable)),
      (503, None),
      (429, None),
    ] {
      assert!(matches!(class(status, code), PushError::Unavailable(_)));
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
      classify(401, Some(GatewayErrorCode::UnknownServer), DEFAULT_BACKOFF),
      GatewayFailure::UnknownServer
    ));
  }
}
