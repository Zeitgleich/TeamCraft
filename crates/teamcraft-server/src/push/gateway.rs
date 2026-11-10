//! Push gateway client (docs/push-gateway.md): sends the sealed wake-ups for APNs and FCM
//! registrations to a gateway that holds the credentials of the published store apps. The gateway
//! sees the device token and ciphertext, never a user, device or key. The server proves who it is
//! with a signature of its own key pair, registered once at the gateway.

use std::io::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand_core::{OsRng, RngCore};
use teamcraft_crypto::push_gateway::{GatewayRequest, GatewayServerKey};
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{
  BOUND_TOKEN_PREFIX, Base64Bytes, GATEWAY_INFO_PATH, GATEWAY_PUSH_PATH, GATEWAY_REGISTER_PATH,
  GATEWAY_REQUEST_ID_LEN, GATEWAY_SERVER_ID_HEADER, GATEWAY_SIGNATURE_HEADER, GATEWAY_TIMESTAMP_HEADER,
  GatewayErrorBody, GatewayErrorCode, GatewayPushRequest, GatewayRegisterRequest, GatewayUrgency, PushKind,
  PushProvider,
};
use tracing::{info, warn};
use zeroize::Zeroizing;

use super::sender::{PushError, PushMessage, PushSender, PushTarget, Urgency, retry_after};
use crate::auth::BoxFuture;
use crate::dependencies::DependencyGuard;

/// How long to hold back after the gateway said "too many" without saying how long.
const DEFAULT_BACKOFF: Duration = Duration::from_secs(60);
/// The longest pause a gateway's answer can impose (its daily quota resets within a day).
const MAX_BACKOFF: Duration = Duration::from_secs(24 * 60 * 60);
/// How long to wait before registering again after the gateway refused to register us.
const REGISTER_RETRY: Duration = Duration::from_secs(5 * 60);
/// Largest sealed token a registration may carry (the gateway refuses more).
const MAX_BOUND_TOKEN_BYTES: usize = 4096;
/// How long a gateway's list of kinds counts: a gateway update reaches the server within this.
const KINDS_MAX_AGE: Duration = Duration::from_secs(60 * 60);
/// How long after a failed read of the list it is read again. Meanwhile the last list read counts, and
/// with none read yet a newer kind goes to the retry queue.
const KINDS_RETRY: Duration = Duration::from_secs(5 * 60);
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
  /// The gateway's breaker, once [`Self::watch_health`] set it.
  guard: OnceLock<Arc<DependencyGuard>>,
  /// The kinds the gateway's `/v1/info` listed last, and when it is read again.
  kinds: tokio::sync::Mutex<GatewayKinds>,
}

#[derive(Default)]
struct GatewayKinds {
  /// What the last successful read listed; a failed read keeps it.
  maybe_listed: Option<Vec<PushKind>>,
  maybe_read_again_at: Option<Instant>,
}

/// The part of the gateway's `/v1/info` this server reads.
#[derive(serde::Deserialize)]
struct GatewayInfoKinds {
  #[serde(default)]
  kinds: Vec<PushKind>,
}

/// The kind a gateway is told. A meeting's notices go exactly like a message (a visible alert, high
/// priority); the real kind is in the sealed wake-up, so a gateway from before them delivers them too.
fn gateway_kind(kind: &PushKind) -> PushKind {
  match kind {
    PushKind::MeetingReminder | PushKind::MeetingStart => PushKind::Message,
    other => other.clone(),
  }
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
      .map_err(|e| PushError::unavailable(e.without_url().to_string()))?;
    Ok(Arc::new(Self {
      base_url: settings.url.trim_end_matches('/').to_owned(),
      accept_unbound_tokens: settings.accept_unbound_tokens,
      server_id: settings.key.public_key().server_id(),
      key: settings.key,
      http,
      paused_until: Mutex::new(None),
      registration: tokio::sync::Mutex::new(None),
      guard: OnceLock::new(),
      kinds: tokio::sync::Mutex::new(GatewayKinds::default()),
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

  /// Whether the gateway delivers `kind`, as `/v1/info` says (its api.md: servers send only the listed
  /// ones). The kinds of its first version are always taken as offered; a newer kind is only sent to
  /// a gateway that listed it, so an older gateway means no such push, not a refused request each time.
  /// Read at most once per [`KINDS_MAX_AGE`], and after a failure not before [`KINDS_RETRY`]. A failed
  /// read keeps the list read before; with none read yet the push is unavailable for now, so the
  /// retry queue sends it once the list is known rather than dropping it.
  async fn offers_kind(&self, kind: &PushKind) -> Result<bool, PushError> {
    if !matches!(kind, PushKind::ScheduledSeal | PushKind::Sync) {
      return Ok(true);
    }
    let mut known = self.kinds.lock().await;
    let now = Instant::now();
    if known.maybe_read_again_at.is_none_or(|at| now >= at) {
      match self.read_kinds().await {
        Ok(kinds) => {
          known.maybe_listed = Some(kinds);
          known.maybe_read_again_at = Some(now + KINDS_MAX_AGE);
        },
        Err(error) => {
          warn!(%error, "the push gateway's kinds could not be read");
          known.maybe_read_again_at = Some(now + KINDS_RETRY);
        },
      }
    }
    match &known.maybe_listed {
      Some(kinds) => Ok(kinds.contains(kind)),
      None => Err(PushError::unavailable_for(
        "the push gateway's kinds are not known yet",
        known
          .maybe_read_again_at
          .map(|at| at.saturating_duration_since(Instant::now())),
      )),
    }
  }

  async fn read_kinds(&self) -> Result<Vec<PushKind>, reqwest::Error> {
    let info: GatewayInfoKinds = self
      .http
      .get(format!("{}{GATEWAY_INFO_PATH}", self.base_url))
      .send()
      .await?
      .error_for_status()?
      .json()
      .await?;
    Ok(info.kinds)
  }

  /// Asks the gateway whether it answers (its `/v1/info`, which needs no signature), within
  /// `timeout`, for the health checks (#1647). The answer feeds the gateway's breaker, so an outage
  /// shows, and ends, on an instance that sends no push. `None` while the breaker is open and its
  /// next probe isn't due; otherwise whether the gateway answered with a success.
  pub async fn probe(&self, timeout: Duration) -> Option<bool> {
    let maybe_guard = self.guard.get();
    if let Some(guard) = maybe_guard
      && guard.check().is_err()
    {
      return None;
    }
    let request = self
      .http
      .get(format!("{}{GATEWAY_INFO_PATH}", self.base_url))
      .timeout(timeout)
      .send();
    let is_answered = matches!(request.await, Ok(response) if response.status().is_success());
    if let Some(guard) = maybe_guard {
      if is_answered {
        guard.record_success();
      } else {
        guard.record_failure();
      }
    }
    Some(is_answered)
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
      return Err(PushError::unavailable_for(
        "the push gateway asked to wait",
        Some(until.saturating_duration_since(Instant::now())),
      ));
    }
    let maybe_guard = self.guard.get();
    if let Some(guard) = maybe_guard {
      // Down: not even sent; the durable retry queue tries again after `Retry-After`.
      guard
        .check()
        .map_err(|open| PushError::unavailable_for(open.to_string(), Some(open.retry_after)))?;
    }
    let body = serde_json::to_vec(request)
      .map_err(|_| PushError::Invalid("encoding the gateway request failed".into()))?;
    let result = self.push_body(body).await;
    if let Some(guard) = maybe_guard {
      match &result {
        Err(PushError::Unavailable { .. }) => guard.record_failure(),
        // Delivered, or an answer about this push or this server: the gateway works.
        Ok(()) | Err(_) => guard.record_success(),
      }
    }
    result
  }

  /// The push's health is reported to `guard` from now on (issue #743): `/readyz` shows the
  /// gateway as down after several failures, and pushes fail fast (into the retry queue) until it
  /// is probed again. Set once; later calls are ignored.
  pub fn watch_health(&self, guard: Arc<DependencyGuard>) {
    // Ignored if already set: one registry per process.
    let _ = self.guard.set(guard);
  }

  async fn push_body(&self, body: Vec<u8>) -> Result<(), PushError> {
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
      .map_err(|e| GatewayFailure::Provider(PushError::unavailable(e.without_url().to_string())))?;
    let status = response.status();
    if status.is_success() {
      return Ok(());
    }
    let maybe_header_wait = retry_after(response.headers());
    let maybe_body = read_error_body(response).await;
    let maybe_body_wait = maybe_body
      .as_ref()
      .and_then(|body| body.maybe_retry_after_secs)
      .map(|secs| Duration::from_secs(u64::from(secs)));
    Err(classify(
      status.as_u16(),
      maybe_body.map(|body| body.error_code),
      maybe_header_wait.or(maybe_body_wait),
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
      if !self.client.offers_kind(&message.kind).await? {
        // Not an error to retry: this gateway can't deliver it, and the note says what that means.
        return Err(PushError::NotConfigured);
      }
      let (maybe_token, maybe_bound_token) = self.token_fields(&target.token)?;
      let mut request_id = [0u8; GATEWAY_REQUEST_ID_LEN];
      OsRng.fill_bytes(&mut request_id);
      self
        .client
        .push(&GatewayPushRequest {
          request_id: Base64Bytes::new(request_id.to_vec()),
          provider: self.provider.clone(),
          kind: gateway_kind(&message.kind),
          urgency: match message.urgency {
            Urgency::Normal => GatewayUrgency::Normal,
            Urgency::High => GatewayUrgency::High,
          },
          ttl_secs: u32::try_from(message.ttl.as_secs()).unwrap_or(u32::MAX),
          maybe_token,
          maybe_bound_token,
          sealed: Base64Bytes::new(message.sealed.clone()),
          maybe_collapse_id: None,
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
      GatewayFailure::Throttled { status, retry_after } => {
        PushError::unavailable_for(format!("gateway status {status}"), Some(retry_after))
      },
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
fn classify(
  status: u16,
  maybe_code: Option<GatewayErrorCode>,
  maybe_retry_after: Option<Duration>,
) -> GatewayFailure {
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
      return GatewayFailure::Throttled {
        retry_after: maybe_retry_after.unwrap_or(DEFAULT_BACKOFF),
        status,
      };
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
    // Apple or Google down (the gateway passes on how long they asked to wait), or the gateway
    // itself overloaded.
    (_, Some(GatewayErrorCode::ProviderUnavailable)) | (429 | 500..=599, _) => {
      PushError::unavailable_for(format!("gateway status {status}"), maybe_retry_after)
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
    match classify(status, code, None) {
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
        classify(429, Some(code), Some(Duration::from_secs(30))),
        GatewayFailure::Throttled { retry_after, .. } if retry_after == Duration::from_secs(30)
      ));
    }
    for (status, code) in [
      (502, Some(GatewayErrorCode::ProviderUnavailable)),
      (503, None),
      (429, None),
    ] {
      assert!(matches!(class(status, code), PushError::Unavailable { .. }));
    }
    // Without a hint, the server's own limit waits the default; an outage leaves it to the queue.
    assert!(matches!(
      classify(429, Some(GatewayErrorCode::RateLimited), None),
      GatewayFailure::Throttled { retry_after, .. } if retry_after == DEFAULT_BACKOFF
    ));
    let wait = Some(Duration::from_secs(90));
    assert!(matches!(
      classify(502, Some(GatewayErrorCode::ProviderUnavailable), wait),
      GatewayFailure::Provider(PushError::Unavailable { maybe_retry_after, .. }) if maybe_retry_after == wait
    ));
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
      classify(401, Some(GatewayErrorCode::UnknownServer), None),
      GatewayFailure::UnknownServer
    ));
  }
}
