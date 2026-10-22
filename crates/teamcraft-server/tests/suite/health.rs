//! The container health probe against a running router (`--features integration`).
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::time::Duration;

use teamcraft_ops::{ProbeError, probe};
use teamcraft_server::testing::TestApp;

#[tokio::test]
async fn the_probe_reaches_healthz_and_readyz_and_reports_missing_paths() {
  let Some(app) = TestApp::start().await else {
    return;
  };
  let addr: SocketAddr = app.base_url.trim_start_matches("http://").parse().unwrap();

  let results = tokio::task::spawn_blocking(move || {
    let timeout = Duration::from_secs(3);
    (
      probe(addr, "/healthz", timeout),
      probe(addr, "/readyz", timeout),
      probe(addr, "/nonexistent", timeout),
    )
  })
  .await
  .unwrap();

  assert!(results.0.is_ok(), "{:?}", results.0);
  assert!(results.1.is_ok(), "{:?}", results.1);
  assert!(matches!(
    results.2,
    Err(ProbeError::Unhealthy { status: 404, .. })
  ));
  app.stop().await;
}
