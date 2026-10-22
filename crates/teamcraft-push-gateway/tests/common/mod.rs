//! A running gateway against local fake APNs and FCM, and a client that signs requests like a
//! TeamCraft server does.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use rand_core::{OsRng, RngCore};
use serde_json::{Value, json};
use teamcraft_crypto::push_gateway::{GatewayRequest, GatewayServerKey};
use teamcraft_proto::{
  GATEWAY_PUSH_PATH, GATEWAY_REGISTER_PATH, GATEWAY_SERVER_ID_HEADER, GATEWAY_SIGNATURE_HEADER,
  GATEWAY_TIMESTAMP_HEADER,
};
use teamcraft_push_gateway::app::{AppState, api_router, internal_router};
use teamcraft_push_gateway::config::GatewayConfig;
use teamcraft_push_gateway::providers::Providers;
use teamcraft_push_gateway::store::Store;
use teamcraft_push_providers::{
  ApnsSender, ApnsSettings, FcmSender, FcmSettings, ServiceAccount, TokenSender,
};

// The fake APNs and FCM of the server's push tests (HTTP/2 without TLS, recording requests).
#[path = "../../../teamcraft-server/tests/common/push_fakes.rs"]
pub mod push_fakes;

pub use push_fakes::Fake;

pub const APNS_TOKEN: &str = "ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12";
pub const FCM_TOKEN: &str = "fcm-registration-token";

pub struct Gateway {
  pub url: String,
  pub internal_url: String,
  pub state: AppState,
  pub apns: Fake,
  pub fcm: Fake,
  http: reqwest::Client,
}

/// One server's identity at the gateway.
pub struct Client {
  pub key: GatewayServerKey,
  pub server_id: String,
}

pub fn default_config() -> GatewayConfig {
  let mut config = GatewayConfig::default();
  config.database.path = ":memory:".into();
  // Block and limit changes apply at once.
  config.database.server_cache_secs = 0;
  config
}

impl Gateway {
  pub async fn start() -> Self {
    Self::start_with(default_config()).await
  }

  pub async fn start_with(config: GatewayConfig) -> Self {
    let (apns, apns_url) = push_fakes::apns().await;
    let (fcm, fcm_url) = push_fakes::fcm().await;
    let timeout = Duration::from_secs(5);
    let apns_sender: Arc<dyn TokenSender> = Arc::new(
      ApnsSender::new(ApnsSettings {
        key_pem: push_fakes::APNS_KEY_PEM.to_owned().into(),
        key_id: push_fakes::APNS_KEY_ID.into(),
        team_id: push_fakes::APNS_TEAM_ID.into(),
        bundle_id: push_fakes::APNS_BUNDLE_ID.into(),
        base_url: apns_url,
        alert_text: "New message".into(),
        timeout,
      })
      .unwrap(),
    );
    let fcm_sender: Arc<dyn TokenSender> = Arc::new(
      FcmSender::new(FcmSettings {
        account: ServiceAccount::from_json(&push_fakes::fcm_service_account_json(&fcm_url)).unwrap(),
        base_url: fcm_url,
        timeout,
      })
      .unwrap(),
    );
    let providers = Providers {
      maybe_apns: Some(apns_sender),
      maybe_fcm: Some(fcm_sender),
    };
    Self::start_with_providers(config, providers, apns, fcm).await
  }

  pub async fn start_with_providers(
    config: GatewayConfig,
    providers: Providers,
    apns: Fake,
    fcm: Fake,
  ) -> Self {
    let store = Store::open(&config.database.path).unwrap();
    let state = AppState::new(config, store, providers);
    let url = serve(api_router(state.clone()).into_make_service_with_connect_info::<SocketAddr>()).await;
    let internal_url =
      serve(internal_router(state.clone()).into_make_service_with_connect_info::<SocketAddr>()).await;
    Self {
      url,
      internal_url,
      state,
      apns,
      fcm,
      http: reqwest::Client::new(),
    }
  }

  /// A fresh server key, registered.
  pub async fn register_client(&self) -> Client {
    let client = Client::new();
    let response = self.register(&client).await;
    assert_eq!(response.status(), 200, "{:?}", response.text().await);
    client
  }

  pub async fn register(&self, client: &Client) -> reqwest::Response {
    let body = json!({ "public_key": STANDARD.encode(client.key.public_key().as_bytes()) });
    self
      .post_signed(
        GATEWAY_REGISTER_PATH,
        &client.key,
        None,
        body.to_string().into_bytes(),
        now_ms(),
      )
      .await
  }

  /// A signed push request for APNs.
  pub async fn push(&self, client: &Client, body: &Value) -> reqwest::Response {
    self
      .post_signed(
        GATEWAY_PUSH_PATH,
        &client.key,
        Some(&client.server_id),
        body.to_string().into_bytes(),
        now_ms(),
      )
      .await
  }

  /// Any body, key, server ID and timestamp, signed as given: for tests of the checks themselves.
  pub async fn post_signed(
    &self,
    path: &str,
    key: &GatewayServerKey,
    maybe_server_id: Option<&str>,
    body: Vec<u8>,
    timestamp_ms: u64,
  ) -> reqwest::Response {
    let signature = key.sign(&GatewayRequest {
      method: "POST",
      path,
      timestamp_ms,
      body: &body,
    });
    let mut request = self
      .http
      .post(format!("{}{path}", self.url))
      .header(GATEWAY_TIMESTAMP_HEADER, timestamp_ms.to_string())
      .header(GATEWAY_SIGNATURE_HEADER, URL_SAFE_NO_PAD.encode(signature))
      .header("content-type", "application/json")
      .body(body);
    if let Some(server_id) = maybe_server_id {
      request = request.header(GATEWAY_SERVER_ID_HEADER, server_id);
    }
    request.send().await.unwrap()
  }

  pub async fn get(&self, path: &str) -> reqwest::Response {
    self.http.get(format!("{}{path}", self.url)).send().await.unwrap()
  }

  pub async fn get_internal(&self, path: &str) -> reqwest::Response {
    self
      .http
      .get(format!("{}{path}", self.internal_url))
      .send()
      .await
      .unwrap()
  }
}

impl Client {
  pub fn new() -> Self {
    let key = GatewayServerKey::generate(&mut OsRng);
    let server_id = key.public_key().server_id();
    Self { key, server_id }
  }
}

/// A push request body with a fresh request ID.
pub fn push_body(provider: &str, kind: &str, token: &str) -> Value {
  let mut request_id = [0u8; 16];
  OsRng.fill_bytes(&mut request_id);
  json!({
    "request_id": STANDARD.encode(request_id),
    "provider": provider,
    "kind": kind,
    "urgency": "high",
    "ttl_secs": 60,
    "token": token,
    "sealed": STANDARD.encode(SEALED),
  })
}

/// Stands in for a sealed wake-up; the gateway treats it as opaque bytes.
pub const SEALED: &[u8] = b"\x01opaque sealed wake-up bytes";

pub fn now_ms() -> u64 {
  u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis()).unwrap()
}

async fn serve(
  service: axum::extract::connect_info::IntoMakeServiceWithConnectInfo<axum::Router, SocketAddr>,
) -> String {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let address = listener.local_addr().unwrap();
  tokio::spawn(async move {
    axum::serve(listener, service).await.unwrap();
  });
  format!("http://{address}")
}
