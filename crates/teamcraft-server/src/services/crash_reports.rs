//! The crash report relay (docs/crash-reports.md, "Sending"): the apps post their scrubbed
//! envelopes to their own server, which forwards them to the backend the admin set.
//!
//! Why a relay: the web client's content security policy only lets it talk to its own origin, the
//! desktop page only to the shell; the backend sees the server's address instead of every user's;
//! and the apps need no second origin to reach through proxies and captive portals. The server
//! learns nothing new: reports are scrubbed on the device and carry no content.

use std::backtrace::Backtrace;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use reqwest::header::{CONTENT_TYPE, RETRY_AFTER};
use teamcraft_client_api::crash_reporting::trace;
use teamcraft_client_api::crash_reporting::{
  AppInfo, AppPlatform, CrashQueue, CrashReporter, CrashReportingConfig, CrashSendOutcome, EventId,
  InstallationId, PanicParts,
};
use teamcraft_proto::{Dsn, ErrorCode, MAX_CRASH_ENVELOPE_BYTES};
use tokio::sync::Notify;
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
/// The server's own panics reported per process at most; one per place in the code.
const MAX_SERVER_PANIC_REPORTS: u32 = 20;
/// The publisher's crash reports service, built in by whoever builds the server and the apps
/// (the release workflow sets it from a repository variable); empty or absent in other builds.
const PUBLISHER_DSN: Option<&str> = option_env!("TEAMCRAFT_CRASH_REPORTS_DSN");

/// The HTTP client that forwards reports, and the publisher's service of this build.
pub struct CrashRelay {
  http: reqwest::Client,
  allows_private: bool,
  maybe_publisher: Option<Dsn>,
}

impl CrashRelay {
  /// `allows_private`: tests against a backend on this machine. Otherwise the backend must be a
  /// public https address, resolved to public addresses only (an admin can't point the relay into
  /// the server's own network).
  pub fn new(allows_private: bool) -> Result<Self, reqwest::Error> {
    Self::with_publisher(allows_private, publisher_dsn())
  }

  /// With another publisher service than the build's (tests).
  pub fn with_publisher(allows_private: bool, maybe_publisher: Option<Dsn>) -> Result<Self, reqwest::Error> {
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
    Ok(Self {
      http,
      allows_private,
      maybe_publisher,
    })
  }

  /// The publisher's service of this build, if it has one.
  pub fn publisher(&self) -> Option<&Dsn> {
    self.maybe_publisher.as_ref()
  }
}

/// [`PUBLISHER_DSN`], parsed; a value that isn't a DSN is ignored with a warning.
fn publisher_dsn() -> Option<Dsn> {
  let text = PUBLISHER_DSN.map(str::trim).filter(|text| !text.is_empty())?;
  match text.parse() {
    Ok(dsn) => Some(dsn),
    Err(error) => {
      warn!(%error, "the built-in crash reports DSN is invalid; the publisher service is off");
      None
    },
  }
}

/// Forwards one envelope to the backend in the instance settings.
pub async fn relay(state: &AppState, body: Bytes) -> Result<(), ApiError> {
  let relay = &state.crash_relay;
  let Some(dsn) = state.settings.get().crash_reports.backend(relay.publisher()) else {
    return Err(ApiError::NotFound {
      code: ErrorCode::CrashReportsOff,
      what: "crash reports",
    });
  };
  check_envelope(&body)?;
  let url = Url::parse(&dsn.envelope_url(env!("CARGO_PKG_VERSION")))
    .map_err(|_| ApiError::Unavailable("the crash reports DSN is invalid".into()))?;
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

/// Reports the server's own panics to the backend in the instance settings, when crash reports
/// are on (docs/crash-reports.md, "The server"). The report is built and scrubbed in the hook and
/// sent from a thread of its own, so a panicking request never waits for the backend; at most
/// [`MAX_SERVER_PANIC_REPORTS`] per process, one per place in the code. Called after the previous
/// hook was installed (logging), which it keeps.
pub fn install_panic_hook(state: AppState) {
  let installation = InstallationId::from_random_bytes(random_bytes());
  let seen = Mutex::new(HashSet::<(String, u32)>::new());
  let sent = AtomicU32::new(0);
  let previous = std::panic::take_hook();
  std::panic::set_hook(Box::new(move |info| {
    previous(info);
    let Some(config) = state
      .settings
      .get()
      .crash_reports
      .info(state.crash_relay.publisher())
      .and_then(|info| CrashReportingConfig::from_info(&info).ok())
    else {
      return;
    };
    let parts = PanicParts::from_hook(info, Backtrace::force_capture().to_string());
    let place = parts
      .maybe_location
      .as_ref()
      .map(|(file, line, _)| (file.clone(), *line))
      .unwrap_or_default();
    let is_new = seen.lock().is_ok_and(|mut seen| seen.insert(place));
    if !is_new || sent.fetch_add(1, Ordering::Relaxed) >= MAX_SERVER_PANIC_REPORTS {
      return;
    }
    let url = config.dsn.envelope_url(env!("CARGO_PKG_VERSION"));
    let mut reporter = CrashReporter::new(server_app(), installation, CrashQueue::default());
    reporter.set_config(Some(config));
    let event = parts.into_event(EventId::from_random_bytes(random_bytes()), now_ms(), Vec::new());
    reporter.capture(event, 0.0);
    let Some(pending) = reporter.next_send(now_ms()) else {
      return;
    };
    let allows_private = state.crash_relay.allows_private;
    // A thread with its own runtime: the hook may run on a runtime thread that must not block.
    let spawned = std::thread::Builder::new()
      .name("crash-report".into())
      .spawn(move || send_blocking(&url, pending.request.body, allows_private));
    if let Err(error) = spawned {
      warn!(%error, "could not start sending a crash report");
    }
  }));
}

/// How often the trace sender looks at the instance's setting without a kept trace waking it: an
/// admin who turns crash reports on or off is followed within this time.
const TRACE_SETTINGS_RECHECK: Duration = Duration::from_secs(60);

/// Sends the server's kept traces (docs/tracing.md): slow or failed requests with their SQL
/// statements, and a sample, to the backend in the instance settings. The trace layer follows the
/// setting (off: nothing is collected); a kept trace wakes the sender, which also looks at the
/// setting once a [`TRACE_SETTINGS_RECHECK`]. Bounded like the apps' queue; a slow or dead backend
/// only delays traces, never a request.
pub fn spawn_trace_sender(state: AppState) {
  static WAKE: std::sync::OnceLock<Arc<Notify>> = std::sync::OnceLock::new();
  let wake = WAKE.get_or_init(|| Arc::new(Notify::new())).clone();
  trace::set_kept_hook(|| {
    if let Some(wake) = WAKE.get() {
      wake.notify_one();
    }
  });
  let installation = InstallationId::from_random_bytes(random_bytes());
  tokio::spawn(async move {
    let mut reporter = CrashReporter::new(server_app(), installation, CrashQueue::default());
    loop {
      let maybe_config = current_config(&state);
      reporter.set_config(maybe_config.clone());
      trace::configure(reporter.trace_settings());
      if let Some(config) = maybe_config {
        reporter.capture_traces(&trace::take_kept(), random_bytes);
        send_traces(&state, &mut reporter, &config).await;
      } else {
        let _ = trace::take_kept();
      }
      // Either wakes it: a kept trace, or the time to look at the setting again.
      let _ = tokio::time::timeout(TRACE_SETTINGS_RECHECK, wake.notified()).await;
    }
  });
}

fn current_config(state: &AppState) -> Option<CrashReportingConfig> {
  state
    .settings
    .get()
    .crash_reports
    .info(state.crash_relay.publisher())
    .and_then(|info| CrashReportingConfig::from_info(&info).ok())
}

/// One flush of the queued traces, one envelope per request, until the queue or the flush's share
/// is through or the backend asks to wait.
async fn send_traces(state: &AppState, reporter: &mut CrashReporter, config: &CrashReportingConfig) {
  let Ok(url) = Url::parse(&config.dsn.envelope_url(env!("CARGO_PKG_VERSION"))) else {
    return;
  };
  let relay = &state.crash_relay;
  if !relay.allows_private && check_public_url(&url).is_err() {
    return;
  }
  reporter.begin_flush();
  while let Some(pending) = reporter.next_send(now_ms()) {
    let sent = tokio::time::timeout(
      FORWARD_TIMEOUT,
      relay
        .http
        .post(url.clone())
        .header(CONTENT_TYPE, ENVELOPE_CONTENT_TYPE)
        .body(pending.request.body)
        .send(),
    )
    .await;
    let outcome = match sent {
      Ok(Ok(response)) => outcome_of(response.status().as_u16(), response.headers().get(RETRY_AFTER)),
      _ => CrashSendOutcome::Unreachable,
    };
    if !reporter.record_outcome(pending.event_id, outcome, now_ms()) {
      break;
    }
  }
}

/// The backend's answer to a send of the server's own.
fn outcome_of(status: u16, maybe_retry_after: Option<&reqwest::header::HeaderValue>) -> CrashSendOutcome {
  match answer(status, maybe_retry_after) {
    Ok(()) => CrashSendOutcome::Accepted,
    Err(ApiError::RateLimited { retry_after_secs }) => CrashSendOutcome::RateLimited {
      retry_after: Duration::from_secs(retry_after_secs),
    },
    Err(ApiError::Unavailable(_)) => CrashSendOutcome::Unreachable,
    Err(_) => CrashSendOutcome::Rejected,
  }
}

fn send_blocking(url: &str, body: Vec<u8>, allows_private: bool) {
  let Ok(parsed) = Url::parse(url) else {
    return;
  };
  if !allows_private && check_public_url(&parsed).is_err() {
    return;
  }
  let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
    return;
  };
  runtime.block_on(async move {
    let Ok(relay) = CrashRelay::new(allows_private) else {
      return;
    };
    let result = relay
      .http
      .post(parsed)
      .header(CONTENT_TYPE, ENVELOPE_CONTENT_TYPE)
      .body(body)
      .send()
      .await;
    if let Err(error) = result {
      debug!(error = %error.without_url(), "the server's crash report did not reach the backend");
    }
  });
}

fn server_app() -> AppInfo {
  AppInfo {
    version: env!("CARGO_PKG_VERSION").to_owned(),
    platform: AppPlatform::Server,
    os_name: std::env::consts::OS.to_owned(),
    maybe_os_version: None,
  }
}

fn random_bytes() -> [u8; 16] {
  use rand_core::RngCore;
  let mut bytes = [0; 16];
  rand_core::OsRng.fill_bytes(&mut bytes);
  bytes
}

fn now_ms() -> i64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_or(0, |elapsed| {
      i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
    })
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

/// Item types the relay forwards: crash and error reports, and performance data (issue #1521).
/// Anything else (attachments, sessions, replays) is not what the apps send and is refused.
const RELAYED_ITEM_TYPES: [&str; 2] = ["event", "transaction"];

/// An envelope: within the size limit, its first line a JSON object (the envelope header), then
/// one or more items whose type the relay forwards. The payloads are the backend's to read; the
/// server doesn't parse reports further.
fn check_envelope(body: &[u8]) -> Result<(), ApiError> {
  if body.len() > MAX_CRASH_ENVELOPE_BYTES {
    return Err(ApiError::PayloadTooLarge(format!(
      "a crash report may have at most {MAX_CRASH_ENVELOPE_BYTES} bytes"
    )));
  }
  let not_envelope = || ApiError::InvalidRequest("not a crash report envelope".into());
  let (header, mut rest) = split_line(body).ok_or_else(not_envelope)?;
  if !matches!(serde_json::from_slice(header), Ok(serde_json::Value::Object(_))) {
    return Err(not_envelope());
  }
  let mut items = 0;
  while !rest.iter().all(u8::is_ascii_whitespace) {
    let (item_header, after) = split_line(rest).unwrap_or((rest, &[]));
    let item: ItemHeader = serde_json::from_slice(item_header).map_err(|_| not_envelope())?;
    if !RELAYED_ITEM_TYPES.contains(&item.item_type.as_str()) {
      return Err(ApiError::InvalidRequest(
        "the relay forwards only events and transactions".into(),
      ));
    }
    rest = match item.maybe_length {
      Some(length) => {
        let payload_end = usize::try_from(length)
          .ok()
          .filter(|end| *end <= after.len())
          .ok_or_else(not_envelope)?;
        let tail = &after[payload_end..];
        tail.strip_prefix(b"\n").unwrap_or(tail)
      },
      None => split_line(after).map_or(&[][..], |(_, tail)| tail),
    };
    items += 1;
  }
  if items == 0 {
    return Err(not_envelope());
  }
  Ok(())
}

/// An item's header; the rest of it is the backend's.
#[derive(serde::Deserialize)]
struct ItemHeader {
  #[serde(rename = "type")]
  item_type: String,
  #[serde(default, rename = "length")]
  maybe_length: Option<u64>,
}

/// The line up to the first `\n` and what follows it; `None` without a line break.
fn split_line(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
  let end = bytes.iter().position(|byte| *byte == b'\n')?;
  Some((&bytes[..end], &bytes[end + 1..]))
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
    assert!(check_envelope(b"{}\n").is_err(), "no item");
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
  fn transactions_pass_and_other_item_types_do_not() {
    let payload = r#"{"type":"transaction","transaction":"channel.switch"}"#;
    let with_length = format!(
      "{{\"event_id\":\"x\"}}\n{{\"type\":\"transaction\",\"length\":{}}}\n{payload}\n",
      payload.len()
    );
    assert!(check_envelope(with_length.as_bytes()).is_ok());
    assert!(
      check_envelope(b"{}\n{\"type\":\"transaction\"}\n{}").is_ok(),
      "no length, no final newline"
    );
    // A length can hold line breaks inside the payload.
    assert!(check_envelope(b"{}\n{\"type\":\"event\",\"length\":3}\na\nb\n").is_ok());
    for refused in [
      &b"{}\n{\"type\":\"attachment\",\"length\":2}\nab\n"[..],
      b"{}\n{\"type\":\"event\"}\n{}\n{\"type\":\"replay_event\"}\n{}\n",
      b"{}\n{\"type\":\"event\",\"length\":99}\n{}\n",
      b"{}\n{\"length\":2}\n{}\n",
    ] {
      assert_eq!(
        check_envelope(refused).unwrap_err().status(),
        StatusCode::BAD_REQUEST,
        "{}",
        String::from_utf8_lossy(refused)
      );
    }
  }

  #[test]
  fn backend_answers_to_the_servers_traces_map_to_queue_outcomes() {
    assert_eq!(outcome_of(202, None), CrashSendOutcome::Accepted);
    assert_eq!(
      outcome_of(429, Some(&HeaderValue::from_static("5"))),
      CrashSendOutcome::RateLimited {
        retry_after: Duration::from_secs(5)
      }
    );
    assert_eq!(outcome_of(400, None), CrashSendOutcome::Rejected);
    assert_eq!(outcome_of(502, None), CrashSendOutcome::Unreachable);
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
