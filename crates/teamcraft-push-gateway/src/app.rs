//! The gateway's HTTP API (docs/push-gateway.md §3): server registration, signed push requests, the
//! public info document, and the internal health and metrics endpoints.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use teamcraft_crypto::push_gateway::{GatewayPublicKey, GatewayRequest};
use teamcraft_proto::{
  GATEWAY_INFO_PATH, GATEWAY_PUSH_PATH, GATEWAY_REGISTER_PATH, GATEWAY_REQUEST_ID_LEN,
  GATEWAY_SERVER_ID_HEADER, GATEWAY_SIGNATURE_HEADER, GATEWAY_TIMESTAMP_HEADER, GatewayErrorBody,
  GatewayErrorCode, GatewayInfoResponse, GatewayLimits, GatewayPushRequest, GatewayPushResponse,
  GatewayRegisterRequest, GatewayRegisterResponse, GatewayUrgency, PushProvider,
};
use teamcraft_push_providers::{ProviderError, PushMessage, Urgency};
use tracing::{debug, warn};

use crate::config::{GatewayConfig, RegistrationMode};
use crate::limits::{DAY_MS, Limited, Limiter, Outcome, ServerLimits};
use crate::metrics::{Metrics, PushResult, Refusal};
use crate::providers::Providers;
use crate::store::{BlockState, ServerRecord, Store};

const MAX_TOKEN_CHARS: usize = 4096;

/// Everything the handlers share.
#[derive(Clone)]
pub struct AppState {
  pub config: Arc<GatewayConfig>,
  pub store: Store,
  pub limiter: Arc<Limiter>,
  pub providers: Providers,
  pub metrics: Arc<Metrics>,
  servers: Arc<Mutex<HashMap<String, (ServerRecord, u64)>>>,
}

impl AppState {
  pub fn new(config: GatewayConfig, store: Store, providers: Providers) -> Self {
    Self {
      config: Arc::new(config),
      store,
      limiter: Arc::default(),
      providers,
      metrics: Arc::default(),
      servers: Arc::default(),
    }
  }

  /// A server's record, from the cache while it is fresh. A blocked server is only noticed once
  /// the cache entry expires (`database.server_cache_secs`).
  async fn server(&self, server_id: &str, now_ms: u64) -> Result<Option<ServerRecord>, ApiError> {
    let ttl_ms = self.config.database.server_cache_secs * 1000;
    if let Some((record, cached_ms)) = lock(&self.servers).get(server_id)
      && now_ms.saturating_sub(*cached_ms) < ttl_ms
    {
      return Ok(Some(record.clone()));
    }
    let maybe_record = self.store.get_server(server_id).await.map_err(|error| {
      warn!(%error, "reading a server record failed");
      ApiError::internal()
    })?;
    let mut cache = lock(&self.servers);
    match &maybe_record {
      Some(record) => {
        cache.insert(server_id.to_owned(), (record.clone(), now_ms));
      },
      None => {
        cache.remove(server_id);
      },
    }
    Ok(maybe_record)
  }

  fn limits_of(&self, record: &ServerRecord) -> ServerLimits {
    let defaults = &self.config.limits;
    ServerLimits {
      pushes_per_second: record
        .limits
        .maybe_pushes_per_second
        .unwrap_or(defaults.pushes_per_second),
      burst: record.limits.maybe_burst.unwrap_or(defaults.burst),
      pushes_per_day: record
        .limits
        .maybe_pushes_per_day
        .unwrap_or(defaults.pushes_per_day),
    }
  }

  fn public_limits(&self) -> GatewayLimits {
    let limits = &self.config.limits;
    GatewayLimits {
      pushes_per_second: limits.pushes_per_second,
      burst: limits.burst,
      pushes_per_day: limits.pushes_per_day,
      pushes_per_token_per_minute: limits.pushes_per_token_per_minute,
    }
  }
}

/// The public API.
pub fn api_router(state: AppState) -> Router {
  Router::new()
    .route(GATEWAY_PUSH_PATH, post(push_handler))
    .route(GATEWAY_REGISTER_PATH, post(register_handler))
    .route(GATEWAY_INFO_PATH, get(info_handler))
    .with_state(state)
}

/// `/healthz` and `/metrics`, for the operator's network only.
pub fn internal_router(state: AppState) -> Router {
  Router::new()
    .route("/healthz", get(health_handler))
    .route("/metrics", get(metrics_handler))
    .with_state(state)
}

async fn push_handler(
  State(state): State<AppState>,
  headers: HeaderMap,
  body: Body,
) -> Result<Json<GatewayPushResponse>, ApiError> {
  let now_ms = now_millis();
  let limits = state.config.limits;
  let body = read_body(body, limits.max_body_bytes)
    .await
    .map_err(|error| state.refuse(error))?;
  let record = authenticate(&state, &headers, GATEWAY_PUSH_PATH, &body, now_ms)
    .await
    .map_err(|error| state.refuse(error))?;

  let request: GatewayPushRequest = serde_json::from_slice(&body)
    .map_err(|_| state.refuse(ApiError::invalid("the body is not a push request")))?;
  let message = validate(&state, &request).map_err(|error| state.refuse(error))?;
  let sender = state
    .providers
    .get(&request.provider)
    .ok_or_else(|| {
      state.refuse(ApiError::new(
        GatewayErrorCode::ProviderNotConfigured,
        "this provider is not configured here",
      ))
    })?
    .clone();

  let request_id = <[u8; GATEWAY_REQUEST_ID_LEN]>::try_from(request.request_id.as_slice())
    .map_err(|_| state.refuse(ApiError::invalid("request_id must be 16 bytes")))?;
  let server_id = record.server_id.as_str();
  state
    .limiter
    .admit_request_id(server_id, request_id, limits.timestamp_window_secs * 2000, now_ms)
    .map_err(|limited| state.limited(limited))?;
  admit(&state, &record, &request.token, now_ms)
    .await
    .map_err(|error| state.refuse(error))?;

  let result = sender.send(&request.token, &message).await;
  let (outcome, push_result) = match &result {
    Ok(()) => (Outcome::Accepted, PushResult::Accepted),
    Err(ProviderError::Gone) => (Outcome::Gone, PushResult::TokenGone),
    Err(ProviderError::Rejected(_)) => (Outcome::Failed, PushResult::Rejected),
    Err(ProviderError::Unavailable(_)) => (Outcome::Failed, PushResult::Unavailable),
    Err(ProviderError::Invalid(_)) => (Outcome::Failed, PushResult::Invalid),
  };
  state.limiter.record(server_id, outcome);
  state.metrics.count_push(&request.provider, push_result);
  debug!(provider = %request.provider, result = ?push_result, "forwarded a push");
  match result {
    Ok(()) => Ok(Json(GatewayPushResponse { accepted: true })),
    Err(error) => Err(ApiError::from_provider(error)),
  }
}

async fn register_handler(
  State(state): State<AppState>,
  ConnectInfo(peer): ConnectInfo<SocketAddr>,
  headers: HeaderMap,
  body: Body,
) -> Result<Json<GatewayRegisterResponse>, ApiError> {
  let now_ms = now_millis();
  let body = read_body(body, state.config.limits.max_body_bytes)
    .await
    .map_err(|error| state.refuse(error))?;
  let request: GatewayRegisterRequest = serde_json::from_slice(&body)
    .map_err(|_| state.refuse(ApiError::invalid("the body is not a registration")))?;
  let key_bytes = <[u8; 32]>::try_from(request.public_key.as_slice())
    .map_err(|_| state.refuse(ApiError::invalid("the public key must be 32 bytes")))?;
  let public_key = GatewayPublicKey::from_bytes(key_bytes);
  // Proof of possession: the request is signed with the key it registers.
  verify(
    &state,
    &headers,
    &public_key,
    GATEWAY_REGISTER_PATH,
    &body,
    now_ms,
  )
  .map_err(|error| state.refuse(error))?;
  let server_id = public_key.server_id();

  let registration = &state.config.registration;
  let maybe_existing = state.server(&server_id, now_ms).await?;
  if let Some(existing) = &maybe_existing {
    if let BlockState::Blocked { .. } = existing.block {
      return Err(state.refuse(ApiError::new(
        GatewayErrorCode::ServerBlocked,
        "this server is blocked",
      )));
    }
  } else {
    if registration.mode == RegistrationMode::Closed {
      return Err(state.refuse(ApiError::new(
        GatewayErrorCode::RegistrationClosed,
        "this gateway only accepts servers its operator added",
      )));
    }
    let address = client_address(&state.config, &headers, peer.ip());
    state
      .limiter
      .admit_registration(
        address,
        registration.per_address_per_hour,
        registration.global_per_hour,
        now_ms,
      )
      .map_err(|limited| state.limited(limited))?;
    let count = state
      .store
      .count_servers()
      .await
      .map_err(|_| ApiError::internal())?;
    if count >= u64::from(registration.max_servers) {
      return Err(ApiError::new(
        GatewayErrorCode::RegistrationClosed,
        "this gateway has no room for more servers",
      ));
    }
    let created = state
      .store
      .insert_server(&server_id, key_bytes, now_ms_to_secs(now_ms))
      .await
      .map_err(|_| ApiError::internal())?;
    if created {
      state.metrics.count_registration();
    }
  }
  Ok(Json(GatewayRegisterResponse {
    server_id,
    limits: state.public_limits(),
  }))
}

async fn info_handler(State(state): State<AppState>) -> Json<GatewayInfoResponse> {
  Json(GatewayInfoResponse {
    version: env!("CARGO_PKG_VERSION").into(),
    providers: state.providers.configured(),
    is_registration_open: state.config.registration.mode == RegistrationMode::Open,
    limits: state.public_limits(),
  })
}

async fn health_handler(State(state): State<AppState>) -> Response {
  match state.store.ping().await {
    Ok(()) => "ok".into_response(),
    Err(error) => {
      warn!(%error, "the database does not answer");
      StatusCode::SERVICE_UNAVAILABLE.into_response()
    },
  }
}

async fn metrics_handler(State(state): State<AppState>) -> Response {
  let servers = state.store.count_servers().await.unwrap_or(0);
  (
    [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
    state.metrics.render(servers),
  )
    .into_response()
}

async fn read_body(body: Body, max_bytes: usize) -> Result<Vec<u8>, ApiError> {
  to_bytes(body, max_bytes)
    .await
    .map(|bytes| bytes.to_vec())
    .map_err(|_| ApiError::new(GatewayErrorCode::PayloadTooLarge, "the request body is too large"))
}

/// Checks the server header, the timestamp and the signature of a request; then the block list.
async fn authenticate(
  state: &AppState,
  headers: &HeaderMap,
  path: &str,
  body: &[u8],
  now_ms: u64,
) -> Result<ServerRecord, ApiError> {
  let server_id = header_str(headers, GATEWAY_SERVER_ID_HEADER)?;
  let record = state.server(server_id, now_ms).await?.ok_or_else(|| {
    ApiError::new(
      GatewayErrorCode::UnknownServer,
      "unknown server; register its key first",
    )
  })?;
  verify(
    state,
    headers,
    &GatewayPublicKey::from_bytes(record.public_key),
    path,
    body,
    now_ms,
  )?;
  if let BlockState::Blocked { .. } = record.block {
    return Err(ApiError::new(
      GatewayErrorCode::ServerBlocked,
      "this server is blocked",
    ));
  }
  Ok(record)
}

fn verify(
  state: &AppState,
  headers: &HeaderMap,
  public_key: &GatewayPublicKey,
  path: &str,
  body: &[u8],
  now_ms: u64,
) -> Result<(), ApiError> {
  let timestamp_ms: u64 = header_str(headers, GATEWAY_TIMESTAMP_HEADER)?
    .parse()
    .map_err(|_| ApiError::unauthorized("the timestamp is not a number"))?;
  let window_ms = state.config.limits.timestamp_window_secs * 1000;
  if timestamp_ms.abs_diff(now_ms) > window_ms {
    return Err(ApiError::unauthorized(
      "the timestamp is outside the accepted window; check the clock",
    ));
  }
  let signature = URL_SAFE_NO_PAD
    .decode(header_str(headers, GATEWAY_SIGNATURE_HEADER)?)
    .map_err(|_| ApiError::unauthorized("the signature is not base64url"))?;
  public_key
    .verify(
      &GatewayRequest {
        method: "POST",
        path,
        timestamp_ms,
        body,
      },
      &signature,
    )
    .map_err(|_| ApiError::unauthorized("the signature does not match"))
}

/// Field checks that need no state; the wake-up becomes the message for the push service.
fn validate(state: &AppState, request: &GatewayPushRequest) -> Result<PushMessage, ApiError> {
  let limits = &state.config.limits;
  if !matches!(request.provider, PushProvider::Apns | PushProvider::Fcm) {
    return Err(ApiError::invalid("provider must be apns or fcm"));
  }
  if request.token.is_empty() || request.token.len() > MAX_TOKEN_CHARS {
    return Err(ApiError::invalid("the token has an invalid length"));
  }
  if request.sealed.as_slice().is_empty() || request.sealed.as_slice().len() > limits.max_sealed_bytes {
    return Err(ApiError::new(
      GatewayErrorCode::PayloadTooLarge,
      "the sealed wake-up has an invalid size",
    ));
  }
  if matches!(request.kind, teamcraft_proto::PushKind::Unknown(_))
    || matches!(request.urgency, GatewayUrgency::Unknown(_))
  {
    return Err(ApiError::invalid("unknown kind or urgency"));
  }
  Ok(PushMessage {
    sealed: request.sealed.as_slice().to_vec(),
    kind: request.kind.clone(),
    urgency: match request.urgency {
      GatewayUrgency::High => Urgency::High,
      _ => Urgency::Normal,
    },
    ttl: std::time::Duration::from_secs(u64::from(request.ttl_secs.min(limits.max_ttl_secs))),
  })
}

/// The server's rate and daily quota, then the token's.
async fn admit(state: &AppState, record: &ServerRecord, token: &str, now_ms: u64) -> Result<(), ApiError> {
  let day = i64::try_from(now_ms / DAY_MS).unwrap_or(i64::MAX);
  let limits = state.limits_of(record);
  // The daily total comes from the store the first time a server shows up that day.
  if !state.limiter.is_day_loaded(&record.server_id, day) {
    let stored = state
      .store
      .usage(&record.server_id, day)
      .await
      .map_err(|_| ApiError::internal())?;
    state.limiter.seed_day(
      &record.server_id,
      day,
      stored.accepted + stored.gone + stored.failed,
    );
  }
  state
    .limiter
    .admit_server(&record.server_id, limits, day, now_ms)
    .map_err(|limited| state.limited(limited))?;
  state
    .limiter
    .admit_token(token, state.config.limits.pushes_per_token_per_minute, now_ms)
    .map_err(|limited| state.limited(limited))
}

fn client_address(config: &GatewayConfig, headers: &HeaderMap, peer: IpAddr) -> IpAddr {
  config
    .server
    .maybe_client_ip_header
    .as_deref()
    .and_then(|name| headers.get(name))
    .and_then(|value| value.to_str().ok())
    // The last entry is the one the trusted proxy appended.
    .and_then(|value| value.rsplit(',').next())
    .and_then(|value| value.trim().parse().ok())
    .unwrap_or(peer)
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str, ApiError> {
  headers
    .get(name)
    .and_then(|value| value.to_str().ok())
    .ok_or_else(|| ApiError::unauthorized("a signature header is missing"))
}

pub fn now_millis() -> u64 {
  SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
  })
}

fn now_ms_to_secs(now_ms: u64) -> i64 {
  i64::try_from(now_ms / 1000).unwrap_or(i64::MAX)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
  mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// An error response: status, stable code, message, and `Retry-After` when waiting helps.
#[derive(Debug)]
pub struct ApiError {
  status: StatusCode,
  body: GatewayErrorBody,
}

impl ApiError {
  fn new(code: GatewayErrorCode, message: &str) -> Self {
    let status = match code {
      GatewayErrorCode::InvalidRequest => StatusCode::BAD_REQUEST,
      GatewayErrorCode::Unauthorized | GatewayErrorCode::UnknownServer => StatusCode::UNAUTHORIZED,
      GatewayErrorCode::ServerBlocked | GatewayErrorCode::RegistrationClosed => StatusCode::FORBIDDEN,
      GatewayErrorCode::ReplayedRequest => StatusCode::CONFLICT,
      GatewayErrorCode::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
      GatewayErrorCode::RateLimited
      | GatewayErrorCode::QuotaExceeded
      | GatewayErrorCode::TokenRateLimited => StatusCode::TOO_MANY_REQUESTS,
      GatewayErrorCode::TokenGone => StatusCode::GONE,
      GatewayErrorCode::ProviderNotConfigured => StatusCode::NOT_IMPLEMENTED,
      GatewayErrorCode::ProviderRejected | GatewayErrorCode::ProviderUnavailable => StatusCode::BAD_GATEWAY,
      GatewayErrorCode::Unknown(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    Self {
      status,
      body: GatewayErrorBody {
        error_code: code,
        message: message.into(),
        maybe_retry_after_secs: None,
      },
    }
  }

  fn invalid(message: &str) -> Self {
    Self::new(GatewayErrorCode::InvalidRequest, message)
  }

  fn unauthorized(message: &str) -> Self {
    Self::new(GatewayErrorCode::Unauthorized, message)
  }

  fn internal() -> Self {
    Self {
      status: StatusCode::INTERNAL_SERVER_ERROR,
      body: GatewayErrorBody {
        error_code: GatewayErrorCode::Unknown("internal".into()),
        message: "internal error".into(),
        maybe_retry_after_secs: None,
      },
    }
  }

  fn from_provider(error: ProviderError) -> Self {
    // Provider details (status codes, reasons) stay in the gateway's log; the server only learns the class.
    match error {
      ProviderError::Gone => Self::new(
        GatewayErrorCode::TokenGone,
        "the push service reports the token as dead",
      ),
      ProviderError::Rejected(_) => Self::new(
        GatewayErrorCode::ProviderRejected,
        "the push service rejected the push",
      ),
      ProviderError::Unavailable(_) => Self::new(
        GatewayErrorCode::ProviderUnavailable,
        "the push service is unavailable",
      ),
      ProviderError::Invalid(_) => Self::invalid("the push service does not accept this token"),
    }
  }

  fn with_retry_after(mut self, secs: u32) -> Self {
    self.body.maybe_retry_after_secs = Some(secs);
    self
  }

  fn refusal(&self) -> Refusal {
    match self.body.error_code {
      GatewayErrorCode::Unauthorized => Refusal::Unauthorized,
      GatewayErrorCode::UnknownServer => Refusal::UnknownServer,
      GatewayErrorCode::ServerBlocked => Refusal::Blocked,
      GatewayErrorCode::ReplayedRequest => Refusal::Replay,
      GatewayErrorCode::RateLimited => Refusal::RateLimited,
      GatewayErrorCode::QuotaExceeded => Refusal::QuotaExceeded,
      GatewayErrorCode::TokenRateLimited => Refusal::TokenLimited,
      GatewayErrorCode::ProviderNotConfigured => Refusal::NotConfigured,
      _ => Refusal::Malformed,
    }
  }
}

impl AppState {
  /// Counts a refusal and passes the error on.
  fn refuse(&self, error: ApiError) -> ApiError {
    self.metrics.count_refusal(error.refusal());
    error
  }

  fn limited(&self, limited: Limited) -> ApiError {
    let error = match limited {
      Limited::ServerRate { retry_after_secs } => {
        ApiError::new(GatewayErrorCode::RateLimited, "too many pushes; slow down")
          .with_retry_after(retry_after_secs)
      },
      Limited::DailyQuota { retry_after_secs } => ApiError::new(
        GatewayErrorCode::QuotaExceeded,
        "the daily quota of this server is used up",
      )
      .with_retry_after(retry_after_secs),
      Limited::Token { retry_after_secs } => ApiError::new(
        GatewayErrorCode::TokenRateLimited,
        "this device receives too many pushes",
      )
      .with_retry_after(retry_after_secs),
      Limited::Replay => ApiError::new(
        GatewayErrorCode::ReplayedRequest,
        "this request ID was used already",
      ),
    };
    self.refuse(error)
  }
}

impl IntoResponse for ApiError {
  fn into_response(self) -> Response {
    let maybe_retry = self.body.maybe_retry_after_secs;
    let mut response = (self.status, Json(self.body)).into_response();
    if let Some(secs) = maybe_retry
      && let Ok(value) = HeaderValue::from_str(&secs.to_string())
    {
      response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
  }
}
