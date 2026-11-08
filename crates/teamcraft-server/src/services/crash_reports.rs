//! The crash report relay (docs/crash-reports.md, "Sending"): the apps post their scrubbed
//! envelopes to their own server, which forwards them to the backend the admin set.
//!
//! Why a relay: the web client's content security policy only lets it talk to its own origin, the
//! desktop page only to the shell; the backend sees the server's address instead of every user's;
//! and the apps need no second origin to reach through proxies and captive portals. The server
//! learns nothing new: reports are scrubbed on the device and carry no content.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use reqwest::header::{CONTENT_TYPE, RETRY_AFTER};
use teamcraft_proto::{Dsn, ErrorCode, MAX_CRASH_ENVELOPE_BYTES};
use tracing::{debug, warn};
use url::Url;

use crate::error::ApiError;
use crate::outbound::{PublicOnlyResolver, check_public_url};
use crate::state::AppState;

/// One forward, connect to answer. The app's own send has the same bound and keeps the report
/// queued when it runs out.
const FORWARD_TIMEOUT: Duration = Duration::from_secs(8);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
/// What the server waits before trying again when the backend gives no `Retry-After`.
const DEFAULT_RETRY_AFTER_SECS: u64 = 60;
/// Longest wait passed on to the apps.
const MAX_RETRY_AFTER_SECS: u64 = 6 * 60 * 60;
const ENVELOPE_CONTENT_TYPE: &str = "application/x-sentry-envelope";

/// The HTTP client that forwards reports.
pub struct CrashRelay {
  http: reqwest::Client,
  allows_private: bool,
}

impl CrashRelay {
  /// `allows_private`: tests against a backend on this machine. Otherwise the backend must be a
  /// public https address, resolved to public addresses only (an admin can't point the relay into
  /// the server's own network).
  pub fn new(allows_private: bool) -> Result<Self, reqwest::Error> {
    let builder = reqwest::Client::builder()
      .timeout(FORWARD_TIMEOUT)
      .connect_timeout(CONNECT_TIMEOUT)
      .no_proxy()
      .redirect(reqwest::redirect::Policy::none())
      .user_agent(concat!(
        "TeamCraft/",
        env!("CARGO_PKG_VERSION"),
        " (crash reports)"
      ));
    let http = if allows_private {
      builder.build()?
    } else {
      builder.dns_resolver(Arc::new(PublicOnlyResolver)).build()?
    };
    Ok(Self { http, allows_private })
  }
}

/// Forwards one envelope to the backend in the instance settings.
pub async fn relay(state: &AppState, body: Bytes) -> Result<(), ApiError> {
  let Some(info) = state.settings.get().crash_reports.info() else {
    return Err(ApiError::NotFound {
      code: ErrorCode::CrashReportsOff,
      what: "crash reports",
    });
  };
  check_envelope(&body)?;
  let dsn: Dsn = info
    .dsn
    .parse()
    .map_err(|_| ApiError::Unavailable("the crash reports DSN is invalid".into()))?;
  let url = Url::parse(&dsn.envelope_url(env!("CARGO_PKG_VERSION")))
    .map_err(|_| ApiError::Unavailable("the crash reports DSN is invalid".into()))?;
  let relay = &state.crash_relay;
  if !relay.allows_private && check_public_url(&url).is_err() {
    warn!("the crash reports backend is not a public https address; reports are not forwarded");
    return Err(ApiError::Unavailable(
      "the crash reports backend is not reachable".into(),
    ));
  }
  let response = relay
    .http
    .post(url)
    .header(CONTENT_TYPE, ENVELOPE_CONTENT_TYPE)
    .body(body)
    .send()
    .await
    .map_err(|error| {
      debug!(error = %error.without_url(), "crash reports backend unreachable");
      ApiError::Unavailable("the crash reports backend is not reachable".into())
    })?;
  answer(response.status().as_u16(), response.headers().get(RETRY_AFTER))
}

/// Maps the backend's answer to the app's: accepted, wait, drop the report, or try later.
fn answer(status: u16, maybe_retry_after: Option<&reqwest::header::HeaderValue>) -> Result<(), ApiError> {
  let retry_after_secs = || {
    maybe_retry_after
      .and_then(|value| value.to_str().ok())
      .and_then(|value| value.trim().parse::<u64>().ok())
      .unwrap_or(DEFAULT_RETRY_AFTER_SECS)
      .min(MAX_RETRY_AFTER_SECS)
  };
  match status {
    200..=299 => Ok(()),
    429 => Err(ApiError::RateLimited {
      retry_after_secs: retry_after_secs(),
    }),
    400..=499 => {
      debug!(status, "the crash reports backend refused a report");
      Err(ApiError::InvalidRequest(
        "the crash reports backend refused the report".into(),
      ))
    },
    _ => Err(ApiError::Unavailable("the crash reports backend failed".into())),
  }
}

/// An envelope: within the size limit, and its first line a JSON object (the envelope header).
/// The rest is the backend's to read; the server doesn't parse reports further.
fn check_envelope(body: &[u8]) -> Result<(), ApiError> {
  if body.len() > MAX_CRASH_ENVELOPE_BYTES {
    return Err(ApiError::PayloadTooLarge(format!(
      "a crash report may have at most {MAX_CRASH_ENVELOPE_BYTES} bytes"
    )));
  }
  let header = body.split(|byte| *byte == b'\n').next().unwrap_or_default();
  match serde_json::from_slice::<serde_json::Value>(header) {
    Ok(serde_json::Value::Object(_)) if body.contains(&b'\n') => Ok(()),
    _ => Err(ApiError::InvalidRequest("not a crash report envelope".into())),
  }
}

#[cfg(test)]
mod tests {
  use axum::http::StatusCode;
  use reqwest::header::HeaderValue;

  use super::*;

  #[test]
  fn only_envelopes_within_the_limit_pass() {
    assert!(check_envelope(b"{\"event_id\":\"x\"}\n{\"type\":\"event\"}\n{}\n").is_ok());
    assert!(check_envelope(b"{}").is_err(), "no item");
    assert!(check_envelope(b"[1]\n{}\n").is_err());
    assert!(check_envelope(b"<html>\n").is_err());
    let mut huge = b"{}\n".to_vec();
    huge.resize(MAX_CRASH_ENVELOPE_BYTES + 1, b' ');
    assert_eq!(
      check_envelope(&huge).unwrap_err().status(),
      StatusCode::PAYLOAD_TOO_LARGE
    );
  }

  #[test]
  fn backend_answers_map_to_what_the_app_does() {
    assert!(answer(200, None).is_ok());
    let limited = answer(429, Some(&HeaderValue::from_static("120"))).unwrap_err();
    assert_eq!(limited.retry_after_secs(), Some(120));
    let capped = answer(429, Some(&HeaderValue::from_static("999999"))).unwrap_err();
    assert_eq!(capped.retry_after_secs(), Some(MAX_RETRY_AFTER_SECS));
    assert_eq!(answer(403, None).unwrap_err().status(), StatusCode::BAD_REQUEST);
    let failed = answer(502, None).unwrap_err();
    assert_eq!(failed.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(failed.retry_after_secs().is_some());
  }
}
