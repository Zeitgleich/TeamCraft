//! The holiday feed (#1426): an iCalendar URL an admin sets in the console
//! (`holidays.feed_url`). A background task fetches it, every `holidays.feed_refresh_secs` and at
//! once after the settings change, and keeps the last good copy in memory and in
//! `server_settings` (so a restart while the feed is down still has it). [`FeedCopy`] answers
//! holiday questions from memory, never from the network: a feed that is down, slow or answers
//! with a captive portal's page never holds up listing meetings or ringing their invitees.
//!
//! The URL comes from an admin, but the request still must not reach into the server's own network:
//! https only, the address checked by [`crate::outbound`] for the URL, every redirect and every
//! connection. Each phase has its own time limit, the body a size limit, and what isn't a calendar
//! is refused before it can replace a good copy.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration as StdDuration;

use chrono::NaiveDate;
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use teamcraft_proto::{HolidayFeedError, HolidayFeedStatus, HolidaySettings};
use time::OffsetDateTime;
use tokio::sync::Notify;
use tracing::{info, warn};
use url::Url;

use super::HolidaySource;
use super::ics::{MAX_FEED_DATES, ParseError, parse_holidays};
use crate::outbound::{PublicOnlyResolver, check_public_url, check_public_url_text};
use crate::services::convert::unix_millis;
use crate::state::AppState;

/// The row of the last good copy in `server_settings` (not a settings group: no `settings.`
/// prefix).
pub const COPY_SETTING: &str = "holidays.feed_copy";
/// Longest feed URL an admin can set.
pub const MAX_FEED_URL_CHARS: usize = 2000;
/// Years before and after the fetch a copy keeps: series look back at their past and two years
/// ahead.
const YEARS_BACK: i32 = 5;
const YEARS_AHEAD: i32 = 10;
/// Most redirects followed (`http` to `https`, a moved file, a CDN).
const MAX_REDIRECTS: usize = 3;
/// The first retry after a failure; it doubles up to [`MAX_RETRY`] or the refresh interval.
const FIRST_RETRY: StdDuration = StdDuration::from_secs(60);
const MAX_RETRY: StdDuration = StdDuration::from_secs(6 * 3600);
/// Without a feed the task looks at the settings this often (and whenever they change).
const IDLE_CHECK: StdDuration = StdDuration::from_secs(3600);

/// The limits of one fetch, per phase.
#[derive(Debug, Clone, Copy)]
pub struct FetchLimits {
  /// Until the connection (TCP and TLS) stands.
  pub connect: StdDuration,
  /// Until the status and the headers are in.
  pub answer: StdDuration,
  /// Between two pieces of the body.
  pub read: StdDuration,
  /// The whole request, redirects included.
  pub total: StdDuration,
  pub max_bytes: usize,
}

impl FetchLimits {
  pub const DEFAULT: Self = Self {
    connect: StdDuration::from_secs(10),
    answer: StdDuration::from_secs(20),
    read: StdDuration::from_secs(20),
    total: StdDuration::from_secs(60),
    max_bytes: 2 * 1024 * 1024,
  };
}

/// The last good copy of the feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedCopy {
  /// The URL it was fetched from: a copy of another URL than the settings name is not used.
  pub url: String,
  pub fetched_at: OffsetDateTime,
  pub dates: BTreeSet<NaiveDate>,
}

impl HolidaySource for FeedCopy {
  fn holidays_between(&self, from: NaiveDate, to: NaiveDate) -> BTreeSet<NaiveDate> {
    if from > to {
      return BTreeSet::new();
    }
    self.dates.range(from..=to).copied().collect()
  }
}

/// Why a fetch failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FeedError {
  #[error("the feed is unreachable")]
  Unreachable,
  #[error("the feed took too long")]
  Timeout,
  /// An error status, or a redirect that may not be followed.
  #[error("the feed answered with HTTP status {0}")]
  HttpStatus(u16),
  #[error("the feed is larger than the server reads")]
  TooLarge,
  #[error(transparent)]
  Parse(#[from] ParseError),
}

/// The feed's copy and how fetching it goes.
#[derive(Default)]
pub struct HolidayFeed {
  inner: Mutex<Inner>,
  wake: Notify,
}

#[derive(Default)]
struct Inner {
  maybe_copy: Option<Arc<FeedCopy>>,
  /// The URL of the last attempt and when it was.
  maybe_attempt: Option<(String, OffsetDateTime)>,
  maybe_error: Option<FeedError>,
  /// Failures in a row.
  failures: u32,
  /// When the next attempt is due, set by the last one.
  maybe_next_at: Option<OffsetDateTime>,
  /// The settings changed: fetch at once.
  is_due: bool,
}

/// The copy as `server_settings` keeps it.
#[derive(Serialize, Deserialize)]
struct StoredCopy {
  url: String,
  fetched_at_ms: i64,
  /// `YYYY-MM-DD`.
  dates: Vec<String>,
}

/// The health check's view of the feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedHealth {
  NotConfigured,
  Configured {
    /// When the copy in use was fetched.
    maybe_fetched_at: Option<OffsetDateTime>,
    /// The last attempt failed.
    has_failed: bool,
    max_age: time::Duration,
  },
}

impl HolidayFeed {
  /// The copy of `url`, if there is one.
  pub fn copy_for(&self, url: &str) -> Option<Arc<FeedCopy>> {
    self.lock().maybe_copy.clone().filter(|copy| copy.url == url)
  }

  /// How the feed of `url` stands, for the console.
  pub fn status(&self, url: &str) -> HolidayFeedStatus {
    let inner = self.lock();
    let maybe_copy = inner.maybe_copy.as_ref().filter(|copy| copy.url == url);
    let maybe_attempt = inner
      .maybe_attempt
      .as_ref()
      .filter(|(attempted, _)| attempted == url);
    let maybe_error = inner.maybe_error.filter(|_| maybe_attempt.is_some());
    HolidayFeedStatus {
      maybe_fetched_at: maybe_copy.map(|copy| unix_millis(copy.fetched_at)),
      holidays: maybe_copy.map_or(0, |copy| u32::try_from(copy.dates.len()).unwrap_or(u32::MAX)),
      maybe_last_attempt_at: maybe_attempt.map(|(_, at)| unix_millis(*at)),
      maybe_error: maybe_error.map(|error| error.wire()),
      maybe_http_status: match maybe_error {
        Some(FeedError::HttpStatus(status)) => Some(status),
        _ => None,
      },
    }
  }

  /// What the health check judges.
  pub fn health(&self, settings: &HolidaySettings) -> FeedHealth {
    let Some(url) = settings.maybe_feed_url.as_deref() else {
      return FeedHealth::NotConfigured;
    };
    let inner = self.lock();
    FeedHealth::Configured {
      maybe_fetched_at: inner
        .maybe_copy
        .as_ref()
        .filter(|copy| copy.url == url)
        .map(|copy| copy.fetched_at),
      has_failed: inner.maybe_error.is_some()
        && inner
          .maybe_attempt
          .as_ref()
          .is_some_and(|(attempted, _)| attempted == url),
      max_age: time::Duration::seconds(i64::try_from(settings.feed_max_age_secs).unwrap_or(i64::MAX)),
    }
  }

  /// The holiday settings changed: fetch at once, without the wait of earlier failures.
  pub fn settings_changed(&self) {
    {
      let mut inner = self.lock();
      inner.is_due = true;
      inner.failures = 0;
    }
    self.wake.notify_one();
  }

  /// How long until the feed of `url` is fetched next.
  fn due_in(&self, url: &str, refresh: StdDuration, now: OffsetDateTime) -> StdDuration {
    let inner = self.lock();
    if inner.is_due {
      return StdDuration::ZERO;
    }
    let due_at = match (&inner.maybe_attempt, inner.maybe_next_at) {
      (Some((attempted, _)), Some(next_at)) if attempted == url => next_at,
      // Nothing tried for this URL since the start: a copy from before the restart waits for
      // its time, none is fetched now.
      _ => match inner.maybe_copy.as_ref().filter(|copy| copy.url == url) {
        Some(copy) => copy.fetched_at + refresh,
        None => return StdDuration::ZERO,
      },
    };
    StdDuration::try_from(due_at - now).unwrap_or(StdDuration::ZERO)
  }

  fn succeeded(&self, copy: Arc<FeedCopy>, refresh: StdDuration) {
    let now = copy.fetched_at;
    let mut inner = self.lock();
    inner.maybe_attempt = Some((copy.url.clone(), now));
    inner.maybe_copy = Some(copy);
    inner.maybe_error = None;
    inner.failures = 0;
    inner.is_due = false;
    inner.maybe_next_at = Some(now + spread(refresh, OsRng.next_u32()));
  }

  fn failed(&self, url: &str, error: FeedError, refresh: StdDuration, now: OffsetDateTime) -> u32 {
    let mut inner = self.lock();
    inner.maybe_attempt = Some((url.to_owned(), now));
    inner.maybe_error = Some(error);
    inner.failures = inner.failures.saturating_add(1);
    inner.is_due = false;
    inner.maybe_next_at = Some(now + backoff(inner.failures, refresh, OsRng.next_u32()));
    inner.failures
  }

  fn lock(&self) -> MutexGuard<'_, Inner> {
    self.inner.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

impl FeedError {
  fn wire(self) -> HolidayFeedError {
    match self {
      Self::Unreachable => HolidayFeedError::Unreachable,
      Self::Timeout => HolidayFeedError::Timeout,
      Self::HttpStatus(_) => HolidayFeedError::HttpStatus,
      Self::TooLarge => HolidayFeedError::TooLarge,
      Self::Parse(ParseError::NotACalendar) => HolidayFeedError::NotACalendar,
      Self::Parse(ParseError::Malformed) => HolidayFeedError::Malformed,
      Self::Parse(ParseError::Empty) => HolidayFeedError::Empty,
      Self::Parse(ParseError::TooMany) => HolidayFeedError::TooMany,
    }
  }
}

/// Reads the last good copy from the database; at the start, before the task runs. A copy that
/// can't be read is ignored (the next fetch replaces it).
pub async fn load(state: &AppState) {
  match state.database.server_setting(COPY_SETTING).await {
    Ok(Some(row)) => match parse_stored(&row.value) {
      Some(copy) => state.holiday_feed.lock().maybe_copy = Some(Arc::new(copy)),
      None => warn!("ignoring a stored holiday feed copy this version cannot read"),
    },
    Ok(None) => {},
    Err(error) => warn!(%error, "reading the stored holiday feed copy failed"),
  }
}

/// Fetches the feed in the background until the process ends.
pub fn spawn(state: AppState) {
  tokio::spawn(async move {
    let client = match build_client(&FetchLimits::DEFAULT, false) {
      Ok(client) => client,
      Err(error) => {
        warn!(%error, "the holiday feed client can't be built: the feed is not fetched");
        return;
      },
    };
    loop {
      let settings = state.settings.get().holidays.clone();
      let refresh = StdDuration::from_secs(settings.feed_refresh_secs.max(1));
      let wait = match settings.maybe_feed_url.as_deref() {
        None => IDLE_CHECK,
        Some(url) => {
          let wait = state.holiday_feed.due_in(url, refresh, OffsetDateTime::now_utc());
          if wait.is_zero() {
            refresh_feed(&state, &client, url, refresh).await;
            continue;
          }
          wait
        },
      };
      tokio::select! {
        () = tokio::time::sleep(wait) => {},
        () = state.holiday_feed.wake.notified() => {},
      }
    }
  });
}

/// One attempt: fetch, read, and keep the copy if it is good.
async fn refresh_feed(state: &AppState, client: &reqwest::Client, url: &str, refresh: StdDuration) {
  let now = OffsetDateTime::now_utc();
  let host = Url::parse(url)
    .ok()
    .and_then(|parsed| parsed.host_str().map(str::to_owned))
    .unwrap_or_default();
  let result = match check_public_url_text(url) {
    Ok(parsed) => fetch_holidays(client, &parsed, &FetchLimits::DEFAULT, now).await,
    Err(_) => Err(FeedError::Unreachable),
  };
  match result {
    Ok(dates) => {
      let copy = Arc::new(FeedCopy {
        url: url.to_owned(),
        fetched_at: now,
        dates,
      });
      // The host only: the path of a private calendar's feed is its password.
      info!(%host, holidays = copy.dates.len(), "holiday feed fetched");
      store(state, &copy).await;
      state.holiday_feed.succeeded(copy, refresh);
    },
    Err(error) => {
      let failures = state.holiday_feed.failed(url, error, refresh, now);
      warn!(%host, %error, failures, "fetching the holiday feed failed: the last good copy stays");
    },
  }
}

/// Fetches and reads the feed: the holidays from [`YEARS_BACK`] years before `now` to
/// [`YEARS_AHEAD`] after.
pub async fn fetch_holidays(
  client: &reqwest::Client,
  url: &Url,
  limits: &FetchLimits,
  now: OffsetDateTime,
) -> Result<BTreeSet<NaiveDate>, FeedError> {
  let text = fetch(client, url, limits).await?;
  let year = now.year();
  let window = NaiveDate::from_ymd_opt(year - YEARS_BACK, 1, 1).unwrap_or(NaiveDate::MIN)
    ..=NaiveDate::from_ymd_opt(year + YEARS_AHEAD, 12, 31).unwrap_or(NaiveDate::MAX);
  Ok(parse_holidays(&text, &window)?)
}

/// The body of the feed as text, within the limits; an HTML page is refused by its type.
pub async fn fetch(client: &reqwest::Client, url: &Url, limits: &FetchLimits) -> Result<String, FeedError> {
  let request = client.get(url.clone()).header(
    reqwest::header::ACCEPT,
    "text/calendar, text/plain;q=0.5, */*;q=0.1",
  );
  let mut response = tokio::time::timeout(limits.answer, request.send())
    .await
    .map_err(|_| FeedError::Timeout)?
    .map_err(|error| error_of(&error))?;
  let status = response.status();
  if !status.is_success() {
    return Err(FeedError::HttpStatus(status.as_u16()));
  }
  let is_html = response
    .headers()
    .get(reqwest::header::CONTENT_TYPE)
    .and_then(|value| value.to_str().ok())
    .is_some_and(|value| value.to_ascii_lowercase().contains("html"));
  if is_html {
    return Err(ParseError::NotACalendar.into());
  }
  let max_bytes = u64::try_from(limits.max_bytes).unwrap_or(u64::MAX);
  if response.content_length().is_some_and(|length| length > max_bytes) {
    return Err(FeedError::TooLarge);
  }
  let mut body = Vec::new();
  while let Some(chunk) = response.chunk().await.map_err(|error| error_of(&error))? {
    if body.len().saturating_add(chunk.len()) > limits.max_bytes {
      return Err(FeedError::TooLarge);
    }
    body.extend_from_slice(&chunk);
  }
  Ok(String::from_utf8_lossy(&body).into_owned())
}

/// The client of the feed: phase timeouts, no proxy (it would resolve names past the check), and,
/// unless `allow_private` (tests against a local server), names resolved to public addresses
/// only and redirects only to URLs that pass the check.
pub fn build_client(limits: &FetchLimits, allow_private: bool) -> Result<reqwest::Client, reqwest::Error> {
  let policy = reqwest::redirect::Policy::custom(move |attempt| {
    if may_follow(attempt.url(), attempt.previous().len(), allow_private) {
      attempt.follow()
    } else {
      attempt.stop()
    }
  });
  let builder = reqwest::Client::builder()
    .connect_timeout(limits.connect)
    .read_timeout(limits.read)
    .timeout(limits.total)
    .no_proxy()
    .redirect(policy)
    .user_agent(concat!(
      "TeamCraft/",
      env!("CARGO_PKG_VERSION"),
      " (holiday feed)"
    ));
  if allow_private {
    builder.build()
  } else {
    builder.dns_resolver(Arc::new(PublicOnlyResolver)).build()
  }
}

/// Whether a redirect to `url`, after `previous` requests, is followed.
fn may_follow(url: &Url, previous: usize, allow_private: bool) -> bool {
  previous <= MAX_REDIRECTS && (allow_private || check_public_url(url).is_ok())
}

fn error_of(error: &reqwest::Error) -> FeedError {
  if error.is_timeout() {
    FeedError::Timeout
  } else {
    FeedError::Unreachable
  }
}

/// `base` and up to a tenth more, so instances started together don't all fetch at once.
fn spread(base: StdDuration, random: u32) -> StdDuration {
  base + base.mul_f64(f64::from(random) / f64::from(u32::MAX) / 10.0)
}

/// The wait after `failures` failures in a row: doubling from [`FIRST_RETRY`], at most the
/// refresh interval and [`MAX_RETRY`], a quarter more or less at random.
fn backoff(failures: u32, refresh: StdDuration, random: u32) -> StdDuration {
  let doublings = failures.saturating_sub(1).min(16);
  let base = FIRST_RETRY
    .saturating_mul(1 << doublings)
    .min(refresh)
    .min(MAX_RETRY);
  let factor = 0.75 + 0.5 * f64::from(random) / f64::from(u32::MAX);
  base.mul_f64(factor)
}

async fn store(state: &AppState, copy: &FeedCopy) {
  let stored = StoredCopy {
    url: copy.url.clone(),
    fetched_at_ms: unix_millis(copy.fetched_at).0,
    dates: copy.dates.iter().map(ToString::to_string).collect(),
  };
  let Ok(value) = serde_json::to_string(&stored) else {
    return;
  };
  if let Err(error) = state
    .database
    .put_system_server_setting(COPY_SETTING, &value)
    .await
  {
    warn!(%error, "storing the holiday feed copy failed: it is kept in memory only");
  }
}

/// A stored copy, if it reads and keeps the bounds.
fn parse_stored(value: &str) -> Option<FeedCopy> {
  let stored: StoredCopy = serde_json::from_str(value).ok()?;
  if stored.dates.len() > MAX_FEED_DATES || stored.url.chars().count() > MAX_FEED_URL_CHARS {
    return None;
  }
  let dates = stored
    .dates
    .iter()
    .map(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").ok())
    .collect::<Option<BTreeSet<_>>>()?;
  let fetched_at =
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(stored.fetched_at_ms) * 1_000_000).ok()?;
  Some(FeedCopy {
    url: stored.url,
    fetched_at,
    dates,
  })
}

#[cfg(test)]
mod tests {
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  use tokio::net::TcpListener;

  use super::*;

  const CALENDAR: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTART;VALUE=DATE:20261003\r\nRRULE:FREQ=YEARLY\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

  fn limits() -> FetchLimits {
    FetchLimits {
      connect: StdDuration::from_secs(2),
      answer: StdDuration::from_millis(500),
      read: StdDuration::from_millis(500),
      total: StdDuration::from_secs(3),
      max_bytes: 4096,
    }
  }

  /// A server that answers every request with `answer` (raw HTTP), after `delay`.
  async fn serve(answer: String, delay: StdDuration) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
      while let Ok((mut socket, _)) = listener.accept().await {
        let answer = answer.clone();
        tokio::spawn(async move {
          let mut request = [0_u8; 2048];
          let _ = socket.read(&mut request).await;
          tokio::time::sleep(delay).await;
          let _ = socket.write_all(answer.as_bytes()).await;
          let _ = socket.shutdown().await;
        });
      }
    });
    format!("http://127.0.0.1:{port}/feed.ics").parse().unwrap()
  }

  fn ok(content_type: &str, body: &str) -> String {
    format!(
      "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
      body.len()
    )
  }

  async fn fetched(answer: String) -> Result<BTreeSet<NaiveDate>, FeedError> {
    let url = serve(answer, StdDuration::ZERO).await;
    let client = build_client(&limits(), true).unwrap();
    let now = OffsetDateTime::from_unix_timestamp(1_790_000_000).unwrap();
    fetch_holidays(&client, &url, &limits(), now).await
  }

  #[tokio::test]
  async fn a_calendar_is_read_into_the_years_kept() {
    let dates = fetched(ok("text/calendar; charset=utf-8", CALENDAR))
      .await
      .unwrap();
    // Fetched in 2026: 2026 to 2036.
    assert_eq!(dates.len(), 11);
    assert!(dates.contains(&NaiveDate::from_ymd_opt(2036, 10, 3).unwrap()));
  }

  #[tokio::test]
  async fn a_captive_portal_page_is_refused() {
    let page = "<html><body>Sign in to the guest network</body></html>";
    assert_eq!(
      fetched(ok("text/html", page)).await,
      Err(FeedError::Parse(ParseError::NotACalendar))
    );
    // Also when it claims to be something else.
    assert_eq!(
      fetched(ok("application/octet-stream", page)).await,
      Err(FeedError::Parse(ParseError::NotACalendar))
    );
  }

  #[tokio::test]
  async fn error_statuses_are_errors() {
    let answer = "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    assert_eq!(fetched(answer.into()).await, Err(FeedError::HttpStatus(503)));
  }

  #[tokio::test]
  async fn a_feed_larger_than_the_limit_is_refused_by_length_and_while_reading() {
    let big = "X".repeat(5000);
    assert_eq!(fetched(ok("text/calendar", &big)).await, Err(FeedError::TooLarge));
    // Without a length: counted while reading.
    let answer = format!("HTTP/1.1 200 OK\r\nContent-Type: text/calendar\r\nConnection: close\r\n\r\n{big}");
    assert_eq!(fetched(answer).await, Err(FeedError::TooLarge));
  }

  #[tokio::test]
  async fn a_server_that_does_not_answer_times_out() {
    let url = serve(ok("text/calendar", CALENDAR), StdDuration::from_secs(5)).await;
    let client = build_client(&limits(), true).unwrap();
    assert_eq!(fetch(&client, &url, &limits()).await, Err(FeedError::Timeout));
  }

  #[tokio::test]
  async fn the_guarded_client_never_connects_to_this_machine() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let client = build_client(&limits(), false).unwrap();
    // `localhost` passes no textual check, but even asked directly the resolver refuses it.
    let url: Url = format!("https://localhost:{port}/feed.ics").parse().unwrap();
    assert_eq!(fetch(&client, &url, &limits()).await, Err(FeedError::Unreachable));
    let accepted = tokio::time::timeout(StdDuration::from_millis(300), listener.accept()).await;
    assert!(
      accepted.is_err(),
      "the feed client connected to a private address"
    );
  }

  #[test]
  fn redirects_are_followed_only_to_public_https_and_only_a_few() {
    let public: Url = "https://cdn.example.com/feed.ics".parse().unwrap();
    assert!(may_follow(&public, 1, false));
    assert!(!may_follow(&public, MAX_REDIRECTS + 1, false));
    for refused in [
      "http://cdn.example.com/feed.ics",
      "https://127.0.0.1/feed.ics",
      "https://169.254.169.254/latest/meta-data",
      "https://localhost/feed.ics",
      "https://[::1]/feed.ics",
    ] {
      let url: Url = refused.parse().unwrap();
      assert!(!may_follow(&url, 1, false), "{refused}");
    }
  }

  #[test]
  fn backoff_doubles_up_to_the_refresh_interval_with_jitter() {
    let day = StdDuration::from_secs(86_400);
    let middle = u32::MAX / 2;
    let first = backoff(1, day, middle);
    assert!((59..=61).contains(&first.as_secs()), "{first:?}");
    assert!((119..=121).contains(&backoff(2, day, middle).as_secs()));
    let capped = backoff(40, day, middle).as_secs();
    assert!(
      (MAX_RETRY.as_secs() - 1..=MAX_RETRY.as_secs()).contains(&capped),
      "{capped}"
    );
    let hourly = StdDuration::from_secs(3600);
    assert!((3599..=3600).contains(&backoff(40, hourly, middle).as_secs()));
    // A quarter either way.
    assert_eq!(backoff(1, day, 0).as_secs(), 45);
    assert_eq!(backoff(1, day, u32::MAX).as_secs(), 75);
    assert!(spread(day, u32::MAX) <= day + day / 10 && spread(day, 0) == day);
  }

  #[test]
  fn the_copy_answers_its_range_and_only_for_its_url() {
    let date = |day| NaiveDate::from_ymd_opt(2026, 10, day).unwrap();
    let copy = FeedCopy {
      url: "https://a.example.com/x.ics".into(),
      fetched_at: OffsetDateTime::UNIX_EPOCH,
      dates: [date(1), date(3), date(5)].into(),
    };
    assert_eq!(copy.holidays_between(date(2), date(5)), [date(3), date(5)].into());
    assert!(copy.holidays_between(date(5), date(1)).is_empty());
    let feed = HolidayFeed::default();
    feed.lock().maybe_copy = Some(Arc::new(copy));
    assert!(feed.copy_for("https://a.example.com/x.ics").is_some());
    assert!(feed.copy_for("https://b.example.com/x.ics").is_none());
  }

  #[test]
  fn failures_keep_the_copy_and_a_settings_change_fetches_at_once() {
    let feed = HolidayFeed::default();
    let url = "https://a.example.com/x.ics";
    let now = OffsetDateTime::from_unix_timestamp(1_790_000_000).unwrap();
    let refresh = StdDuration::from_secs(86_400);
    assert!(feed.due_in(url, refresh, now).is_zero(), "nothing fetched yet");
    feed.succeeded(
      Arc::new(FeedCopy {
        url: url.into(),
        fetched_at: now,
        dates: BTreeSet::new(),
      }),
      refresh,
    );
    assert!(feed.due_in(url, refresh, now) >= refresh);
    feed.failed(url, FeedError::Timeout, refresh, now);
    assert!(feed.copy_for(url).is_some(), "the good copy stays");
    assert!(feed.due_in(url, refresh, now) <= StdDuration::from_secs(75));
    let status = feed.status(url);
    assert_eq!(status.maybe_error, Some(HolidayFeedError::Timeout));
    assert!(status.maybe_fetched_at.is_some());
    feed.settings_changed();
    assert!(feed.due_in(url, refresh, now).is_zero());
    // Another URL has no copy and is fetched at once.
    let other = "https://b.example.com/x.ics";
    assert!(feed.copy_for(other).is_none());
    assert_eq!(feed.status(other).maybe_error, None);
  }

  #[test]
  fn a_stored_copy_round_trips_and_a_broken_one_is_ignored() {
    let copy = FeedCopy {
      url: "https://a.example.com/x.ics".into(),
      fetched_at: OffsetDateTime::from_unix_timestamp(1_790_000_000).unwrap(),
      dates: [NaiveDate::from_ymd_opt(2026, 10, 3).unwrap()].into(),
    };
    let stored = StoredCopy {
      url: copy.url.clone(),
      fetched_at_ms: unix_millis(copy.fetched_at).0,
      dates: vec!["2026-10-03".into()],
    };
    let text = serde_json::to_string(&stored).unwrap();
    assert_eq!(parse_stored(&text), Some(copy));
    assert_eq!(parse_stored("{\"url\":1}"), None);
    let bad_date = text.replace("2026-10-03", "2026-13-03");
    assert_eq!(parse_stored(&bad_date), None);
    let too_many = serde_json::to_string(&StoredCopy {
      url: "https://a.example.com".into(),
      fetched_at_ms: 0,
      dates: vec!["2026-10-03".into(); MAX_FEED_DATES + 1],
    })
    .unwrap();
    assert_eq!(parse_stored(&too_many), None);
  }
}
