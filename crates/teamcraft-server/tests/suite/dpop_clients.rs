//! The client core's DPoP against the real server path (e2ee.md §3.1.2): the core's own proofs, nonce
//! memory, one retry and clock, on REST, the gateway and the relay, under each policy.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

use crate::common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use futures_util::{SinkExt, StreamExt};
use reqwest::StatusCode;
use reqwest::header::AUTHORIZATION;
use serde_json::{Value, json};
use teamcraft_core::api::dpop::{DPOP_HEADER, DPOP_NONCE_HEADER, DpopAnswer, DpopProver, SoftwareDpopSigner};
use teamcraft_core::api::{ApiError, HttpServerApi, ProvenToken, ServerApi};
use teamcraft_core::{LinkOffer, client::LinkWait};
use teamcraft_crypto::dpop::{DpopSigningKey, JwkThumbprint};
use teamcraft_proto::{
  DeviceActivity, DpopPolicy, ErrorCode, GATEWAY_PATH, GatewayClientMessage, GatewayServerMessage,
};
use teamcraft_server::testing::{TestApp, TestUser};
use tokio_tungstenite::tungstenite::Message;

const ME: &str = "/api/v1/me";
const SERVER_INFO_PATH: &str = "/api/v1/server-info";
/// Rounds of the round-trip measurement.
const MEASURE_ROUNDS: usize = 50;

macro_rules! app {
  ($start:expr) => {
    match $start.await {
      Some(app) => app,
      None => return,
    }
  };
}

/// A server that knows the address the test reaches it on.
async fn start() -> Option<TestApp> {
  let app = TestApp::start().await?;
  set_settings(&app, |settings| {
    settings.security.public_origins = vec![app.base_url.clone()];
  });
  Some(app)
}

fn set_settings(app: &TestApp, change: impl FnOnce(&mut teamcraft_server::state::Settings)) {
  let mut settings = (*app.state.settings.get()).clone();
  change(&mut settings);
  app.state.settings.set_current(settings);
}

fn set_policy(app: &TestApp, policy: DpopPolicy) {
  set_settings(app, |settings| settings.token_binding.dpop_policy = policy);
}

mod requests {
  use super::*;

  #[tokio::test]
  async fn a_bound_token_goes_with_a_proof_and_the_first_request_meets_the_challenge() {
    let app = app!(start());
    let device = Device::new(&app, &owner());
    assert!(!device.prover.has_nonce());
    device.api(&app).me().await.unwrap();
    assert!(device.prover.has_nonce());
    device.api(&app).me().await.unwrap();
    app.stop().await;
  }

  /// What a restart of the server looks like to a client: the nonce it remembers is unknown.
  #[tokio::test]
  async fn a_nonce_the_server_forgot_gets_one_retry() {
    let app = app!(start());
    let device = Device::new(&app, &owner());
    device.prover.remember_nonce("from-before-the-restart");
    device.api(&app).me().await.unwrap();
    app.stop().await;
  }

  #[tokio::test]
  async fn a_clock_an_hour_off_corrects_itself_from_the_answer() {
    let app = app!(start());
    let device = Device::new(&app, &owner());
    // The prover's idea of the server's time is an hour ahead; so are its proofs.
    let wrong = http_date_in_an_hour();
    device.prover.note_answer(DpopAnswer {
      maybe_nonce: None,
      maybe_date: Some(&wrong),
    });
    device.api(&app).me().await.unwrap();
    app.stop().await;
  }

  #[tokio::test]
  async fn a_replayed_proof_is_refused_with_the_key_on_the_device() {
    let app = app!(start());
    let device = Device::new(&app, &owner());
    device.api(&app).me().await.unwrap();
    let url = format!("{}{ME}", app.base_url);
    let proof = device
      .prover
      .proof("GET", &url, Some(&device.token))
      .await
      .unwrap();
    let send = || {
      app
        .http
        .get(&url)
        .header(AUTHORIZATION, format!("DPoP {}", device.token))
        .header(DPOP_HEADER, proof.as_str())
        .send()
    };
    assert_eq!(send().await.unwrap().status(), StatusCode::OK);
    let replayed = send().await.unwrap();
    assert_eq!(replayed.status(), StatusCode::UNAUTHORIZED);
    let body: Value = replayed.json().await.unwrap();
    assert_eq!(body["error_code"], "invalid_dpop_proof");
    app.stop().await;
  }

  #[tokio::test]
  async fn a_token_bound_to_a_key_this_device_lost_never_leaves() {
    let app = app!(start());
    let device = Device::new(&app, &owner());
    let lost = Device {
      token: device.token.clone(),
      ..Device::with_key(&app, &owner(), DpopSigningKey::generate())
    };
    let error = lost.api(&app).me().await.unwrap_err();
    assert!(matches!(error, ApiError::BoundKeyLost), "{error}");
    assert!(error.is_sign_in_required());
    app.stop().await;
  }

  /// A server that doesn't know the address it is reached on (operations.md, "When something goes
  /// wrong"): the session stays, and the app says what is wrong.
  #[tokio::test]
  async fn proofs_for_an_address_the_server_does_not_know_keep_the_session() {
    let app = app!(start());
    let device = Device::new(&app, &owner());
    set_settings(&app, |settings| {
      settings.security.public_origins = vec!["https://chat.example.com".to_owned()];
    });
    let error = device.api(&app).me().await.unwrap_err();
    assert_eq!(error.error_code(), Some(&ErrorCode::InvalidDpopProof), "{error}");
    assert!(!error.is_sign_in_required());
    assert!(error.is_transient());
    assert!(error.interception().is_some());
    app.stop().await;
  }
}

mod policies {
  use super::*;

  #[tokio::test]
  async fn an_unbound_token_works_until_the_policy_is_required() {
    let app = app!(start());
    let user = owner();
    let token = app.token_with(&user, now_unix(), &json!({}));
    let device = Device::with_token(token, DpopSigningKey::generate());
    for policy in [DpopPolicy::Off, DpopPolicy::Optional] {
      set_policy(&app, policy.clone());
      device
        .api(&app)
        .me()
        .await
        .unwrap_or_else(|error| panic!("{policy:?}: {error}"));
      // It went as a bearer token: nothing to learn a nonce for.
      assert!(!device.prover.has_nonce(), "{policy:?}");
    }
    set_policy(&app, DpopPolicy::Required);
    let error = device.api(&app).me().await.unwrap_err();
    assert_eq!(error.error_code(), Some(&ErrorCode::DpopRequired), "{error}");
    assert!(error.is_sign_in_required());
    app.stop().await;
  }

  #[tokio::test]
  async fn a_bound_token_with_its_proof_works_under_every_policy() {
    let app = app!(start());
    let device = Device::new(&app, &owner());
    for policy in [DpopPolicy::Off, DpopPolicy::Optional, DpopPolicy::Required] {
      set_policy(&app, policy.clone());
      device
        .api(&app)
        .me()
        .await
        .unwrap_or_else(|error| panic!("{policy:?}: {error}"));
    }
    app.stop().await;
  }

  /// An app without a key, such as the mobile apps for now: bound tokens don't reach it (they send
  /// no proof at the token endpoint), and a bearer token keeps working under `optional`.
  #[tokio::test]
  async fn a_client_without_a_key_works_as_bearer_under_optional() {
    let app = app!(start());
    let token = app.token_with(&owner(), now_unix(), &json!({}));
    let api = HttpServerApi::new(
      app.base_url.clone(),
      ProvenToken {
        token,
        maybe_prover: None,
      },
    );
    api.me().await.unwrap();
    app.stop().await;
  }
}

mod first_frames {
  use super::*;

  #[tokio::test]
  async fn the_gateway_takes_the_cores_proof_after_the_nonce_frame() {
    let app = app!(start());
    let user = owner();
    let device = Device::new(&app, &user);
    let device_id = register_device(&app, &user).await;
    let api = device.api(&app);

    let first = identify(&app, &api, &device.token, &device_id).await;
    let Some(GatewayServerMessage::DpopNonce { nonce }) = first.first() else {
      panic!("{first:?}");
    };
    api.note_dpop_nonce(nonce);
    let second = identify(&app, &api, &device.token, &device_id).await;
    assert!(
      second
        .iter()
        .any(|frame| matches!(frame, GatewayServerMessage::Ready { .. })),
      "{second:?}"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn an_unbound_token_has_no_proof_in_its_first_frame() {
    let app = app!(start());
    let token = app.token_with(&owner(), now_unix(), &json!({}));
    let device = Device::with_token(token, DpopSigningKey::generate());
    let proof = device
      .api(&app)
      .first_frame_proof(GATEWAY_PATH, &device.token)
      .await
      .unwrap();
    assert_eq!(proof, None);
    app.stop().await;
  }

  /// The relay's challenge is handled inside the join: one more connection, and it is in.
  #[tokio::test]
  async fn the_relay_is_joined_after_its_nonce_frame() {
    let app = app!(start());
    let device = Device::new(&app, &owner());
    assert!(!device.prover.has_nonce());
    let wait = LinkWait::join(&device.api(&app), &LinkOffer::generate()).await;
    assert!(wait.is_ok(), "{:?}", wait.err());
    assert!(device.prover.has_nonce());
    app.stop().await;
  }
}

mod server_info {
  use super::*;

  #[tokio::test]
  async fn it_offers_a_nonce_only_where_a_proof_would_pass() {
    let app = app!(start());
    let host = app.base_url.trim_start_matches("http://").to_owned();
    assert!(nonce_on(&app, &host).await.is_some());
    assert_eq!(nonce_on(&app, "other.example.com").await, None);

    set_policy(&app, DpopPolicy::Off);
    assert_eq!(nonce_on(&app, &host).await, None);
    set_policy(&app, DpopPolicy::Optional);

    set_settings(&app, |settings| settings.security.public_origins = Vec::new());
    assert_eq!(nonce_on(&app, &host).await, None);
    app.stop().await;
  }

  async fn nonce_on(app: &TestApp, host: &str) -> Option<String> {
    let response = app
      .http
      .get(format!("{}{SERVER_INFO_PATH}", app.base_url))
      .header(reqwest::header::HOST, host)
      .send()
      .await
      .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response
      .headers()
      .get(DPOP_NONCE_HEADER)
      .map(|value| value.to_str().unwrap().to_owned())
  }
}

/// The numbers in the pull request and docs/performance.md, printed with `--nocapture`.
mod measured {
  use super::*;

  #[tokio::test]
  async fn the_first_request_pays_one_round_trip_and_a_signature() {
    let app = app!(start());
    let user = owner();
    let mut first = Vec::new();
    let mut later = Vec::new();
    let mut bearer = Vec::new();
    let unbound = HttpServerApi::new(
      app.base_url.clone(),
      ProvenToken {
        token: app.token_with(&user, now_unix(), &json!({})),
        maybe_prover: None,
      },
    );
    unbound.me().await.unwrap();
    for _ in 0..MEASURE_ROUNDS {
      let device = Device::new(&app, &user);
      let api = device.api(&app);
      first.push(timed(api.me()).await);
      later.push(timed(api.me()).await);
      bearer.push(timed(unbound.me()).await);
    }
    let key = DpopSigningKey::generate();
    let signer = SoftwareDpopSigner::new(key);
    let prover = DpopProver::new(Arc::new(signer));
    let started = Instant::now();
    for _ in 0..MEASURE_ROUNDS {
      prover
        .proof("GET", "https://chat.example.com/api/v1/me", Some("token"))
        .await
        .unwrap();
    }
    let per_proof = started.elapsed() / u32::try_from(MEASURE_ROUNDS).unwrap();
    eprintln!(
      "GET /me on loopback, median: bearer {:?}, bound with a nonce {:?}, bound first request {:?}; \
       one proof {per_proof:?}",
      median(&mut bearer),
      median(&mut later),
      median(&mut first),
    );
    app.stop().await;
  }

  async fn timed<T>(request: impl Future<Output = Result<T, ApiError>>) -> Duration {
    let started = Instant::now();
    request.await.ok().unwrap();
    started.elapsed()
  }

  fn median(values: &mut [Duration]) -> Duration {
    values.sort_unstable();
    values[values.len() / 2]
  }
}

/// A device with its key and a token bound to it.
struct Device {
  prover: Arc<DpopProver>,
  token: String,
}

impl Device {
  fn new(app: &TestApp, user: &TestUser) -> Self {
    Self::with_key(app, user, DpopSigningKey::generate())
  }

  fn with_key(app: &TestApp, user: &TestUser, key: DpopSigningKey) -> Self {
    let bound: JwkThumbprint = key.thumbprint();
    let token = app.token_with(
      user,
      now_unix(),
      &json!({"typ": "DPoP", "cnf": {"jkt": bound.as_str()}}),
    );
    Self::with_token(token, key)
  }

  fn with_token(token: String, key: DpopSigningKey) -> Self {
    Self {
      prover: Arc::new(DpopProver::new(Arc::new(SoftwareDpopSigner::new(key)))),
      token,
    }
  }

  fn api(&self, app: &TestApp) -> HttpServerApi<ProvenToken> {
    HttpServerApi::new(
      app.base_url.clone(),
      ProvenToken {
        token: self.token.clone(),
        maybe_prover: Some(Arc::clone(&self.prover)),
      },
    )
  }
}

async fn identify(
  app: &TestApp,
  api: &HttpServerApi<ProvenToken>,
  token: &str,
  device_id: &str,
) -> Vec<GatewayServerMessage> {
  let (mut socket, _) = tokio_tungstenite::connect_async(&app.ws_url).await.unwrap();
  socket.next().await;
  let frame = GatewayClientMessage::Identify {
    maybe_dpop_proof: api.first_frame_proof(GATEWAY_PATH, token).await.unwrap(),
    token: token.to_owned(),
    device_id: serde_json::from_value(json!(device_id)).unwrap(),
    activity: DeviceActivity::Active,
  };
  socket
    .send(Message::text(serde_json::to_string(&frame).unwrap()))
    .await
    .unwrap();
  let mut frames = Vec::new();
  while let Ok(Some(Ok(frame))) = tokio::time::timeout(Duration::from_secs(5), socket.next()).await {
    let Message::Text(text) = frame else {
      continue;
    };
    let frame: GatewayServerMessage = serde_json::from_str(text.as_str()).unwrap();
    let is_last = matches!(
      frame,
      GatewayServerMessage::Ready { .. } | GatewayServerMessage::Error { .. }
    );
    frames.push(frame);
    if is_last {
      break;
    }
  }
  frames
}

fn now_unix() -> i64 {
  i64::try_from(common::now_secs()).unwrap()
}

fn http_date_in_an_hour() -> String {
  let now = time::OffsetDateTime::now_utc() + time::Duration::hours(1);
  let format = time::macros::format_description!(
    "[weekday repr:short], [day] [month repr:short] [year] [hour]:[minute]:[second] GMT"
  );
  now.format(&format).unwrap()
}
