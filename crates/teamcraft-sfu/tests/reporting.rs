//! The SFU reports joins and leaves to the server's internal endpoints with its registration token.

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
  net::{Ipv6Addr, SocketAddr},
  sync::{Arc, Mutex},
  time::Duration,
};

use axum::{Json, Router, extract::State, http::HeaderMap, routing::post};
use teamcraft_media::{
  bwe::BweConfig,
  call_engine::{CallEngineConfig, CallEngineEvent},
  call_native::{NativeCall, NativeCallConfig},
};
use teamcraft_proto::{MediaCapabilities, SfuParticipantReport};
use teamcraft_sfu::{
  auth::DevTokenVerifier,
  config::SfuConfig,
  reporting::HttpReporter,
  server::{CALL_PATH, Sfu},
};
use tokio::net::TcpListener;
use wtransport::Identity;

const REGISTRATION_TOKEN: &str = "sfu-registration-test-token";

type Reports = Arc<Mutex<Vec<(String, Option<String>, SfuParticipantReport)>>>;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn joins_and_leaves_are_reported_with_the_registration_token() {
  let reports = Reports::default();
  let server_url = start_fake_server(Arc::clone(&reports)).await;
  let (port, certificate_hash) = start_sfu(&server_url).await;

  let engine = CallEngineConfig {
    user_id: "7".parse().unwrap(),
    device_id: "70".parse().unwrap(),
    capabilities: MediaCapabilities::default(),
    bwe: BweConfig::default(),
  };
  let sfu_url = format!(
    "https://{}{CALL_PATH}",
    SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port)
  );
  let mut config = NativeCallConfig::new(sfu_url, "dev:5:7:70".into(), engine);
  config.maybe_certificate_hash = Some(certificate_hash);
  let mut call = NativeCall::join(config).await.unwrap();
  // Joined means the SFU accepted (and reported) us.
  let joined = tokio::time::timeout(Duration::from_secs(5), async {
    while let Some(event) = call.next_event().await {
      if matches!(event, CallEngineEvent::Joined { .. }) {
        return true;
      }
    }
    false
  })
  .await
  .unwrap();
  assert!(joined);
  call.leave().await.unwrap();

  let recorded = wait_for(&reports, 2).await;
  let kinds: Vec<&str> = recorded.iter().map(|(kind, _, _)| kind.as_str()).collect();
  assert_eq!(kinds, vec!["joined", "left"]);
  for (_, authorization, report) in &recorded {
    assert_eq!(
      authorization.as_deref(),
      Some(format!("Bearer {REGISTRATION_TOKEN}").as_str())
    );
    assert_eq!(report.call_id, "5".parse().unwrap());
    assert_eq!(report.user_id, "7".parse().unwrap());
    assert_eq!(report.device_id, "70".parse().unwrap());
  }
}

async fn wait_for(reports: &Reports, count: usize) -> Vec<(String, Option<String>, SfuParticipantReport)> {
  tokio::time::timeout(Duration::from_secs(5), async {
    loop {
      let recorded = reports.lock().unwrap().clone();
      if recorded.len() >= count {
        return recorded;
      }
      tokio::time::sleep(Duration::from_millis(20)).await;
    }
  })
  .await
  .expect("reports did not arrive")
}

/// A stand-in for the server's internal call endpoints that records what it receives.
async fn start_fake_server(reports: Reports) -> String {
  async fn record(
    kind: &'static str,
    State(reports): State<Reports>,
    headers: HeaderMap,
    Json(report): Json<SfuParticipantReport>,
  ) {
    let authorization = headers
      .get("authorization")
      .and_then(|value| value.to_str().ok())
      .map(str::to_owned);
    reports
      .lock()
      .unwrap()
      .push((kind.to_owned(), authorization, report));
  }
  let router = Router::new()
    .route(
      "/api/internal/v1/calls/participants/joined",
      post(|state, headers, body| record("joined", state, headers, body)),
    )
    .route(
      "/api/internal/v1/calls/participants/left",
      post(|state, headers, body| record("left", state, headers, body)),
    )
    .with_state(reports);
  let listener = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).await.unwrap();
  let address = listener.local_addr().unwrap();
  tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
  format!("http://{address}")
}

async fn start_sfu(server_url: &str) -> (u16, [u8; 32]) {
  let config = SfuConfig {
    bind_addr: SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
    ..SfuConfig::default()
  };
  let identity = Identity::self_signed(["localhost", "127.0.0.1", "::1"]).unwrap();
  let sfu = Sfu::bind(config, identity, Arc::new(DevTokenVerifier))
    .unwrap()
    .with_reporter(Arc::new(HttpReporter::new(server_url, REGISTRATION_TOKEN)));
  let port = sfu.local_addr().unwrap().port();
  let certificate_hash = *sfu.certificate_hash().as_ref();
  tokio::spawn(sfu.run());
  (port, certificate_hash)
}
