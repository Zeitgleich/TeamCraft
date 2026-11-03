//! What an API request meets while a dependency is in trouble (issue #743). Object storage, the
//! identity provider and the SFU sit behind their own client calls, which go through their breakers;
//! the database has no such call, every handler reaches it through the pool. So the database's
//! breaker sits here, around the request:
//!
//! - **Down:** while it is open, requests that need the database answer 503 with `Retry-After` at
//!   once, instead of waiting for a pooled connection (`acquire_timeout_secs`) each. When its time
//!   is up, one request pings first; a success closes the breaker and the request goes on.
//! - **Noticing:** handlers' errors feed the breaker, so an outage opens it under traffic, without
//!   waiting for anybody to ask `/readyz`.
//! - **Slow:** nothing bounds a query on a connection that stopped answering. A request that makes no
//!   progress for `request_deadline_secs` answers 503 too. Receiving its body counts as progress, so
//!   a large upload on a slow link is not cut off (robustness.md, per-phase timeouts).
//!
//! Queries are not retried here: only the handler knows whether repeating them is safe. Clients retry
//! after `Retry-After`, with their idempotency keys.

use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use teamcraft_db::DbError;
use teamcraft_proto::{DependencyName, DependencyState};
use tokio::time::Instant;
use tracing::warn;

use crate::auth::BoxFuture;
use crate::dependencies::{CircuitOpen, DependencyGuard, Fault, Retries};
use crate::error::{ApiError, DATABASE_RETRY_AFTER_SECS, DatabaseOutage};
use crate::router::logged_path;
use crate::state::AppState;

const API_PREFIX: &str = "/api/v1/";
/// API paths (with what is below them) that answer from memory, so a client can still learn what
/// server this is and draw its brand while the database is down. The gateway checks by itself once
/// the socket has sent its token.
const WITHOUT_DATABASE: [&str; 5] = [
  "/api/v1/brand",
  "/api/v1/client-compatibility",
  "/api/v1/gateway",
  "/api/v1/openapi.json",
  "/api/v1/server-info",
];
/// API paths without a deadline: the gateway stays open by design, and import batches are an
/// admin's long-running tool whose requests can legitimately take minutes.
const WITHOUT_DEADLINE: [&str; 2] = ["/api/v1/admin/import", "/api/v1/gateway"];

/// The database's breaker and the request deadline. Cheap to clone.
#[derive(Clone)]
pub struct OutageGate {
  guard: Arc<DependencyGuard>,
  ping: Ping,
  readiness_timeout: Duration,
  maybe_request_deadline: Option<Duration>,
}

/// One round trip to the database.
pub type Ping = Arc<dyn Fn() -> BoxFuture<'static, Result<(), DbError>> + Send + Sync>;

/// Why a ping of the database didn't succeed.
#[derive(Debug, thiserror::Error)]
pub enum PingError {
  #[error(transparent)]
  Database(#[from] DbError),
  #[error("the database did not answer in time")]
  TimedOut,
  #[error(transparent)]
  Down(#[from] CircuitOpen),
}

/// When the request last made progress: its start, or the last piece of its body.
#[derive(Clone)]
struct Progress(Arc<Mutex<Instant>>);

impl OutageGate {
  pub fn new(
    guard: Arc<DependencyGuard>,
    ping: Ping,
    readiness_timeout: Duration,
    maybe_request_deadline: Option<Duration>,
  ) -> Self {
    Self {
      guard,
      ping,
      readiness_timeout,
      maybe_request_deadline,
    }
  }

  pub fn from_state(state: &AppState) -> Self {
    let database = state.database.clone();
    let options = state.dependencies.options();
    Self::new(
      state.dependencies.guard(DependencyName::Database),
      Arc::new(move || {
        let database = database.clone();
        Box::pin(async move { database.ping().await })
      }),
      options.readiness_timeout,
      options.maybe_request_deadline,
    )
  }

  /// One request through the gate; `run` is the rest of the stack.
  pub async fn handle<F, Fut>(&self, request: Request, run: F) -> Response
  where
    F: FnOnce(Request) -> Fut,
    Fut: Future<Output = Response>,
  {
    let path = request.uri().path().to_owned();
    if !path.starts_with(API_PREFIX) {
      return run(request).await;
    }
    let needs_database = !is_listed(&WITHOUT_DATABASE, &path);
    if needs_database && let Err(open) = self.admit().await {
      return ApiError::from(open).into_response();
    }
    let response = match self.maybe_request_deadline {
      Some(deadline) if !is_listed(&WITHOUT_DEADLINE, &path) => {
        run_with_deadline(deadline, &path, request, run).await
      },
      _ => run(request).await,
    };
    if needs_database {
      self.note_response(&response);
    }
    response
  }

  /// Fails fast while the database is down. When a probe is due this call is the probe: it pings, and
  /// a success closes the breaker. Costs nothing while the breaker is closed.
  pub async fn admit(&self) -> Result<(), CircuitOpen> {
    if self.guard.state() == DependencyState::Ok {
      return Ok(());
    }
    match self.probe().await {
      Ok(()) => Ok(()),
      Err(PingError::Down(open)) => Err(open),
      Err(PingError::Database(_) | PingError::TimedOut) => Err(CircuitOpen {
        dependency: self.guard.name().clone(),
        retry_after: self.guard.retry_after(),
      }),
    }
  }

  /// One ping through the database's breaker, bounded by `readiness_timeout`.
  pub async fn probe(&self) -> Result<(), PingError> {
    self
      .guard
      .call(
        Retries::Once,
        |_| Fault::Outage,
        || async {
          match tokio::time::timeout(self.readiness_timeout, (self.ping)()).await {
            Ok(result) => Ok(result?),
            Err(_) => Err(PingError::TimedOut),
          }
        },
      )
      .await
  }

  /// What came of work that used the database outside the HTTP stack (the gateway's handshake).
  pub fn note_outcome<T>(&self, outcome: &Result<T, ApiError>) {
    match outcome {
      Ok(_) => self.guard.record_answer(),
      Err(ApiError::Database(error)) if error.is_outage() => self.guard.record_failure(),
      Err(_) => {},
    }
  }

  fn note_response(&self, response: &Response) {
    if response.extensions().get::<DatabaseOutage>().is_some() {
      self.guard.record_failure();
    } else if !response.status().is_server_error() {
      self.guard.record_answer();
    }
  }
}

impl Progress {
  fn new() -> Self {
    Self(Arc::new(Mutex::new(Instant::now())))
  }

  fn last(&self) -> Instant {
    *self.0.lock().unwrap_or_else(PoisonError::into_inner)
  }

  fn note(&self) {
    *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Instant::now();
  }

  /// Makes every piece of a request's body count as progress. A request without a body stays as it
  /// is.
  fn watch(&self, request: Request) -> Request {
    if !has_body(&request) {
      return request;
    }
    let (parts, body) = request.into_parts();
    let progress = self.clone();
    let stream = body.into_data_stream().inspect(move |_| progress.note());
    Request::from_parts(parts, Body::from_stream(stream))
  }
}

/// The middleware of [`OutageGate`].
pub async fn outage_gate(State(gate): State<OutageGate>, request: Request, next: Next) -> Response {
  gate.handle(request, |request| next.run(request)).await
}

/// Runs `run` until it answers or made no progress for `deadline`. Dropping the handler's future is
/// what a client hanging up does too: an open transaction rolls back.
async fn run_with_deadline<F, Fut>(deadline: Duration, path: &str, request: Request, run: F) -> Response
where
  F: FnOnce(Request) -> Fut,
  Fut: Future<Output = Response>,
{
  let progress = Progress::new();
  let handler = run(progress.watch(request));
  tokio::pin!(handler);
  loop {
    let expires_at = progress.last() + deadline;
    tokio::select! {
      biased;
      response = &mut handler => return response,
      () = tokio::time::sleep_until(expires_at) => {
        if progress.last() + deadline <= Instant::now() {
          warn!(
            path = %logged_path(path),
            deadline_secs = deadline.as_secs(),
            "a request made no progress within the deadline"
          );
          return ApiError::Busy {
            what: "request deadline",
            retry_after_secs: DATABASE_RETRY_AFTER_SECS,
          }
          .into_response();
        }
      },
    }
  }
}

/// Whether `path` is one of `paths` or below one.
fn is_listed(paths: &[&str], path: &str) -> bool {
  paths.iter().any(|listed| {
    path
      .strip_prefix(listed)
      .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
  })
}

fn has_body(request: &Request) -> bool {
  let headers = request.headers();
  let is_empty = headers
    .get(header::CONTENT_LENGTH)
    .is_some_and(|length| length.as_bytes() == b"0");
  !is_empty
    && (headers.contains_key(header::CONTENT_LENGTH) || headers.contains_key(header::TRANSFER_ENCODING))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn listed_paths_match_whole_segments() {
    assert!(is_listed(&WITHOUT_DATABASE, "/api/v1/brand"));
    assert!(is_listed(&WITHOUT_DATABASE, "/api/v1/brand/assets/logo"));
    assert!(!is_listed(&WITHOUT_DATABASE, "/api/v1/brandnew"));
    assert!(!is_listed(&WITHOUT_DATABASE, "/api/v1/me"));
    assert!(is_listed(&WITHOUT_DEADLINE, "/api/v1/admin/import/users"));
    assert!(!is_listed(&WITHOUT_DEADLINE, "/api/v1/admin/users"));
  }
}
