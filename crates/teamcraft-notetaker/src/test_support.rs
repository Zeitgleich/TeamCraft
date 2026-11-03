//! A local stand-in for OpenRouter for the tests: it answers from a script and records the
//! requests. No test reaches the real API, and none needs a key.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use secrecy::SecretString;
use serde_json::Value;

use crate::openrouter::{OpenRouterClient, RetrySettings};

pub(crate) const KEY: &str = "sk-or-v1-test-key";

pub(crate) struct FakeOpenRouter {
  addr: SocketAddr,
  state: Arc<FakeState>,
}

struct FakeState {
  replies: Mutex<Vec<Reply>>,
  requests: Mutex<Vec<Recorded>>,
}

/// One scripted answer.
#[derive(Clone)]
pub(crate) struct Reply {
  status: u16,
  body: String,
  maybe_retry_after: Option<u64>,
}

/// One request as the fake received it.
pub(crate) struct Recorded {
  pub path: String,
  pub authorization: Option<String>,
  pub title: Option<String>,
  pub body: Value,
}

impl FakeOpenRouter {
  /// Serves `replies` in order on a loopback port; requests beyond them get `500`.
  pub(crate) async fn start(replies: Vec<Reply>) -> Self {
    let state = Arc::new(FakeState {
      replies: Mutex::new(replies.into_iter().rev().collect()),
      requests: Mutex::new(Vec::new()),
    });
    let app = axum::Router::new()
      .route("/api/v1/audio/transcriptions", post(answer))
      .route("/api/v1/chat/completions", post(answer))
      .with_state(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("[::1]:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Self { addr, state }
  }

  /// A client of the fake with fast retries.
  pub(crate) fn client(&self, max_retries: u32) -> OpenRouterClient {
    OpenRouterClient::new(
      &format!("http://{}/api/v1", self.addr),
      SecretString::from(KEY.to_owned()),
      Duration::from_secs(5),
      RetrySettings {
        max_retries,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(5),
      },
      Some("TeamCraft notetaker".into()),
    )
    .unwrap()
  }

  /// The requests so far (taken out).
  pub(crate) fn requests(&self) -> Vec<Recorded> {
    std::mem::take(&mut *self.state.requests.lock().unwrap())
  }
}

impl Reply {
  pub(crate) fn json(status: u16, body: &str) -> Self {
    Self {
      status,
      body: body.to_owned(),
      maybe_retry_after: None,
    }
  }

  /// A chat completion answering `text`.
  pub(crate) fn chat(text: &str) -> Self {
    let body = serde_json::json!({
      "choices": [{"message": {"role": "assistant", "content": text}}],
      "usage": {"prompt_tokens": 100, "completion_tokens": 10, "cost": 0.0001},
    });
    Self::json(200, &body.to_string())
  }

  pub(crate) fn retry_after(mut self, seconds: u64) -> Self {
    self.maybe_retry_after = Some(seconds);
    self
  }
}

async fn answer(
  State(state): State<Arc<FakeState>>,
  uri: Uri,
  headers: HeaderMap,
  Json(body): Json<Value>,
) -> Response {
  let header = |name: &str| {
    headers
      .get(name)
      .and_then(|value| value.to_str().ok())
      .map(str::to_owned)
  };
  state.requests.lock().unwrap().push(Recorded {
    path: uri.path().to_owned(),
    authorization: header("authorization"),
    title: header("x-title"),
    body,
  });
  let reply = state
    .replies
    .lock()
    .unwrap()
    .pop()
    .unwrap_or(Reply::json(500, "no reply left"));
  let status = StatusCode::from_u16(reply.status).unwrap();
  let mut response = (status, reply.body).into_response();
  if let Some(seconds) = reply.maybe_retry_after {
    response
      .headers_mut()
      .insert("retry-after", seconds.to_string().parse().unwrap());
  }
  response
}
