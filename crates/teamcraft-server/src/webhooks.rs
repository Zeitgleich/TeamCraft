//! Outgoing webhooks (docs/webhooks.md): events the server sees anyway (channels created, members
//! joining, calls starting) are queued in the database per subscribed webhook and posted, signed,
//! by background workers. Metadata only: IDs and times, never names or content.
//!
//! A target that fails is backed off as a whole (one attempt per backoff step, not one per queued
//! event), so a recovered target is not flooded; the queue is bounded per webhook and by age; the
//! requests go only to public addresses unless the operator allowed private ones (SSRF).

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rand_core::{OsRng, RngCore as _};
use reqwest::StatusCode;
use teamcraft_crypto::webhook::{WebhookMessage, WebhookSecret};
use teamcraft_db::{DbError, NewWebhookLogEntry};
use teamcraft_net::retry::{RetryPolicy, parse_retry_after};
use teamcraft_proto::{
  CallId, ConversationId, Id as _, SystemEventKind, UnixMillis, UserId, WebhookDeliveryOutcome,
  WebhookEventData, WebhookEventId, WebhookEventKind, WebhookId, WebhookPayload,
};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};
use url::Url;

use crate::config::WebhooksConfig;
use crate::outbound::{PublicOnlyResolver, check_public_url};
use crate::state::AppState;

/// Longest error text kept for the console.
const MAX_ERROR_CHARS: usize = 300;
/// How often expired deliveries are swept.
const SWEEP_INTERVAL: Duration = Duration::from_secs(600);
const USER_AGENT: &str = concat!("TeamCraft-Webhooks/", env!("CARGO_PKG_VERSION"));

/// The HTTP clients and the wake-up signal of the delivery workers.
pub struct WebhookDispatcher {
  /// Resolves names only to public addresses.
  public: reqwest::Client,
  /// Any address; used only when `[webhooks].allow_private_targets` is on.
  private: reqwest::Client,
  allows_private: bool,
  signal: Notify,
}

/// Why a URL is not a webhook target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TargetRefusal {
  #[error("not an http(s) URL")]
  NotAUrl,
  #[error("must use https")]
  NotHttps,
  #[error("must not contain credentials")]
  Credentials,
  #[error("points to a private address")]
  Private,
}

/// How one attempt ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptResult {
  Delivered {
    status: u16,
  },
  /// Worth trying again: 5xx, 408, 429, a timeout or a connection error.
  Temporary {
    maybe_status: Option<u16>,
    error: String,
    maybe_retry_after: Option<Duration>,
  },
  /// Trying again won't help: another 4xx, a redirect, a refused target.
  Permanent {
    maybe_status: Option<u16>,
    error: String,
  },
}

/// Why an event could not be queued.
#[derive(Debug, thiserror::Error)]
pub enum WebhookEmitError {
  #[error("no ID for the event: {0}")]
  NoId(String),
  #[error("serializing the event failed: {0}")]
  Serialize(serde_json::Error),
  #[error("queueing the event failed: {0}")]
  Queue(#[from] DbError),
}

impl WebhookDispatcher {
  pub fn new(config: &WebhooksConfig) -> Result<Self, reqwest::Error> {
    // No proxy from the environment: a proxy resolves names itself, past the resolver. No
    // redirects: a redirect could point anywhere, and the signature is for this URL.
    let builder = || {
      reqwest::Client::builder()
        .timeout(Duration::from_secs(config.request_timeout_secs.max(1)))
        .connect_timeout(Duration::from_secs(config.connect_timeout_secs.max(1)))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(USER_AGENT)
    };
    Ok(Self {
      public: builder().dns_resolver(Arc::new(PublicOnlyResolver)).build()?,
      private: builder().build()?,
      allows_private: config.allow_private_targets,
      signal: Notify::new(),
    })
  }

  pub fn allows_private(&self) -> bool {
    self.allows_private
  }

  /// Posts one signed event. Never follows redirects; the answer's body is not read.
  pub async fn send(&self, url: &str, secret: &WebhookSecret, event_id: &str, body: &str) -> AttemptResult {
    let url = match check_target_url(url, self.allows_private) {
      Ok(url) => url,
      Err(refusal) => {
        return AttemptResult::Permanent {
          maybe_status: None,
          error: refusal.to_string(),
        };
      },
    };
    let timestamp_secs = unix_now_secs();
    let signature = secret.sign(&WebhookMessage {
      id: event_id,
      timestamp_secs,
      body: body.as_bytes(),
    });
    let client = if self.allows_private {
      &self.private
    } else {
      &self.public
    };
    let result = client
      .post(url)
      .header(reqwest::header::CONTENT_TYPE, "application/json")
      .header("webhook-id", event_id)
      .header("webhook-timestamp", timestamp_secs.to_string())
      .header("webhook-signature", signature)
      .body(body.to_owned())
      .send()
      .await;
    match result {
      Ok(response) => classify(response.status(), response.headers()),
      Err(error) => AttemptResult::Temporary {
        maybe_status: None,
        error: describe_send_error(error),
        maybe_retry_after: None,
      },
    }
  }

  fn notify(&self) {
    self.signal.notify_waiters();
  }
}

/// Checks a webhook URL by its text: `https` without credentials to a public host. With private
/// targets allowed, `http` and private hosts pass too. Names are checked again when connecting
/// ([`PublicOnlyResolver`]), so a name that resolves to a private address is refused then.
pub fn check_target_url(text: &str, allows_private: bool) -> Result<Url, TargetRefusal> {
  let url: Url = text.trim().parse().map_err(|_| TargetRefusal::NotAUrl)?;
  if allows_private {
    if !matches!(url.scheme(), "https" | "http") || url.host().is_none() {
      return Err(TargetRefusal::NotAUrl);
    }
    if !url.username().is_empty() || url.password().is_some() {
      return Err(TargetRefusal::Credentials);
    }
    return Ok(url);
  }
  check_public_url(&url).map_err(|refusal| match refusal {
    crate::outbound::UrlRefusal::NotAUrl => TargetRefusal::NotAUrl,
    crate::outbound::UrlRefusal::NotHttps => TargetRefusal::NotHttps,
    crate::outbound::UrlRefusal::Credentials => TargetRefusal::Credentials,
    crate::outbound::UrlRefusal::Private => TargetRefusal::Private,
  })?;
  Ok(url)
}

/// The webhook event a conversation's system event stands for, if any.
pub fn event_for_system(kind: &SystemEventKind) -> Option<WebhookEventKind> {
  match kind {
    SystemEventKind::MemberAdded => Some(WebhookEventKind::MemberJoined),
    SystemEventKind::MemberLeft | SystemEventKind::MemberRemoved => Some(WebhookEventKind::MemberLeft),
    SystemEventKind::Archived => Some(WebhookEventKind::ChannelArchived),
    SystemEventKind::Unarchived => Some(WebhookEventKind::ChannelUnarchived),
    _ => None,
  }
}

/// Tells the webhooks about an event in a conversation, if it is a channel. A failure is logged:
/// the action itself has happened.
pub async fn emit_for_conversation(
  state: &AppState,
  kind: WebhookEventKind,
  conversation: ConversationId,
  maybe_actor: Option<UserId>,
  maybe_subject: Option<UserId>,
  maybe_call: Option<CallId>,
) {
  match state.database.is_channel(conversation).await {
    Ok(true) => {},
    Ok(false) => return,
    Err(error) => {
      warn!(%error, conversation_id = %conversation, "checking a conversation for webhooks failed");
      return;
    },
  }
  emit(
    state,
    kind,
    WebhookEventData {
      maybe_conversation_id: Some(conversation),
      maybe_actor_id: maybe_actor,
      maybe_subject_id: maybe_subject,
      maybe_call_id: maybe_call,
      maybe_check: None,
    },
  )
  .await;
}

/// Queues the event for every enabled webhook subscribed to it and wakes the workers. A failure is
/// logged: the action that raised the event has happened.
pub async fn emit(state: &AppState, kind: WebhookEventKind, data: WebhookEventData) {
  if let Err(error) = try_emit(state, kind.clone(), data).await {
    warn!(%error, event = %kind, "queueing a webhook event failed");
  }
}

/// [`emit`] for a caller that tries again later when the event could not be queued (the health
/// alerts, whose database may be what failed).
pub async fn try_emit(
  state: &AppState,
  kind: WebhookEventKind,
  data: WebhookEventData,
) -> Result<(), WebhookEmitError> {
  let event_id: WebhookEventId = state
    .next_id()
    .map_err(|error| WebhookEmitError::NoId(error.to_string()))?;
  let payload = WebhookPayload {
    id: event_id.to_string(),
    kind: kind.clone(),
    created_at: UnixMillis(unix_now_millis()),
    data,
  };
  let body = serde_json::to_string(&payload).map_err(WebhookEmitError::Serialize)?;
  let max_queued = state.settings.get().webhooks.max_queued_per_webhook;
  let outcome = state
    .database
    .enqueue_webhook_event(event_id.snowflake().get(), &kind, &body, max_queued)
    .await?;
  if outcome.dropped > 0 {
    warn!(event = %kind, dropped = outcome.dropped, "webhook queues full; event dropped for them");
  }
  if outcome.queued > 0 {
    state.webhooks.notify();
  }
  Ok(())
}

/// Starts the delivery workers and the sweeper of expired deliveries.
pub fn spawn_workers(state: &AppState) -> Vec<JoinHandle<()>> {
  let workers = state.settings.get().webhooks.workers.max(1);
  let mut handles = Vec::with_capacity(workers + 1);
  for _ in 0..workers {
    handles.push(tokio::spawn(run_worker(state.clone())));
  }
  handles.push(tokio::spawn(sweep_expired(state.clone())));
  handles
}

/// Claims one due delivery and makes the attempt. Returns whether there was one.
pub async fn deliver_next(state: &AppState) -> Result<bool, DbError> {
  let Some(claim) = state.database.claim_webhook_delivery().await? else {
    return Ok(false);
  };
  let row = claim.row().clone();
  let config = state.settings.get().webhooks.clone();
  let attempt = row.attempts.saturating_add(1);
  let started = Instant::now();
  let result = match open_secret(state, row.webhook_id, &row.sealed_secret) {
    Some(secret) => {
      state
        .webhooks
        .send(&row.url, &secret, &row.event_id.to_string(), &row.payload)
        .await
    },
    None => AttemptResult::Permanent {
      maybe_status: None,
      error: "the webhook secret cannot be read with this settings key".into(),
    },
  };
  let mut log = NewWebhookLogEntry {
    id: log_id(state)?,
    webhook_id: row.webhook_id,
    event_id: row.event_id,
    event_kind: row.event_kind.clone(),
    attempt,
    outcome: WebhookDeliveryOutcome::Delivered,
    maybe_status_code: None,
    maybe_error: None,
    duration: started.elapsed(),
  };
  let keep = config.log_entries_per_webhook;
  match result {
    AttemptResult::Delivered { status } => {
      log.maybe_status_code = Some(status);
      debug!(webhook_id = %row.webhook_id, attempt, "webhook delivered");
      claim.delivered(&log, keep).await?;
    },
    AttemptResult::Temporary {
      maybe_status,
      error,
      maybe_retry_after,
    } => {
      log.maybe_status_code = maybe_status;
      log.maybe_error = Some(error.clone());
      let delay = next_delay(&config, row.consecutive_failures, maybe_retry_after, fraction());
      if attempt >= config.max_attempts.max(1) {
        info!(webhook_id = %row.webhook_id, attempt, %error, "webhook delivery given up: attempts used");
        log.outcome = WebhookDeliveryOutcome::Failed;
        claim.give_up(Some(delay), &error, &log, keep).await?;
      } else {
        debug!(webhook_id = %row.webhook_id, attempt, %error, delay_ms = delay.as_millis(), "webhook failed; backing off");
        log.outcome = WebhookDeliveryOutcome::Retrying;
        claim.retry_later(delay, &error, &log, keep).await?;
      }
    },
    AttemptResult::Permanent { maybe_status, error } => {
      log.maybe_status_code = maybe_status;
      log.maybe_error = Some(error.clone());
      log.outcome = WebhookDeliveryOutcome::Failed;
      info!(webhook_id = %row.webhook_id, attempt, %error, "webhook delivery refused for good");
      claim.give_up(None, &error, &log, keep).await?;
    },
  }
  Ok(true)
}

/// The wait after `failures` failures in a row before this one: exponential from the initial
/// delay up to the cap, with equal jitter (`fraction` in `[0, 1)`); a target's `Retry-After` is
/// honoured up to twice the cap.
pub fn next_delay(
  config: &WebhooksConfig,
  failures: u32,
  maybe_retry_after: Option<Duration>,
  fraction: f64,
) -> Duration {
  let max = Duration::from_secs(config.max_retry_delay_secs);
  let policy = RetryPolicy::new(Duration::from_secs(config.initial_retry_delay_secs.max(1)), max);
  let delay = policy.jittered(failures, fraction);
  match maybe_retry_after {
    Some(asked) => delay.max(asked.min(max.saturating_mul(2))),
    None => delay,
  }
}

/// Whether an answer counts as delivered, worth a retry, or refused for good.
pub fn classify(status: StatusCode, headers: &reqwest::header::HeaderMap) -> AttemptResult {
  let code = status.as_u16();
  if status.is_success() {
    return AttemptResult::Delivered { status: code };
  }
  let error = format!("HTTP {code}");
  if status.is_server_error()
    || status == StatusCode::REQUEST_TIMEOUT
    || status == StatusCode::TOO_MANY_REQUESTS
  {
    let maybe_retry_after = headers
      .get(reqwest::header::RETRY_AFTER)
      .and_then(|value| value.to_str().ok())
      .and_then(parse_retry_after);
    return AttemptResult::Temporary {
      maybe_status: Some(code),
      error,
      maybe_retry_after,
    };
  }
  AttemptResult::Permanent {
    maybe_status: Some(code),
    error,
  }
}

/// The secret of a webhook from its sealed form; `None` (logged) without a usable settings key.
pub fn open_secret(state: &AppState, webhook: WebhookId, sealed: &[u8]) -> Option<WebhookSecret> {
  let key = state.maybe_settings_key.as_ref()?;
  let plain = key
    .open(&secret_name(webhook), sealed)
    .inspect_err(|_| warn!(webhook_id = %webhook, "a webhook secret cannot be opened with this settings key"))
    .ok()?;
  WebhookSecret::from_bytes(plain.expose_secret()).ok()
}

/// The associated data a webhook's secret is sealed under: a sealed secret can't be moved to
/// another webhook's row.
pub fn secret_name(webhook: WebhookId) -> String {
  format!("webhook.{webhook}")
}

async fn run_worker(state: AppState) {
  loop {
    match deliver_next(&state).await {
      Ok(true) => {},
      Ok(false) => wait_for_work(&state).await,
      Err(error) => {
        warn!(%error, "the webhook delivery queue failed");
        wait_for_work(&state).await;
      },
    }
  }
}

async fn wait_for_work(state: &AppState) {
  let poll_interval = Duration::from_secs(state.settings.get().webhooks.poll_interval_secs.max(1));
  tokio::select! {
    () = state.webhooks.signal.notified() => {},
    () = tokio::time::sleep(poll_interval) => {},
  }
}

async fn sweep_expired(state: AppState) {
  let mut interval = tokio::time::interval(SWEEP_INTERVAL);
  loop {
    interval.tick().await;
    let max_age = Duration::from_secs(state.settings.get().webhooks.max_age_hours.saturating_mul(3600));
    match state.database.delete_expired_webhook_deliveries(max_age).await {
      Ok(0) => {},
      Ok(count) => info!(count, "expired webhook deliveries dropped"),
      Err(error) => warn!(%error, "sweeping expired webhook deliveries failed"),
    }
  }
}

fn log_id(state: &AppState) -> Result<i64, DbError> {
  let id: WebhookEventId = state.ids.next_id()?;
  Ok(id.snowflake().get())
}

/// What went wrong, without the URL (it may carry a token in its path or query).
fn describe_send_error(error: reqwest::Error) -> String {
  let kind = if error.is_timeout() {
    "timed out"
  } else if error.is_connect() {
    "connection failed"
  } else if error.is_request() {
    "request failed"
  } else {
    "failed"
  };
  let mut text = format!("{kind}: {}", error.without_url());
  if text.chars().count() > MAX_ERROR_CHARS {
    text = text.chars().take(MAX_ERROR_CHARS).collect();
  }
  text
}

fn fraction() -> f64 {
  f64::from(OsRng.next_u32()) / (f64::from(u32::MAX) + 1.0)
}

fn unix_now_secs() -> i64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map_or(0, |elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX))
}

fn unix_now_millis() -> i64 {
  SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| {
    i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
  })
}

#[cfg(test)]
mod tests {
  use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};

  use super::*;

  mod target_urls {
    use super::*;

    #[test]
    fn public_https_targets_pass() {
      for url in [
        "https://hooks.example.com/teamcraft",
        "https://203.0.114.10/hook",
        "https://[2606:4700::1111]/hook",
      ] {
        assert!(check_target_url(url, false).is_ok(), "{url}");
      }
    }

    #[test]
    fn private_ipv4_targets_are_refused() {
      for url in [
        "https://127.0.0.1/hook",
        "https://10.1.2.3/hook",
        "https://172.16.0.1/hook",
        "https://192.168.1.10/hook",
        "https://169.254.169.254/latest/meta-data",
        "https://100.64.0.1/hook",
        "https://0.0.0.0/hook",
        "https://localhost/hook",
        "https://intranet/hook",
        "https://service.internal/hook",
      ] {
        assert_eq!(
          check_target_url(url, false).err(),
          Some(TargetRefusal::Private),
          "{url}"
        );
      }
    }

    #[test]
    fn private_ipv6_targets_are_refused() {
      for url in [
        "https://[::1]/hook",
        "https://[fe80::1]/hook",
        "https://[fd00::1]/hook",
        "https://[::ffff:10.0.0.1]/hook",
        "https://[::ffff:127.0.0.1]/hook",
        "https://[64:ff9b::a00:1]/hook",
        "https://[::]/hook",
      ] {
        assert_eq!(
          check_target_url(url, false).err(),
          Some(TargetRefusal::Private),
          "{url}"
        );
      }
    }

    #[test]
    fn plain_http_credentials_and_other_schemes_are_refused() {
      assert_eq!(
        check_target_url("http://hooks.example.com/x", false).err(),
        Some(TargetRefusal::NotHttps)
      );
      assert_eq!(
        check_target_url("https://user:pw@hooks.example.com/x", false).err(),
        Some(TargetRefusal::Credentials)
      );
      assert_eq!(
        check_target_url("file:///etc/passwd", true).err(),
        Some(TargetRefusal::NotAUrl)
      );
      assert_eq!(
        check_target_url("not a url", false).err(),
        Some(TargetRefusal::NotAUrl)
      );
    }

    #[test]
    fn the_operator_can_allow_private_targets() {
      for url in [
        "http://127.0.0.1:8080/hook",
        "https://[fd00::1]/hook",
        "http://ci.internal/hook",
      ] {
        assert!(check_target_url(url, true).is_ok(), "{url}");
      }
      assert_eq!(
        check_target_url("http://user:pw@ci.internal/hook", true).err(),
        Some(TargetRefusal::Credentials)
      );
    }
  }

  mod backoff {
    use super::*;

    #[test]
    fn the_wait_doubles_per_failure_up_to_the_cap() {
      let config = WebhooksConfig::default();
      // Without jitter (fraction 1.0 gives the full backoff).
      assert_eq!(next_delay(&config, 0, None, 1.0), Duration::from_secs(10));
      assert_eq!(next_delay(&config, 1, None, 1.0), Duration::from_secs(20));
      assert_eq!(next_delay(&config, 3, None, 1.0), Duration::from_secs(80));
      assert_eq!(next_delay(&config, 30, None, 1.0), Duration::from_secs(3600));
    }

    #[test]
    fn jitter_keeps_at_least_half_the_backoff() {
      let config = WebhooksConfig::default();
      assert_eq!(next_delay(&config, 2, None, 0.0), Duration::from_secs(20));
      let spread = next_delay(&config, 2, None, 0.5);
      assert!(spread > Duration::from_secs(20) && spread < Duration::from_secs(40));
    }

    #[test]
    fn retry_after_is_honoured_up_to_twice_the_cap() {
      let config = WebhooksConfig::default();
      assert_eq!(
        next_delay(&config, 0, Some(Duration::from_secs(120)), 1.0),
        Duration::from_secs(120)
      );
      assert_eq!(
        next_delay(&config, 0, Some(Duration::from_secs(86_400)), 1.0),
        Duration::from_secs(7200)
      );
    }
  }

  mod answers {
    use super::*;

    #[test]
    fn success_server_errors_and_refusals_are_told_apart() {
      let headers = HeaderMap::new();
      assert_eq!(
        classify(StatusCode::NO_CONTENT, &headers),
        AttemptResult::Delivered { status: 204 }
      );
      for status in [
        StatusCode::INTERNAL_SERVER_ERROR,
        StatusCode::BAD_GATEWAY,
        StatusCode::REQUEST_TIMEOUT,
        StatusCode::TOO_MANY_REQUESTS,
      ] {
        assert!(
          matches!(classify(status, &headers), AttemptResult::Temporary { .. }),
          "{status}"
        );
      }
      for status in [
        StatusCode::BAD_REQUEST,
        StatusCode::UNAUTHORIZED,
        StatusCode::NOT_FOUND,
        StatusCode::GONE,
        StatusCode::MOVED_PERMANENTLY,
        StatusCode::FOUND,
      ] {
        assert!(
          matches!(classify(status, &headers), AttemptResult::Permanent { .. }),
          "{status}"
        );
      }
    }

    #[test]
    fn retry_after_of_a_rate_limit_is_read() {
      let mut headers = HeaderMap::new();
      headers.insert(RETRY_AFTER, HeaderValue::from_static("90"));
      assert_eq!(
        classify(StatusCode::TOO_MANY_REQUESTS, &headers),
        AttemptResult::Temporary {
          maybe_status: Some(429),
          error: "HTTP 429".into(),
          maybe_retry_after: Some(Duration::from_secs(90)),
        }
      );
    }
  }

  #[test]
  fn system_events_map_to_webhook_events() {
    assert_eq!(
      event_for_system(&SystemEventKind::MemberRemoved),
      Some(WebhookEventKind::MemberLeft)
    );
    assert_eq!(event_for_system(&SystemEventKind::Renamed), None);
    assert_eq!(event_for_system(&SystemEventKind::MissedCall), None);
  }
}
