//! Request hardening in front of the routes: per-client and per-user rate limits, protection against
//! token guessing, and the security headers of API responses.
//!
//! The limits are token buckets kept in memory per server instance (config `[security]`). They are a
//! first line against abuse, not a quota system: a determined attacker with many addresses needs
//! the reverse proxy's limits as well.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::header::{
  CACHE_CONTROL, CONTENT_SECURITY_POLICY, REFERRER_POLICY, STRICT_TRANSPORT_SECURITY, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use teamcraft_proto::UserId;

use crate::config::RateLimitConfig;
use crate::error::ApiError;
use crate::state::AppState;

/// More entries than this trigger a sweep of idle buckets.
const SWEEP_THRESHOLD: usize = 50_000;
const API_CSP: &str = "default-src 'none'; frame-ancestors 'none'";
/// How long an address the import account authenticated from passes the per-address limit
/// (admin.md §3.8); each of its requests renews it.
const IMPORT_EXEMPTION: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
  Ip,
  UserWrite,
  Public,
  AuthFailure,
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
  tokens: f64,
  updated_at: Instant,
}

/// The rate limiter state.
#[derive(Debug)]
pub struct RateLimits {
  config: RateLimitConfig,
  buckets: Mutex<HashMap<(Kind, String), Bucket>>,
  /// E-mails the meetings of one organizer may still send, refilled over an hour. A quota, not a
  /// rate limit: it applies even when rate limits are switched off.
  mail_budgets: Mutex<HashMap<UserId, Bucket>>,
  /// Addresses the import account sent an authenticated request from, until when they pass the
  /// per-address limit: the address limit runs before authentication, so the importer's address is
  /// learned from its requests.
  import_addresses: Mutex<HashMap<String, Instant>>,
}

impl RateLimits {
  pub fn new(config: &RateLimitConfig) -> Self {
    Self {
      config: config.clone(),
      buckets: Mutex::new(HashMap::new()),
      mail_budgets: Mutex::new(HashMap::new()),
      import_addresses: Mutex::new(HashMap::new()),
    }
  }

  /// The import account authenticated from `ip`: its requests from there pass the per-address
  /// limit for [`IMPORT_EXEMPTION`].
  pub fn note_import_address(&self, ip: &str, now: Instant) {
    let mut addresses = self
      .import_addresses
      .lock()
      .unwrap_or_else(PoisonError::into_inner);
    addresses.retain(|_, until| *until > now);
    addresses.insert(ip.to_owned(), now + IMPORT_EXEMPTION);
  }

  fn is_import_address(&self, ip: &str, now: Instant) -> bool {
    self
      .import_addresses
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .get(ip)
      .is_some_and(|until| *until > now)
  }

  /// Spends `count` e-mails of the user's hourly budget of `per_hour`. `false` (and nothing spent)
  /// if the budget does not cover them all.
  pub fn spend_mails(&self, user: UserId, count: u32, per_hour: u32) -> bool {
    let capacity = f64::from(per_hour);
    let now = Instant::now();
    let mut budgets = self.mail_budgets.lock().unwrap_or_else(PoisonError::into_inner);
    let bucket = budgets.entry(user).or_insert(Bucket {
      tokens: capacity,
      updated_at: now,
    });
    let elapsed = now.saturating_duration_since(bucket.updated_at).as_secs_f64();
    bucket.tokens = (bucket.tokens + elapsed * capacity / 3600.0).min(capacity);
    bucket.updated_at = now;
    let needed = f64::from(count);
    if bucket.tokens >= needed {
      bucket.tokens -= needed;
      true
    } else {
      false
    }
  }

  /// Counts a write of `user`; fails with 429 when the user is over the limit.
  pub fn check_user_write(&self, user: UserId) -> Result<(), ApiError> {
    self.take(Kind::UserWrite, &user.to_string(), Instant::now())
  }

  fn check_ip(&self, ip: &str, now: Instant) -> Result<(), ApiError> {
    self.take(Kind::Ip, ip, now)
  }

  fn check_public(&self, ip: &str, now: Instant) -> Result<(), ApiError> {
    self.take(Kind::Public, ip, now)
  }

  /// Fails if the address used up its failed authentications.
  fn check_not_blocked(&self, ip: &str, now: Instant) -> Result<(), ApiError> {
    if !self.config.is_enabled {
      return Ok(());
    }
    let mut buckets = self.lock();
    let rate = self.rate(Kind::AuthFailure);
    let bucket = buckets
      .entry((Kind::AuthFailure, ip.to_owned()))
      .or_insert(Bucket {
        tokens: rate,
        updated_at: now,
      });
    refill(bucket, rate, now);
    if bucket.tokens >= 1.0 {
      Ok(())
    } else {
      Err(retry_after(bucket.tokens, rate))
    }
  }

  pub fn record_auth_failure(&self, ip: &str, now: Instant) {
    if !self.config.is_enabled {
      return;
    }
    let mut buckets = self.lock();
    let rate = self.rate(Kind::AuthFailure);
    let bucket = buckets
      .entry((Kind::AuthFailure, ip.to_owned()))
      .or_insert(Bucket {
        tokens: rate,
        updated_at: now,
      });
    refill(bucket, rate, now);
    bucket.tokens = (bucket.tokens - 1.0).max(0.0);
  }

  fn take(&self, kind: Kind, key: &str, now: Instant) -> Result<(), ApiError> {
    if !self.config.is_enabled {
      return Ok(());
    }
    let rate = self.rate(kind);
    let mut buckets = self.lock();
    if buckets.len() > SWEEP_THRESHOLD {
      buckets.retain(|(kind, _), bucket| {
        let rate = self.rate(*kind);
        refill(bucket, rate, now);
        bucket.tokens < rate
      });
    }
    let bucket = buckets.entry((kind, key.to_owned())).or_insert(Bucket {
      tokens: rate,
      updated_at: now,
    });
    refill(bucket, rate, now);
    if bucket.tokens >= 1.0 {
      bucket.tokens -= 1.0;
      Ok(())
    } else {
      Err(retry_after(bucket.tokens, rate))
    }
  }

  /// Burst size (and per-minute rate) of a limit.
  fn rate(&self, kind: Kind) -> f64 {
    let per_minute = match kind {
      Kind::Ip => self.config.ip_requests_per_minute,
      Kind::UserWrite => self.config.user_writes_per_minute,
      Kind::Public => self.config.public_requests_per_minute,
      Kind::AuthFailure => self.config.auth_failures_per_minute,
    };
    f64::from(per_minute.max(1))
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(Kind, String), Bucket>> {
    // The buckets are plain numbers, so a poisoned lock is still consistent.
    self.buckets.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

fn refill(bucket: &mut Bucket, per_minute: f64, now: Instant) {
  let elapsed = now.saturating_duration_since(bucket.updated_at).as_secs_f64();
  bucket.tokens = (bucket.tokens + elapsed * per_minute / 60.0).min(per_minute);
  bucket.updated_at = now;
}

fn retry_after(tokens: f64, per_minute: f64) -> ApiError {
  let seconds = ((1.0 - tokens) * 60.0 / per_minute).ceil().max(1.0);
  // Truncation is intended: a few seconds at most for realistic settings.
  #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
  ApiError::RateLimited {
    retry_after_secs: seconds as u64,
  }
}

/// The address a request counts for, put into the request by [`guard`] so that long-lived
/// connections (gateway, rendezvous) can count failed authentications against it.
#[derive(Debug, Clone)]
pub struct ClientIp(pub String);

/// The middleware around every route: rate limits, then the response headers.
pub async fn guard(State(state): State<AppState>, request: Request, next: Next) -> Response {
  let path = request.uri().path().to_owned();
  let is_api = path.starts_with("/api/") || path == "/healthz" || path == "/readyz";
  if !is_api {
    // The web client sets its own headers (see `web`).
    return next.run(request).await;
  }
  let settings = &state.settings.get().security;
  let ip = client_ip(&request, settings.trust_forwarded_for);
  let mut request = request;
  warn_once_if_behind_an_unconfigured_proxy(&request, settings.trust_forwarded_for);
  request.extensions_mut().insert(ClientIp(ip.clone()));
  let now = Instant::now();
  let limits = &state.rate_limits;
  let refusal = if limits.is_import_address(&ip, now) {
    Ok(())
  } else {
    limits.check_ip(&ip, now)
  };
  let refusal = refusal
    .and_then(|()| limits.check_not_blocked(&ip, now))
    .and_then(|()| {
      if is_unauthenticated_endpoint(&path) {
        limits.check_public(&ip, now)
      } else {
        Ok(())
      }
    });
  let mut response = match refusal {
    Ok(()) => next.run(request).await,
    Err(error) => error.into_response(),
  };
  if response.status() == StatusCode::UNAUTHORIZED {
    limits.record_auth_failure(&ip, Instant::now());
  }
  add_headers(response.headers_mut(), settings.hsts);
  response
}

/// Endpoints whose callers have no user credentials: their abuse is limited per address.
fn is_unauthenticated_endpoint(path: &str) -> bool {
  path == "/api/v1/meetings/join"
    || path == "/api/v1/auth/backchannel-logout"
    || path.starts_with("/api/v1/shares/")
    || path == "/api/v1/push/web/subscription-change"
    || path == "/api/v1/auth/local/activate"
    || path == "/api/v1/auth/local/login"
    || path.starts_with("/api/internal/")
}

fn add_headers(headers: &mut HeaderMap, hsts: bool) {
  headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
  headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
  headers.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static(API_CSP));
  // Responses carry per-user data (and ciphertext): no shared cache may keep them.
  headers
    .entry(CACHE_CONTROL)
    .or_insert(HeaderValue::from_static("no-store"));
  if hsts {
    headers.insert(
      STRICT_TRANSPORT_SECURITY,
      HeaderValue::from_static("max-age=31536000; includeSubDomains"),
    );
  }
}

/// Behind a reverse proxy without `trust_forwarded_for`, every client has the proxy's address: they
/// share one rate limit and bad tokens from anybody block everybody. A request that comes from a
/// private address and carries `X-Forwarded-For` gives it away; say so once.
fn warn_once_if_behind_an_unconfigured_proxy(request: &Request, trust_forwarded_for: bool) {
  static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
  let peer = request
    .extensions()
    .get::<ConnectInfo<SocketAddr>>()
    .map(|info| info.0.ip());
  if looks_like_an_unconfigured_proxy(request.headers(), peer, trust_forwarded_for)
    && !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed)
  {
    tracing::warn!(
      "requests arrive from a private address with X-Forwarded-For: if a reverse proxy sits in front \
       of the server, set security.trust_forwarded_for = true, or all clients share one rate limit"
    );
  }
}

fn looks_like_an_unconfigured_proxy(
  headers: &HeaderMap,
  peer: Option<IpAddr>,
  trust_forwarded_for: bool,
) -> bool {
  if trust_forwarded_for || !headers.contains_key("x-forwarded-for") {
    return false;
  }
  peer.is_some_and(|ip| match normalize_ip(ip) {
    IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
    IpAddr::V6(v6) => v6.is_loopback() || (v6.segments()[0] & 0xfe00) == 0xfc00,
  })
}

/// An IPv4-mapped IPv6 address as the IPv4 address it stands for.
fn normalize_ip(ip: IpAddr) -> IpAddr {
  match ip {
    IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
    other => other,
  }
}

/// The address a request counts for: the connection's, or with a trusted proxy the last entry of
/// `X-Forwarded-For` (the client as the proxy saw it; earlier entries are client-supplied).
fn client_ip(request: &Request, trust_forwarded_for: bool) -> String {
  if trust_forwarded_for
    && let Some(forwarded) = request
      .headers()
      .get("x-forwarded-for")
      .and_then(|value| value.to_str().ok())
      .and_then(|value| value.rsplit(',').next())
      .and_then(|entry| entry.trim().parse::<IpAddr>().ok())
  {
    return normalize(forwarded);
  }
  request
    .extensions()
    .get::<ConnectInfo<SocketAddr>>()
    .map_or_else(|| "unknown".to_owned(), |info| normalize(info.0.ip()))
}

/// IPv4-mapped IPv6 addresses (from dual-stack sockets) count as the IPv4 address; IPv6 clients
/// count per /64, since one subscriber usually controls a whole /64.
fn normalize(ip: IpAddr) -> String {
  match ip {
    IpAddr::V4(v4) => v4.to_string(),
    IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
      Some(v4) => v4.to_string(),
      None => {
        let segments = v6.segments();
        format!(
          "{:x}:{:x}:{:x}:{:x}::/64",
          segments[0], segments[1], segments[2], segments[3]
        )
      },
    },
  }
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use axum::http::HeaderValue;

  use super::*;

  #[test]
  fn a_private_peer_with_forwarded_for_looks_like_an_unconfigured_proxy() {
    let with = |value: Option<&str>| {
      let mut headers = HeaderMap::new();
      if let Some(value) = value {
        headers.insert("x-forwarded-for", HeaderValue::from_str(value).unwrap());
      }
      headers
    };
    let proxy: Option<IpAddr> = Some("10.0.0.2".parse().unwrap());
    let xff = with(Some("203.0.113.9"));
    assert!(looks_like_an_unconfigured_proxy(&xff, proxy, false));
    assert!(looks_like_an_unconfigured_proxy(
      &xff,
      Some("::ffff:172.16.0.1".parse().unwrap()),
      false
    ));
    assert!(looks_like_an_unconfigured_proxy(
      &xff,
      Some("127.0.0.1".parse().unwrap()),
      false
    ));
    // Configured, direct clients, public peers and unknown peers do not trigger it.
    assert!(!looks_like_an_unconfigured_proxy(&xff, proxy, true));
    assert!(!looks_like_an_unconfigured_proxy(&with(None), proxy, false));
    assert!(!looks_like_an_unconfigured_proxy(
      &xff,
      Some("198.51.100.4".parse().unwrap()),
      false
    ));
    assert!(!looks_like_an_unconfigured_proxy(&xff, None, false));
  }

  fn limits(edit: impl FnOnce(&mut RateLimitConfig)) -> RateLimits {
    let mut config = RateLimitConfig::default();
    edit(&mut config);
    RateLimits::new(&config)
  }

  mod buckets {
    use super::*;

    #[test]
    fn the_import_accounts_address_passes_for_a_while() {
      let limits = limits(|_| ());
      let start = Instant::now();
      assert!(!limits.is_import_address("1.2.3.4", start));
      limits.note_import_address("1.2.3.4", start);
      assert!(limits.is_import_address("1.2.3.4", start + Duration::from_secs(60)));
      assert!(!limits.is_import_address("5.6.7.8", start));
      assert!(!limits.is_import_address("1.2.3.4", start + IMPORT_EXEMPTION));
    }

    #[test]
    fn a_burst_is_allowed_then_refused_with_a_retry_time_and_refills() {
      let limits = limits(|c| c.ip_requests_per_minute = 60);
      let start = Instant::now();
      for _ in 0..60 {
        assert!(limits.check_ip("1.2.3.4", start).is_ok());
      }
      let Err(ApiError::RateLimited { retry_after_secs }) = limits.check_ip("1.2.3.4", start) else {
        panic!("expected a refusal");
      };
      assert_eq!(retry_after_secs, 1);
      assert!(
        limits
          .check_ip("1.2.3.4", start + Duration::from_millis(1100))
          .is_ok()
      );
      assert!(
        limits.check_ip("5.6.7.8", start).is_ok(),
        "other addresses are unaffected"
      );
    }

    #[test]
    fn disabled_limits_allow_everything() {
      let limits = limits(|c| {
        c.is_enabled = false;
        c.ip_requests_per_minute = 1;
      });
      for _ in 0..100 {
        assert!(limits.check_ip("x", Instant::now()).is_ok());
      }
      limits.record_auth_failure("x", Instant::now());
      assert!(limits.check_not_blocked("x", Instant::now()).is_ok());
    }
  }

  mod auth_failures {
    use super::*;

    #[test]
    fn an_address_that_keeps_failing_is_blocked_until_the_bucket_refills() {
      let limits = limits(|c| c.auth_failures_per_minute = 3);
      let start = Instant::now();
      for _ in 0..3 {
        assert!(limits.check_not_blocked("a", start).is_ok());
        limits.record_auth_failure("a", start);
      }
      assert!(limits.check_not_blocked("a", start).is_err());
      assert!(limits.check_not_blocked("b", start).is_ok());
      assert!(
        limits
          .check_not_blocked("a", start + Duration::from_secs(21))
          .is_ok()
      );
    }
  }

  mod addresses {
    use super::*;

    #[test]
    fn mapped_addresses_count_as_ipv4_and_ipv6_clients_per_64() {
      assert_eq!(normalize("::ffff:192.0.2.7".parse().unwrap()), "192.0.2.7");
      assert_eq!(normalize("192.0.2.7".parse().unwrap()), "192.0.2.7");
      assert_eq!(
        normalize("2001:db8:1:2:aaaa:bbbb:cccc:dddd".parse().unwrap()),
        normalize("2001:db8:1:2::1".parse().unwrap())
      );
      assert_ne!(
        normalize("2001:db8:1:2::1".parse().unwrap()),
        normalize("2001:db8:1:3::1".parse().unwrap())
      );
    }

    #[test]
    fn the_forwarded_header_counts_only_when_trusted_and_only_its_last_entry() {
      let request = |value: &str| {
        Request::builder()
          .header("x-forwarded-for", value)
          .body(axum::body::Body::empty())
          .unwrap()
      };
      assert_eq!(client_ip(&request("9.9.9.9, 192.0.2.1"), true), "192.0.2.1");
      assert_eq!(client_ip(&request("9.9.9.9, 192.0.2.1"), false), "unknown");
      assert_eq!(client_ip(&request("garbage"), true), "unknown");
    }
  }
}
