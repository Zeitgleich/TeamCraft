//! `GET /metrics` against a running server (operations.md, "Metrics"): real users, conversations and
//! tokens go through the counted router, and none of them shows up in the output.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common::*;

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use teamcraft_ops::metrics::{bind_metrics, spawn_metrics_endpoint, with_http_metrics};
use teamcraft_server::listener::bind_listener;
use teamcraft_server::metrics::{BackgroundJob, ServerMetrics};
use teamcraft_server::router::router;
use teamcraft_server::state::AppState;
use teamcraft_server::testing::{TestApp, TestUser};

const CHECK_TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::test]
async fn the_output_shows_what_happened_and_nobody_it_happened_to() {
  let Some(app) = TestApp::start().await else {
    return;
  };
  let alice = TestUser::new("metrics-alice", &["owner"]);
  let token = app.token(&alice);
  let counted = serve_counted(&app.state).await;
  let client = reqwest::Client::new();

  // A signed-in request, a made-up route with something that looks like a secret, and a probe.
  let me = client
    .get(format!("{counted}/api/v1/me"))
    .bearer_auth(&token)
    .send()
    .await
    .unwrap();
  assert!(me.status().is_success(), "{}", me.status());
  let user_id = me.json::<serde_json::Value>().await.unwrap()["id"]
    .as_str()
    .unwrap()
    .to_owned();
  let space = create_space(&app, &alice, "metrics-secret-space", "open").await;
  let channel = create_channel(&app, &alice, &space, "metrics-secret-channel", "public").await;
  let messages = client
    .get(format!("{counted}/api/v1/conversations/{channel}/messages"))
    .bearer_auth(&token)
    .send()
    .await
    .unwrap();
  assert!(messages.status().is_success(), "{}", messages.status());
  let made_up = format!("{counted}/api/v1/made-up-secret-route/471147114711");
  client.get(&made_up).bearer_auth(&token).send().await.unwrap();
  client.get(format!("{counted}/healthz")).send().await.unwrap();

  app
    .state
    .metrics
    .check_database(
      &app.state.database,
      CHECK_TIMEOUT,
      time::OffsetDateTime::now_utc(),
    )
    .await;
  let output = scrape(&app.state).await;

  assert!(output.contains(r#"teamcraft_http_requests_total{route_class="users",status="2xx"}"#));
  assert!(output.contains(r#"teamcraft_http_requests_total{route_class="messages",status="2xx"} 1"#));
  assert!(output.contains(r#"teamcraft_http_requests_total{route_class="other","#));
  assert!(output.contains(r#"teamcraft_http_requests_total{route_class="health",status="2xx"} 1"#));
  assert!(output.contains(r#"teamcraft_db_pool_connections{state="in_use"}"#));
  assert!(output.contains(r#"teamcraft_job_last_success_timestamp_seconds{job="database_check"}"#));
  assert!(
    !output.contains(r#"teamcraft_db_size_bytes 0"#),
    "the check read a size"
  );
  assert!(output.contains("teamcraft_push_retries_pending 0"));

  for secret in [
    token.as_str(),
    user_id.as_str(),
    alice.subject.as_str(),
    alice.username.as_str(),
    alice.email.as_str(),
    channel.as_str(),
    space.as_str(),
    "metrics-secret",
    "made-up-secret-route",
    "471147114711",
  ] {
    assert!(!output.contains(secret), "{secret} is in the output");
  }
  app.stop().await;
}

#[tokio::test]
async fn job_timestamps_stay_at_zero_until_a_job_succeeded() {
  let metrics = ServerMetrics::new().unwrap();
  let Some(app) = TestApp::start().await else {
    return;
  };
  let before = metrics.render(&app.state).unwrap();
  assert!(before.contains(r#"teamcraft_job_last_success_timestamp_seconds{job="retention"} 0"#));
  let now = time::OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
  metrics.job_succeeded(BackgroundJob::Retention, now);
  let after = metrics.render(&app.state).unwrap();
  assert!(after.contains(r#"teamcraft_job_last_success_timestamp_seconds{job="retention"} 1700000000"#));
  app.stop().await;
}

/// Serves the app the way `main` does with `[metrics]` on; its base URL.
async fn serve_counted(state: &AppState) -> String {
  let listener = bind_listener((Ipv4Addr::LOCALHOST, 0).into()).unwrap();
  let addr = listener.local_addr().unwrap();
  let app = with_http_metrics(router(state.clone()), state.metrics.http());
  tokio::spawn(async move {
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await
  });
  format!("http://{addr}")
}

/// One scrape of a metrics listener.
async fn scrape(state: &AppState) -> String {
  let listeners = bind_metrics(&[SocketAddr::from((Ipv4Addr::LOCALHOST, 0))]).unwrap();
  let addr = listeners[0].local_addr().unwrap();
  let scraped = state.clone();
  spawn_metrics_endpoint(listeners, move || scraped.metrics.render(&scraped));
  reqwest::get(format!("http://{addr}/metrics"))
    .await
    .unwrap()
    .error_for_status()
    .unwrap()
    .text()
    .await
    .unwrap()
}
