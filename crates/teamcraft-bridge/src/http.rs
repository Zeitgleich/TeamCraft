//! The local endpoints tools talk to:
//!
//! - `POST /hooks/{token}`: Slack/Mattermost-compatible incoming webhook (JSON, or the original form
//!   with `payload=`), one message per request; as `multipart/form-data` with files, like Discord's
//!   and Slack's (`file_hooks.rs`).
//! - `POST /ingest/{token}`: log lines, plain text or NDJSON, batched by the worker.
//! - `GET /healthz`: for the container health check.
//!
//! The token selects the source, and so the conversation and the limits (`sources.rs`). Unknown
//! tokens, and tokens of a source of the other kind, get 404, like Slack's webhook URLs, so
//! scanning for valid ones learns nothing. Other refusals answer `{"error_code", "message"}` with a
//! stable `error_code` ([`HookError`]).

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, FromRequest, Path, Request, State};
use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE, RETRY_AFTER};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use tokio::sync::mpsc;
use tracing::warn;

use crate::batch::LogLine;
use crate::file_hooks::{FileSettings, FormBody, receive_form};
use crate::file_jobs::{FilesFailure, FilesMessage};
use crate::form::FormError;
use crate::render::{PayloadError, parse_log_lines, parse_webhook};
use crate::sources::{Endpoint, Source, SourceRegistry};

/// What the endpoints hand to the worker.
#[derive(Debug)]
pub enum Submission {
  /// One webhook message, sent as it is.
  Message {
    source: Arc<Source>,
    text: String,
    is_alert: bool,
  },
  /// Log lines to batch.
  Lines {
    source: Arc<Source>,
    lines: Vec<LogLine>,
  },
  /// Lines refused by the rate limit, to be reported in the channel.
  Dropped { source: Arc<Source>, count: usize },
  /// A webhook message with files, which go up before it is sent.
  Files(FilesMessage),
}

#[derive(Clone)]
pub struct HttpState {
  sources: SourceRegistry,
  sender: mpsc::Sender<Submission>,
  files: Arc<FileSettings>,
}

/// Why a request was refused: a status and a stable `error_code` (api-conventions.md).
#[derive(Debug, thiserror::Error)]
pub enum HookError {
  #[error("rate limit exceeded")]
  RateLimited { retry_after: Duration },
  #[error("the bridge takes no more file bytes this minute")]
  FileRateLimited { retry_after: Duration },
  #[error("the request is larger than the bridge takes in file bytes a minute")]
  RequestTooLarge,
  #[error("the body is larger than listen.max_body_bytes")]
  BodyTooLarge,
  #[error("the body could not be read")]
  UnreadableBody,
  #[error(transparent)]
  Payload(#[from] PayloadError),
  #[error(transparent)]
  Form(#[from] FormError),
  #[error("the server refused a file as too large")]
  ServerFileTooLarge,
  #[error("the files could not be stored for their upload")]
  SpoolFailed,
  #[error("the message could not be stored")]
  NotQueued,
  #[error("uploading a file to the server failed")]
  UploadFailed,
  #[error("the server can't be reached; try again later")]
  ServerUnreachable,
  #[error("the bridge is behind; try again later")]
  Busy,
  #[error("the bridge is shutting down")]
  ShuttingDown,
}

/// A webhook's body: read whole up to `listen.max_body_bytes` (JSON, the form-encoded `payload=`),
/// or left to stream for `multipart/form-data`, which carries files far larger than that.
enum WebhookBody {
  Whole { content_type: String, bytes: Bytes },
  Form(FormBody),
}

/// A body read whole, up to `listen.max_body_bytes`; a larger one is refused with a code.
struct WholeBody(Bytes);

impl HttpState {
  pub fn new(sources: SourceRegistry, sender: mpsc::Sender<Submission>, files: FileSettings) -> Self {
    Self {
      sources,
      sender,
      files: Arc::new(files),
    }
  }

  pub fn sources(&self) -> &SourceRegistry {
    &self.sources
  }

  pub fn files(&self) -> &FileSettings {
    &self.files
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
  body: WebhookBody,
) -> Response {
  let Some(source) = state.sources.find(&token, Endpoint::Webhook) else {
    return StatusCode::NOT_FOUND.into_response();
  };
  if let Some(refusal) = refuse_if_limited(&state, &source, 1) {
    return refusal;
  }
  let accepted = match body {
    WebhookBody::Whole { content_type, bytes } => {
      match parse_webhook(&content_type, &bytes, source.danger_is_alert) {
        Ok(message) => submit(
          &state,
          Submission::Message {
            source,
            text: message.text,
            is_alert: message.is_alert,
          },
        ),
        Err(error) => Err(HookError::Payload(error)),
      }
    },
    WebhookBody::Form(form) => receive_form(&state, source, form).await,
  };
  match accepted {
    Ok(()) => (StatusCode::OK, "ok").into_response(),
    Err(error) => error.into_response(),
  }
}

async fn ingest_handler(
  State(state): State<HttpState>,
  Path(token): Path<String>,
  headers: HeaderMap,
  WholeBody(body): WholeBody,
) -> Response {
  let Some(source) = state.sources.find(&token, Endpoint::Ingest) else {
    return StatusCode::NOT_FOUND.into_response();
  };
  let mut lines = parse_log_lines(&content_type(&headers), &body, &source.rules);
  if lines.is_empty() {
    return (StatusCode::OK, "accepted 0 lines").into_response();
  }
  let total = lines.len();
  let wanted = u32::try_from(total).unwrap_or(u32::MAX);
  let allowed = state.sources.take_up_to(&source, wanted, Instant::now()) as usize;
  if allowed == 0 {
    // Everything was refused; tell the channel once per request, and the sender to slow down.
    let _ = state.sender.try_send(Submission::Dropped {
      source: source.clone(),
      count: total,
    });
    return too_many_requests(&state, &source);
  }
  lines.truncate(allowed);
  if allowed < total {
    let _ = state.sender.try_send(Submission::Dropped {
      source: source.clone(),
      count: total - allowed,
    });
  }
  if let Err(error) = submit(&state, Submission::Lines { source, lines }) {
    return error.into_response();
  }
  let answer = if allowed < total {
    format!(
      "accepted {allowed} lines, dropped {} (rate limit)",
      total - allowed
    )
  } else {
    "ok".to_owned()
  };
  (StatusCode::OK, answer).into_response()
}

fn refuse_if_limited(state: &HttpState, source: &Source, count: u32) -> Option<Response> {
  let allowed = state.sources.try_take(source, count, Instant::now());
  (!allowed).then(|| too_many_requests(state, source))
}

fn too_many_requests(state: &HttpState, source: &Source) -> Response {
  let retry_after = state.sources.retry_after(source, Instant::now());
  HookError::RateLimited { retry_after }.into_response()
}

/// Hands a submission to the worker without waiting.
pub(crate) fn submit(state: &HttpState, submission: Submission) -> Result<(), HookError> {
  match state.sender.try_send(submission) {
    Ok(()) => Ok(()),
    Err(mpsc::error::TrySendError::Full(_)) => {
      warn!("the send queue is full; refusing input");
      Err(HookError::Busy)
    },
    Err(mpsc::error::TrySendError::Closed(_)) => Err(HookError::ShuttingDown),
  }
}

fn content_type(headers: &HeaderMap) -> String {
  raw_content_type(headers).to_ascii_lowercase()
}

/// As sent: a multipart boundary is case-sensitive.
fn raw_content_type(headers: &HeaderMap) -> &str {
  headers
    .get(CONTENT_TYPE)
    .and_then(|value| value.to_str().ok())
    .unwrap_or("")
}

impl HookError {
  pub fn status(&self) -> StatusCode {
    match self {
      Self::RateLimited { .. } | Self::FileRateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
      Self::RequestTooLarge | Self::BodyTooLarge | Self::ServerFileTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
      Self::UnreadableBody | Self::Payload(_) => StatusCode::BAD_REQUEST,
      Self::Form(error) => match error {
        FormError::Multipart(_) | FormError::NotText(_) => StatusCode::BAD_REQUEST,
        FormError::TooManyFiles { .. }
        | FormError::FileTooLarge { .. }
        | FormError::FieldsTooLarge { .. }
        | FormError::TooManyFields => StatusCode::PAYLOAD_TOO_LARGE,
      },
      Self::SpoolFailed | Self::NotQueued => StatusCode::INTERNAL_SERVER_ERROR,
      Self::UploadFailed => StatusCode::BAD_GATEWAY,
      Self::ServerUnreachable | Self::Busy | Self::ShuttingDown => StatusCode::SERVICE_UNAVAILABLE,
    }
  }

  /// Stable: part of the bridge's interface (docs/bridge.md, "Answers").
  pub fn code(&self) -> &'static str {
    match self {
      Self::RateLimited { .. } => "rate_limited",
      Self::FileRateLimited { .. } => "file_rate_limited",
      Self::RequestTooLarge => "request_too_large",
      Self::BodyTooLarge => "payload_too_large",
      Self::UnreadableBody => "invalid_body",
      Self::Payload(_) => "invalid_payload",
      Self::Form(error) => match error {
        FormError::Multipart(_) => "invalid_multipart",
        FormError::NotText(_) => "invalid_payload",
        FormError::TooManyFiles { .. } => "too_many_files",
        FormError::FileTooLarge { .. } => "file_too_large",
        FormError::FieldsTooLarge { .. } => "payload_too_large",
        FormError::TooManyFields => "too_many_fields",
      },
      Self::ServerFileTooLarge => "file_too_large",
      Self::SpoolFailed => "spool_failed",
      Self::NotQueued => "internal_error",
      Self::UploadFailed => "upload_failed",
      Self::ServerUnreachable => "server_unreachable",
      Self::Busy => "bridge_busy",
      Self::ShuttingDown => "shutting_down",
    }
  }

  fn maybe_retry_after(&self) -> Option<Duration> {
    match self {
      Self::RateLimited { retry_after } | Self::FileRateLimited { retry_after } => Some(*retry_after),
      _ => None,
    }
  }
}

impl IntoResponse for HookError {
  fn into_response(self) -> Response {
    let body = Json(json!({"error_code": self.code(), "message": self.to_string()}));
    match self.maybe_retry_after() {
      Some(retry_after) => {
        let seconds = retry_after.as_secs_f64().ceil().max(1.0).to_string();
        (self.status(), [(RETRY_AFTER, seconds)], body).into_response()
      },
      None => (self.status(), body).into_response(),
    }
  }
}

impl From<FilesFailure> for HookError {
  fn from(failure: FilesFailure) -> Self {
    match failure {
      FilesFailure::TooLarge => Self::ServerFileTooLarge,
      FilesFailure::Unreachable => Self::ServerUnreachable,
      FilesFailure::Failed => Self::UploadFailed,
      FilesFailure::LocalFile => Self::SpoolFailed,
      FilesFailure::Busy => Self::Busy,
      FilesFailure::NotQueued => Self::NotQueued,
    }
  }
}

impl<S: Send + Sync> FromRequest<S> for WebhookBody {
  type Rejection = HookError;

  async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
    let content_type = raw_content_type(request.headers()).to_owned();
    if content_type
      .to_ascii_lowercase()
      .starts_with("multipart/form-data")
    {
      let maybe_length = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok());
      return Ok(Self::Form(FormBody {
        content_type,
        maybe_length,
        body: request.into_body(),
      }));
    }
    let WholeBody(bytes) = WholeBody::from_request(request, state).await?;
    Ok(Self::Whole {
      content_type: content_type.to_ascii_lowercase(),
      bytes,
    })
  }
}

impl<S: Send + Sync> FromRequest<S> for WholeBody {
  type Rejection = HookError;

  async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
    Bytes::from_request(request, state)
      .await
      .map(Self)
      .map_err(|rejection| match rejection.status() {
        StatusCode::PAYLOAD_TOO_LARGE => HookError::BodyTooLarge,
        _ => HookError::UnreadableBody,
      })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const TOKEN_A: &str = "alpha-token-0123456789";
  const TOKEN_B: &str = "bravo-token-0123456789";

  use std::sync::atomic::{AtomicU32, Ordering};

  use crate::config::BridgeConfig;
  use crate::file_hooks::FileSettings;

  /// Spool directories of the tests, one each.
  static NEXT_SPOOL: AtomicU32 = AtomicU32::new(0);

  fn config(rate: u32) -> BridgeConfig {
    config_with_files(rate, "")
  }

  /// Source b takes one file per message; `files` adds to the `[files]` section.
  fn config_with_files(rate: u32, files: &str) -> BridgeConfig {
    let spool = std::env::temp_dir().join(format!(
      "teamcraft-bridge-http-{}-{}",
      std::process::id(),
      NEXT_SPOOL.fetch_add(1, Ordering::Relaxed)
    ));
    BridgeConfig::from_toml(&format!(
      r#"
      [server]
      url = "http://localhost"
      [store]
      path = "x.db"
      [listen]
      max_body_bytes = 4096
      [files]
      max_file_bytes = 1024
      spool_dir = "{spool}"
      {files}
      [[sources]]
      name = "a"
      token = "{TOKEN_A}"
      conversation_id = "1"
      rate_limit_per_minute = {rate}
      [[sources]]
      name = "b"
      token = "{TOKEN_B}"
      conversation_id = "2"
      max_files_per_message = 1
      "#,
      spool = spool.display()
    ))
    .unwrap()
  }

  async fn start(rate: u32) -> (String, mpsc::Receiver<Submission>) {
    start_with(config(rate)).await
  }

  async fn start_with(config: BridgeConfig) -> (String, mpsc::Receiver<Submission>) {
    let (sender, receiver) = mpsc::channel(64);
    let state = HttpState::new(
      SourceRegistry::new(&config),
      sender,
      FileSettings::from_config(&config),
    );
    let app = router(state, config.listen.max_body_bytes);
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
          assert_eq!(
            (source.name.as_str(), text.as_str(), is_alert),
            ("b", "**ci**\nhi", false)
          );
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
      for endpoint in ["ingest", "hooks"] {
        let big = "x".repeat(5000);
        let response = reqwest::Client::new()
          .post(format!("{base}/{endpoint}/{TOKEN_A}"))
          .body(big)
          .send()
          .await
          .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let answer: serde_json::Value = response.json().await.unwrap();
        assert_eq!(answer["error_code"], "payload_too_large", "{endpoint}");
      }
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
          assert_eq!(source.name, "a");
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
          Submission::Message { .. } | Submission::Files(_) => {},
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

  mod files {
    use super::*;
    use crate::file_jobs::FilesFailure;

    const FORM_TYPE: &str = "multipart/form-data; boundary=XyZ";

    #[tokio::test]
    async fn a_message_with_files_is_answered_once_the_worker_has_uploaded_them() {
      let (base, mut receiver) = start(60).await;
      let body = form(&[
        (
          "payload_json",
          None,
          br#"{"text":"Build 7","username":"ci"}"#.as_slice(),
        ),
        ("files[0]", Some("report.txt"), b"all green"),
        ("files[1]", Some("graph.png"), b"png bytes"),
      ]);
      let request = tokio::spawn(post_form(format!("{base}/hooks/{TOKEN_A}"), FORM_TYPE, body));
      let Some(Submission::Files(message)) = receiver.recv().await else {
        panic!("no files")
      };
      assert_eq!(
        (message.source.name.as_str(), message.text.as_str()),
        ("a", "**ci**\nBuild 7")
      );
      let names: Vec<_> = message.files.iter().map(|file| file.file_name.as_str()).collect();
      assert_eq!(names, ["report.txt", "graph.png"]);
      assert_eq!(message.files[1].media_type, "image/png");
      assert_eq!(std::fs::read(message.files[0].path()).unwrap(), b"all green");
      let paths: Vec<_> = message.files.iter().map(|file| file.path().to_owned()).collect();
      // As the worker does: the files are deleted once uploaded, then the producer is answered.
      let FilesMessage { reply, files, .. } = message;
      drop(files);
      reply.send(Ok(())).unwrap();
      let (status, body) = request.await.unwrap();
      assert_eq!((status, body.as_str()), (StatusCode::OK, "ok"));
      assert!(paths.iter().all(|path| !path.exists()), "the spool is emptied");
    }

    #[tokio::test]
    async fn a_failed_upload_answers_with_a_stable_code_and_sends_nothing() {
      let (base, mut receiver) = start(60).await;
      for (failure, status, code) in [
        (FilesFailure::Failed, StatusCode::BAD_GATEWAY, "upload_failed"),
        (
          FilesFailure::TooLarge,
          StatusCode::PAYLOAD_TOO_LARGE,
          "file_too_large",
        ),
        (
          FilesFailure::Unreachable,
          StatusCode::SERVICE_UNAVAILABLE,
          "server_unreachable",
        ),
      ] {
        let body = form(&[("text", None, b"x".as_slice()), ("file", Some("a.bin"), b"1")]);
        let request = tokio::spawn(post_form(format!("{base}/hooks/{TOKEN_A}"), FORM_TYPE, body));
        let Some(Submission::Files(message)) = receiver.recv().await else {
          panic!("no files")
        };
        message.reply.send(Err(failure)).unwrap();
        let (answer_status, answer) = request.await.unwrap();
        assert_eq!(answer_status, status);
        assert_eq!(error_code(&answer), code);
      }
    }

    #[tokio::test]
    async fn a_form_without_files_is_an_ordinary_webhook() {
      let (base, mut receiver) = start(60).await;
      let body = form(&[
        ("text", None, b"Deploy done".as_slice()),
        ("username", None, b"cd"),
      ]);
      let (status, _) = post_form(format!("{base}/hooks/{TOKEN_A}"), FORM_TYPE, body).await;
      assert_eq!(status, StatusCode::OK);
      match receiver.recv().await.unwrap() {
        Submission::Message { text, .. } => assert_eq!(text, "**cd**\nDeploy done"),
        other => panic!("{other:?}"),
      }
    }

    #[tokio::test]
    async fn limits_are_refused_with_413_and_stable_codes() {
      let (base, mut receiver) = start(60).await;
      for (token, body, code) in [
        (
          TOKEN_B,
          form(&[("file", Some("1"), b"1".as_slice()), ("file", Some("2"), b"2")]),
          "too_many_files",
        ),
        (
          TOKEN_A,
          form(&[("file", Some("big.bin"), [0u8; 1025].as_slice())]),
          "file_too_large",
        ),
        (
          TOKEN_A,
          form(&[("text", None, [b'x'; 5000].as_slice())]),
          "payload_too_large",
        ),
      ] {
        let (status, answer) = post_form(format!("{base}/hooks/{token}"), FORM_TYPE, body).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{code}");
        assert_eq!(error_code(&answer), code);
      }
      assert!(receiver.try_recv().is_err(), "nothing reached the worker");
    }

    #[tokio::test]
    async fn malformed_forms_are_refused_with_400() {
      let (base, _receiver) = start(60).await;
      let url = format!("{base}/hooks/{TOKEN_A}");
      let truncated = form(&[("text", None, b"x".as_slice())])[..20].to_vec();
      for (content_type, body, code) in [
        (FORM_TYPE, truncated, "invalid_multipart"),
        ("multipart/form-data", b"--XyZ--".to_vec(), "invalid_multipart"),
        (
          FORM_TYPE,
          form(&[("other", None, b"x".as_slice())]),
          "invalid_payload",
        ),
        (
          FORM_TYPE,
          form(&[("payload_json", None, b"{".as_slice())]),
          "invalid_payload",
        ),
      ] {
        let (status, answer) = post_form(url.clone(), content_type, body).await;
        assert_eq!(
          (status, error_code(&answer)),
          (StatusCode::BAD_REQUEST, code.to_owned())
        );
      }
    }

    #[tokio::test]
    async fn file_bytes_over_the_bridge_budget_get_429_with_retry_after() {
      let (base, mut receiver) = start_with(config_with_files(60, "total_bytes_per_minute = 1000")).await;
      let url = format!("{base}/hooks/{TOKEN_A}");
      let body = form(&[("file", Some("a.bin"), [1u8; 600].as_slice())]);
      let first = tokio::spawn(post_form(url.clone(), FORM_TYPE, body.clone()));
      let Some(Submission::Files(message)) = receiver.recv().await else {
        panic!("no files")
      };
      message.reply.send(Ok(())).unwrap();
      assert_eq!(first.await.unwrap().0, StatusCode::OK, "the first one fits");
      let response = reqwest::Client::new()
        .post(&url)
        .header("content-type", FORM_TYPE)
        .body(body)
        .send()
        .await
        .unwrap();
      assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
      assert!(response.headers().get("retry-after").is_some());
      assert_eq!(error_code(&response.text().await.unwrap()), "file_rate_limited");
      let huge = form(&[("file", Some("a.bin"), [1u8; 1001].as_slice())]);
      let (status, answer) = post_form(url, FORM_TYPE, huge).await;
      assert_eq!(
        (status, error_code(&answer)),
        (StatusCode::PAYLOAD_TOO_LARGE, "request_too_large".to_owned())
      );
    }

    #[tokio::test]
    async fn other_refusals_carry_a_code_too() {
      let (base, _receiver) = start(1).await;
      let url = format!("{base}/hooks/{TOKEN_A}");
      assert_eq!(
        post(url.clone(), "application/json", r#"{"text":"1"}"#).await.0,
        StatusCode::OK
      );
      let (status, answer, _) = post(url.clone(), "application/json", r#"{"text":"2"}"#).await;
      assert_eq!(
        (status, error_code(&answer)),
        (StatusCode::TOO_MANY_REQUESTS, "rate_limited".to_owned())
      );
      let (base, _receiver) = start(60).await;
      let (status, answer, _) = post(format!("{base}/hooks/{TOKEN_A}"), "application/json", "{}").await;
      assert_eq!(
        (status, error_code(&answer)),
        (StatusCode::BAD_REQUEST, "invalid_payload".to_owned())
      );
    }

    async fn post_form(url: String, content_type: &'static str, body: Vec<u8>) -> (StatusCode, String) {
      let response = reqwest::Client::new()
        .post(url)
        .header("content-type", content_type)
        .body(body)
        .send()
        .await
        .unwrap();
      (response.status(), response.text().await.unwrap())
    }

    fn error_code(answer: &str) -> String {
      let value: serde_json::Value = serde_json::from_str(answer).unwrap();
      value["error_code"].as_str().unwrap().to_owned()
    }

    /// A form with the boundary `XyZ`: (name, file name, data).
    fn form(parts: &[(&str, Option<&str>, &[u8])]) -> Vec<u8> {
      let parts: Vec<_> = parts
        .iter()
        .map(|(name, maybe_file_name, data)| (*name, *maybe_file_name, None, *data))
        .collect();
      crate::multipart::tests::form(&parts)
    }
  }

  #[tokio::test]
  async fn the_health_endpoint_answers() {
    let (base, _receiver) = start(60).await;
    let response = reqwest::get(format!("{base}/healthz")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
  }
}
