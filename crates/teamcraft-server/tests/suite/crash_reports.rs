//! The crash report relay (docs/crash-reports.md): off by default, forwards envelopes to the
//! backend the admin set, and passes the backend's answer on in a form the apps act on.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode as AxumStatus, Uri};
use axum::routing::post;
use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_server::testing::TestApp;

const ENVELOPE: &str =
  "{\"event_id\":\"00000000000000000000000000000001\"}\n{\"type\":\"event\",\"length\":2}\n{}\n";

macro_rules! app {
  ($start:expr) => {
    match $start.await {
      Some(app) => app,
      None => return,
    }
  };
}

#[tokio::test]
async fn reports_are_refused_while_off() {
  let app = app!(TestApp::start());
  let response = post_envelope(&app, ENVELOPE).await;
  assert_eq!(response.status(), StatusCode::NOT_FOUND);
  let body: Value = response.json().await.unwrap();
  assert_eq!(body["error_code"], "crash_reports_off");
  app.stop().await;
}

#[tokio::test]
async fn envelopes_reach_the_backend_and_its_answer_comes_back() {
  let app = app!(TestApp::start());
  let backend = FakeBackend::start(AxumStatus::OK).await;
  enable(&app, &backend.dsn()).await;

  let response = post_envelope(&app, ENVELOPE).await;
  assert_eq!(response.status(), StatusCode::ACCEPTED);
  let received = backend.received();
  assert_eq!(received.len(), 1);
  let (path, query, content_type, body) = &received[0];
  assert_eq!(path, "/api/5/envelope/");
  assert!(query.contains("sentry_key=publickey"), "{query}");
  assert_eq!(content_type, "application/x-sentry-envelope");
  assert_eq!(body, ENVELOPE);

  // Not an envelope, or too large: refused before anything is forwarded.
  assert_eq!(
    post_envelope(&app, "<html>").await.status(),
    StatusCode::BAD_REQUEST
  );
  let huge = format!("{{}}\n{}", " ".repeat(300 * 1024));
  assert_eq!(
    post_envelope(&app, &huge).await.status(),
    StatusCode::PAYLOAD_TOO_LARGE
  );
  assert_eq!(backend.received().len(), 1);

  backend.answer_with(AxumStatus::TOO_MANY_REQUESTS);
  let limited = post_envelope(&app, ENVELOPE).await;
  assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
  assert_eq!(limited.headers()["retry-after"], "30");

  backend.answer_with(AxumStatus::BAD_GATEWAY);
  let failed = post_envelope(&app, ENVELOPE).await;
  assert_eq!(failed.status(), StatusCode::SERVICE_UNAVAILABLE);
  assert!(failed.headers().contains_key("retry-after"));

  backend.answer_with(AxumStatus::FORBIDDEN);
  assert_eq!(
    post_envelope(&app, ENVELOPE).await.status(),
    StatusCode::BAD_REQUEST
  );
  app.stop().await;
}

#[tokio::test]
async fn a_dead_backend_is_a_retryable_failure() {
  let app = app!(TestApp::start());
  // Nothing listens on this port.
  let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
  let port = listener.local_addr().unwrap().port();
  drop(listener);
  enable(&app, &format!("http://publickey@127.0.0.1:{port}/5")).await;
  let response = post_envelope(&app, ENVELOPE).await;
  assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
  assert!(response.headers().contains_key("retry-after"));
  app.stop().await;
}

async fn post_envelope(app: &TestApp, body: &str) -> reqwest::Response {
  app
    .http
    .post(format!("{}/api/v1/crash-reports", app.base_url))
    .header("Content-Type", "application/x-sentry-envelope")
    .body(body.to_owned())
    .send()
    .await
    .unwrap()
}

async fn enable(app: &TestApp, dsn: &str) {
  let saved = app
    .put(
      &admin(),
      "/admin/settings",
      json!({"section": "crash_reports", "values": {
        "is_enabled": true, "dsn": dsn, "sample_percent": 100, "environment": "test",
      }}),
    )
    .await;
  assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
}

type Received = Vec<(String, String, String, String)>;

/// A crash reports backend on this machine that records what it gets.
struct FakeBackend {
  port: u16,
  received: Arc<Mutex<Received>>,
  status: Arc<Mutex<AxumStatus>>,
}

impl FakeBackend {
  async fn start(status: AxumStatus) -> Self {
    let received = Arc::new(Mutex::new(Received::new()));
    let status = Arc::new(Mutex::new(status));
    let (sink, answer) = (received.clone(), status.clone());
    let router = Router::new().route(
      "/api/{project}/envelope/",
      post(move |uri: Uri, headers: HeaderMap, body: Bytes| {
        let (sink, answer) = (sink.clone(), answer.clone());
        async move {
          sink.lock().unwrap().push((
            uri.path().to_owned(),
            uri.query().unwrap_or_default().to_owned(),
            headers
              .get("content-type")
              .and_then(|value| value.to_str().ok())
              .unwrap_or_default()
              .to_owned(),
            String::from_utf8_lossy(&body).into_owned(),
          ));
          let status = *answer.lock().unwrap();
          (status, [("retry-after", "30")])
        }
      }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, router).await });
    Self {
      port,
      received,
      status,
    }
  }

  fn dsn(&self) -> String {
    format!("http://publickey@127.0.0.1:{}/5", self.port)
  }

  fn received(&self) -> Received {
    self.received.lock().unwrap().clone()
  }

  fn answer_with(&self, status: AxumStatus) {
    *self.status.lock().unwrap() = status;
  }
}
