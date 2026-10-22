//! The local endpoints tools talk to:
//!
//! - `POST /hooks/{token}`: Slack/Mattermost-compatible incoming webhook (JSON, or the original form
//!   with `payload=`), one message per request.
//! - `POST /ingest/{token}`: log lines, plain text or NDJSON, batched by the worker.
//! - `GET /healthz`: for the container health check.
//!
//! The token selects the source, and so the conversation and the limits. Unknown tokens get 404,
//! like Slack's webhook URLs, so scanning for valid ones learns nothing.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::header::{CONTENT_TYPE, RETRY_AFTER};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use secrecy::{ExposeSecret, SecretString};
use subtle::ConstantTimeEq;
use teamcraft_proto::ConversationId;
use tokio::sync::mpsc;
use tracing::warn;

use crate::batch::LogLine;
use crate::config::BridgeConfig;
use crate::limiter::RateLimiter;
use crate::render::{LogRules, parse_log_lines, parse_webhook};

/// What the endpoints hand to the worker.
#[derive(Debug)]
pub enum Submission {
  /// One webhook message, sent as it is.
  Message {
    source: usize,
    text: String,
    is_alert: bool,
  },
  /// Log lines to batch.
  Lines { source: usize, lines: Vec<LogLine> },
  /// Lines refused by the rate limit, to be reported in the channel.
  Dropped { source: usize, count: usize },
}

/// A producer as the endpoints see it.
pub struct Source {
  pub name: String,
  token: SecretString,
  pub conversation: ConversationId,
  rules: LogRules,
  danger_is_alert: bool,
}

#[derive(Clone)]
pub struct HttpState {
  sources: Arc<Vec<Source>>,
  limiters: Arc<Mutex<Vec<RateLimiter>>>,
  sender: mpsc::Sender<Submission>,
}

impl Source {
  pub fn from_config(config: &crate::config::SourceConfig) -> Self {
    Self {
      name: config.name.clone(),
      token: config.token.clone(),
      conversation: config.conversation_id,
      rules: config.log_rules(),
      danger_is_alert: config.danger_is_alert,
    }
  }
}

impl HttpState {
  pub fn new(config: &BridgeConfig, sender: mpsc::Sender<Submission>) -> Self {
    let now = Instant::now();
    Self {
      sources: Arc::new(config.sources.iter().map(Source::from_config).collect()),
      limiters: Arc::new(Mutex::new(
        config
          .sources
          .iter()
          .map(|source| RateLimiter::new(source.rate_limit_per_minute, now))
          .collect(),
      )),
      sender,
    }
  }

  /// The source whose token matches, compared in constant time.
  fn find(&self, token: &str) -> Option<usize> {
    let mut found = None;
    for (index, source) in self.sources.iter().enumerate() {
      if bool::from(source.token.expose_secret().as_bytes().ct_eq(token.as_bytes())) {
        found = Some(index);
      }
    }
    found
  }

  fn limiters(&self) -> std::sync::MutexGuard<'_, Vec<RateLimiter>> {
    // The limiter state is plain numbers, so a poisoned lock is still consistent.
    self
      .limiters
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
  }
}

pub fn router(state: HttpState, max_body_bytes: usize) -> Router {
  Router::new()
    .route("/hooks/{token}", post(webhook_handler))
    .route("/ingest/{token}", post(ingest_handler))
    .route("/healthz", get(|| async { "ok" }))
    .layer(DefaultBodyLimit::max(max_body_bytes))
    .with_state(state)
}

async fn webhook_handler(
  State(state): State<HttpState>,
  Path(token): Path<String>,
  headers: HeaderMap,
  body: Bytes,
) -> Response {
  let Some(source) = state.find(&token) else {
    return StatusCode::NOT_FOUND.into_response();
  };
  if let Some(refusal) = refuse_if_limited(&state, source, 1) {
    return refusal;
  }
  let content_type = content_type(&headers);
  let message = match parse_webhook(&content_type, &body, state.sources[source].danger_is_alert) {
    Ok(message) => message,
    Err(error) => return (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
  };
  submit(
    &state,
    Submission::Message {
      source,
      text: message.text,
      is_alert: message.is_alert,
    },
  )
}

async fn ingest_handler(
  State(state): State<HttpState>,
  Path(token): Path<String>,
  headers: HeaderMap,
  body: Bytes,
) -> Response {
  let Some(source) = state.find(&token) else {
    return StatusCode::NOT_FOUND.into_response();
  };
  let mut lines = parse_log_lines(&content_type(&headers), &body, &state.sources[source].rules);
  if lines.is_empty() {
    return (StatusCode::OK, "accepted 0 lines").into_response();
  }
  let total = lines.len();
  let allowed = {
    let wanted = u32::try_from(total).unwrap_or(u32::MAX);
    let mut limiters = state.limiters();
    limiters[source].take_up_to(wanted, Instant::now()) as usize
  };
  if allowed == 0 {
    // Everything was refused; tell the channel once per request, and the sender to slow down.
    let _ = state
      .sender
      .try_send(Submission::Dropped { source, count: total });
    return too_many_requests(&state, source);
  }
  lines.truncate(allowed);
  if allowed < total {
    let _ = state.sender.try_send(Submission::Dropped {
      source,
      count: total - allowed,
    });
  }
  let response = submit(&state, Submission::Lines { source, lines });
  if response.status() == StatusCode::OK && allowed < total {
    return (
      StatusCode::OK,
      format!(
        "accepted {allowed} lines, dropped {} (rate limit)",
        total - allowed
      ),
    )
      .into_response();
  }
  response
}

fn refuse_if_limited(state: &HttpState, source: usize, count: u32) -> Option<Response> {
  let allowed = state.limiters()[source].try_take(count, Instant::now());
  (!allowed).then(|| too_many_requests(state, source))
}

fn too_many_requests(state: &HttpState, source: usize) -> Response {
  let retry_after = state.limiters()[source].retry_after().as_secs().max(1);
  (
    StatusCode::TOO_MANY_REQUESTS,
    [(RETRY_AFTER, retry_after.to_string())],
    "rate limit exceeded",
  )
    .into_response()
}

fn submit(state: &HttpState, submission: Submission) -> Response {
  match state.sender.try_send(submission) {
    Ok(()) => (StatusCode::OK, "ok").into_response(),
    Err(mpsc::error::TrySendError::Full(_)) => {
      warn!("the send queue is full; refusing input");
      (
        StatusCode::SERVICE_UNAVAILABLE,
        "the bridge is behind; try again later",
      )
        .into_response()
    },
    Err(mpsc::error::TrySendError::Closed(_)) => {
      (StatusCode::SERVICE_UNAVAILABLE, "the bridge is shutting down").into_response()
    },
  }
}

fn content_type(headers: &HeaderMap) -> String {
  headers
    .get(CONTENT_TYPE)
    .and_then(|value| value.to_str().ok())
    .unwrap_or("")
    .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
  use super::*;

  const TOKEN_A: &str = "alpha-token-0123456789";
  const TOKEN_B: &str = "bravo-token-0123456789";

  fn config(rate: u32) -> BridgeConfig {
    BridgeConfig::from_toml(&format!(
      r#"
      [server]
      url = "http://localhost"
      [store]
      path = "x.db"
      [[sources]]
      name = "a"
      token = "{TOKEN_A}"
      conversation_id = "1"
      rate_limit_per_minute = {rate}
      [[sources]]
      name = "b"
      token = "{TOKEN_B}"
      conversation_id = "2"
      "#
    ))
    .unwrap()
  }

  async fn start(rate: u32) -> (String, mpsc::Receiver<Submission>) {
    let (sender, receiver) = mpsc::channel(64);
    let app = router(HttpState::new(&config(rate), sender), 4096);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    (base, receiver)
  }

  async fn post(url: String, content_type: &str, body: &'static str) -> (StatusCode, String, Option<String>) {
    let response = reqwest::Client::new()
      .post(url)
      .header("content-type", content_type)
      .body(body)
      .send()
      .await
      .unwrap();
    let retry = response
      .headers()
      .get("retry-after")
      .map(|v| v.to_str().unwrap().to_owned());
    (response.status(), response.text().await.unwrap(), retry)
  }

  mod webhooks {
    use super::*;

    #[tokio::test]
    async fn a_valid_webhook_reaches_the_worker_for_its_source() {
      let (base, mut receiver) = start(60).await;
      let (status, body, _) = post(
        format!("{base}/hooks/{TOKEN_B}"),
        "application/json",
        r#"{"text":"hi","username":"ci"}"#,
      )
      .await;
      assert_eq!((status, body.as_str()), (StatusCode::OK, "ok"));
      match receiver.recv().await.unwrap() {
        Submission::Message {
          source,
          text,
          is_alert,
        } => {
          assert_eq!((source, text.as_str(), is_alert), (1, "**ci**\nhi", false));
        },
        other => panic!("{other:?}"),
      }
    }

    #[tokio::test]
    async fn unknown_tokens_and_bad_bodies_are_refused() {
      let (base, mut receiver) = start(60).await;
      assert_eq!(
        post(
          format!("{base}/hooks/nope"),
          "application/json",
          r#"{"text":"x"}"#
        )
        .await
        .0,
        StatusCode::NOT_FOUND
      );
      assert_eq!(
        post(format!("{base}/ingest/nope"), "text/plain", "x").await.0,
        StatusCode::NOT_FOUND
      );
      assert_eq!(
        post(format!("{base}/hooks/{TOKEN_A}"), "application/json", "{}")
          .await
          .0,
        StatusCode::BAD_REQUEST
      );
      assert_eq!(
        post(format!("{base}/hooks/{TOKEN_A}"), "application/json", "not json")
          .await
          .0,
        StatusCode::BAD_REQUEST
      );
      assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn oversized_bodies_are_refused() {
      let (base, _receiver) = start(60).await;
      let big = "x".repeat(5000);
      let response = reqwest::Client::new()
        .post(format!("{base}/ingest/{TOKEN_A}"))
        .body(big)
        .send()
        .await
        .unwrap();
      assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn webhooks_over_the_rate_limit_get_429_with_retry_after() {
      let (base, _receiver) = start(2).await;
      let url = format!("{base}/hooks/{TOKEN_A}");
      assert_eq!(
        post(url.clone(), "application/json", r#"{"text":"1"}"#).await.0,
        StatusCode::OK
      );
      assert_eq!(
        post(url.clone(), "application/json", r#"{"text":"2"}"#).await.0,
        StatusCode::OK
      );
      let (status, _, retry) = post(url, "application/json", r#"{"text":"3"}"#).await;
      assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
      assert!(retry.unwrap().parse::<u64>().unwrap() >= 1);
    }
  }

  mod logs {
    use super::*;

    #[tokio::test]
    async fn lines_are_split_and_alerts_marked() {
      let (base, mut receiver) = start(60).await;
      let (status, ..) = post(
        format!("{base}/ingest/{TOKEN_A}"),
        "text/plain",
        "INFO fine\nERROR broken\n",
      )
      .await;
      assert_eq!(status, StatusCode::OK);
      match receiver.recv().await.unwrap() {
        Submission::Lines { source, lines } => {
          assert_eq!(source, 0);
          assert_eq!(
            lines.iter().map(|l| l.is_alert).collect::<Vec<_>>(),
            [false, true]
          );
        },
        other => panic!("{other:?}"),
      }
    }

    #[tokio::test]
    async fn lines_over_the_limit_are_dropped_and_reported() {
      let (base, mut receiver) = start(3).await;
      let (status, body, _) = post(
        format!("{base}/ingest/{TOKEN_A}"),
        "text/plain",
        "1\n2\n3\n4\n5\n",
      )
      .await;
      assert_eq!(status, StatusCode::OK);
      assert!(
        body.contains("accepted 3") && body.contains("dropped 2"),
        "{body}"
      );
      let mut dropped = 0;
      let mut accepted = 0;
      while let Ok(submission) = receiver.try_recv() {
        match submission {
          Submission::Dropped { count, .. } => dropped += count,
          Submission::Lines { lines, .. } => accepted += lines.len(),
          Submission::Message { .. } => {},
        }
      }
      assert_eq!((accepted, dropped), (3, 2));
      let (status, _, retry) = post(format!("{base}/ingest/{TOKEN_A}"), "text/plain", "6\n").await;
      assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
      assert!(retry.is_some());
    }

    #[tokio::test]
    async fn empty_bodies_are_accepted_without_effect() {
      let (base, mut receiver) = start(60).await;
      let (status, ..) = post(format!("{base}/ingest/{TOKEN_A}"), "text/plain", "\n\n").await;
      assert_eq!(status, StatusCode::OK);
      assert!(receiver.try_recv().is_err());
    }
  }

  #[tokio::test]
  async fn the_health_endpoint_answers() {
    let (base, _receiver) = start(60).await;
    let response = reqwest::get(format!("{base}/healthz")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
  }
}
