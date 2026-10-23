//! A stand-in for a push gateway (docs/push-gateway.md; the contract is the gateway repository's
//! docs/api.md): it checks the server's signatures like a real gateway does, remembers registered keys,
//! opens sealed device tokens (and refuses them for another server, after expiry, or once revoked),
//! records every push and answers with a scripted reply. The real gateway is tested in its own
//! repository against the same JSON fixtures.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use teamcraft_crypto::push_gateway::{
  BoundToken, GatewayPublicKey, GatewayRequest, open_bound_token, token_key_id, token_public_key,
};
use teamcraft_proto::{
  GATEWAY_INFO_PATH, GATEWAY_PUSH_PATH, GATEWAY_REGISTER_PATH, GATEWAY_REVOKE_PATH, GATEWAY_SERVER_ID_HEADER,
  GATEWAY_SIGNATURE_HEADER, GATEWAY_TIMESTAMP_HEADER, GatewayPushRequest, GatewayRegisterRequest,
  GatewayRevokeRequest,
};

/// How far a request's timestamp may differ from the clock, as in the real gateway's defaults.
const TIMESTAMP_WINDOW_MS: u64 = 300_000;
/// The seed of the fake gateway's token key (any 32 bytes; the key is derived from it).
const TOKEN_SEED: [u8; 32] = [0x42; 32];

/// A push the fake accepted: the request as sent, and the opened sealed token inside it.
#[derive(Clone)]
pub struct ReceivedPush {
  pub request: GatewayPushRequest,
  /// `None` for a raw token.
  pub maybe_bound: Option<BoundToken>,
}

#[derive(Default)]
struct Inner {
  known: HashMap<String, GatewayPublicKey>,
  registrations: usize,
  pushes: Vec<ReceivedPush>,
  /// Signed push requests that arrived, accepted or not.
  push_attempts: usize,
  revoked: Vec<Vec<u8>>,
  reply: Option<(u16, Value)>,
  accepts_unbound: bool,
}

#[derive(Clone, Default)]
pub struct FakeGateway {
  inner: Arc<Mutex<Inner>>,
}

impl FakeGateway {
  /// Starts the fake on a free local port; returns it and its base URL.
  pub async fn start() -> (Self, String) {
    let fake = Self::default();
    let router = Router::new()
      .route(GATEWAY_REGISTER_PATH, post(register))
      .route(GATEWAY_PUSH_PATH, post(push))
      .route(GATEWAY_INFO_PATH, get(info))
      .route(GATEWAY_REVOKE_PATH, post(revoke))
      .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (fake, url)
  }

  /// Signed push requests that arrived, accepted or answered with an error.
  pub fn push_attempts(&self) -> usize {
    self.inner.lock().unwrap().push_attempts
  }

  /// The pushes accepted so far, in order.
  pub fn pushes(&self) -> Vec<ReceivedPush> {
    self.inner.lock().unwrap().pushes.clone()
  }

  /// Waits for `count` pushes.
  pub async fn wait_for(&self, count: usize) -> Vec<ReceivedPush> {
    for _ in 0..100 {
      if self.pushes().len() >= count {
        break;
      }
      tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    self.pushes()
  }

  /// The public key apps seal device tokens to.
  pub fn token_public_key(&self) -> [u8; 32] {
    token_public_key(&TOKEN_SEED).unwrap()
  }

  /// Sealed tokens the apps revoked, in order.
  pub fn revocations(&self) -> Vec<Vec<u8>> {
    self.inner.lock().unwrap().revoked.clone()
  }

  /// Also accept raw device tokens, as a gateway in transition does.
  pub fn accept_unbound(&self) {
    self.inner.lock().unwrap().accepts_unbound = true;
  }

  pub fn registrations(&self) -> usize {
    self.inner.lock().unwrap().registrations
  }

  /// Forgets every registered key, as a gateway with a lost database.
  pub fn forget_servers(&self) {
    self.inner.lock().unwrap().known.clear();
  }

  /// Answers every following push with this status and error body (the push is not recorded);
  /// `None` goes back to accepting.
  pub fn set_reply(&self, maybe_reply: Option<(u16, Value)>) {
    self.inner.lock().unwrap().reply = maybe_reply;
  }
}

async fn register(State(fake): State<FakeGateway>, headers: HeaderMap, body: axum::body::Bytes) -> Response {
  let Ok(request) = serde_json::from_slice::<GatewayRegisterRequest>(&body) else {
    return error(400, "invalid_request");
  };
  let Ok(bytes) = <[u8; 32]>::try_from(request.public_key.as_slice()) else {
    return error(400, "invalid_request");
  };
  let key = GatewayPublicKey::from_bytes(bytes);
  // Proof of possession: signed with the key it registers.
  if !signature_fits(&headers, &key, GATEWAY_REGISTER_PATH, &body) {
    return error(401, "unauthorized");
  }
  let mut inner = fake.inner.lock().unwrap();
  inner.known.insert(key.server_id(), key);
  inner.registrations += 1;
  axum::Json(json!({
    "server_id": key.server_id(),
    "limits": {"pushes_per_second": 50, "burst": 200, "pushes_per_day": 200000, "pushes_per_token_per_minute": 30}
  }))
  .into_response()
}

async fn push(State(fake): State<FakeGateway>, headers: HeaderMap, body: axum::body::Bytes) -> Response {
  let server_id = headers
    .get(GATEWAY_SERVER_ID_HEADER)
    .and_then(|value| value.to_str().ok())
    .unwrap_or_default();
  let Some(key) = fake.inner.lock().unwrap().known.get(server_id).copied() else {
    return error(401, "unknown_server");
  };
  if !signature_fits(&headers, &key, GATEWAY_PUSH_PATH, &body) {
    return error(401, "unauthorized");
  }
  let Ok(request) = serde_json::from_slice::<GatewayPushRequest>(&body) else {
    return error(400, "invalid_request");
  };
  let mut inner = fake.inner.lock().unwrap();
  inner.push_attempts += 1;
  if let Some((status, body)) = inner.reply.clone() {
    return (StatusCode::from_u16(status).unwrap(), axum::Json(body)).into_response();
  }
  // Like the real gateway: a sealed token is opened and must name this server and be valid; a raw
  // one is only taken during a transition.
  let maybe_bound = match (&request.maybe_token, &request.maybe_bound_token) {
    (Some(_), None) if inner.accepts_unbound => None,
    (Some(_), None) => return error(410, "token_unbound"),
    (None, Some(blob)) => {
      if inner
        .revoked
        .iter()
        .any(|revoked| revoked.as_slice() == blob.as_slice())
      {
        return error(410, "token_revoked");
      }
      let Ok(bound) = open_bound_token(&TOKEN_SEED, blob.as_slice()) else {
        return error(410, "token_invalid");
      };
      if bound.server_id != server_id {
        return error(410, "token_server_mismatch");
      }
      let now = u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis()).unwrap();
      if bound.expires_at_ms <= now {
        return error(410, "token_expired");
      }
      Some(bound)
    },
    _ => return error(400, "invalid_request"),
  };
  inner.pushes.push(ReceivedPush { request, maybe_bound });
  axum::Json(json!({"accepted": true})).into_response()
}

async fn info(State(fake): State<FakeGateway>) -> Response {
  let public = fake.token_public_key();
  let accepts_unbound = fake.inner.lock().unwrap().accepts_unbound;
  axum::Json(json!({
    "version": "1.0.0",
    "providers": ["apns", "fcm"],
    "is_registration_open": true,
    "limits": {"pushes_per_second": 50, "burst": 200, "pushes_per_day": 200000, "pushes_per_token_per_minute": 30},
    "token_key": {
      "key_id": token_key_id(&public).iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
      "algorithm": "hpke-x25519-hkdf-sha256-chacha20poly1305",
      "public_key": STANDARD.encode(public),
    },
    "are_bound_tokens_required": !accepts_unbound,
    "max_token_lifetime_secs": 7_776_000,
  }))
  .into_response()
}

async fn revoke(State(fake): State<FakeGateway>, body: axum::body::Bytes) -> Response {
  let Ok(request) = serde_json::from_slice::<GatewayRevokeRequest>(&body) else {
    return error(400, "invalid_request");
  };
  fake
    .inner
    .lock()
    .unwrap()
    .revoked
    .push(request.bound_token.as_slice().to_vec());
  axum::Json(json!({"revoked": true})).into_response()
}

fn signature_fits(headers: &HeaderMap, key: &GatewayPublicKey, path: &str, body: &[u8]) -> bool {
  let header = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
  let (Some(timestamp), Some(signature)) =
    (header(GATEWAY_TIMESTAMP_HEADER), header(GATEWAY_SIGNATURE_HEADER))
  else {
    return false;
  };
  let (Ok(timestamp_ms), Ok(signature)) = (timestamp.parse::<u64>(), URL_SAFE_NO_PAD.decode(signature))
  else {
    return false;
  };
  let now = u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis()).unwrap();
  timestamp_ms.abs_diff(now) <= TIMESTAMP_WINDOW_MS
    && key
      .verify(
        &GatewayRequest {
          method: "POST",
          path,
          timestamp_ms,
          body,
        },
        &signature,
      )
      .is_ok()
}

fn error(status: u16, code: &str) -> Response {
  (
    StatusCode::from_u16(status).unwrap(),
    axum::Json(json!({"error_code": code, "message": code})),
  )
    .into_response()
}
