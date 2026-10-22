//! The CalDAV side: meetings are stored as events in a calendar collection of the calendar server
//! (CalCraft in the reference setup).

use std::time::Duration;

use reqwest::StatusCode;
use secrecy::ExposeSecret;
use time::{Duration as TimeDuration, OffsetDateTime};

use crate::auth::BoxFuture;
use crate::config::CalDavConfig;

/// Writes meeting events to a calendar.
pub trait CalendarStore: Send + Sync {
  /// Creates or replaces the event stored as `object_name` (without extension).
  fn put_event<'a>(&'a self, object_name: &'a str, ics: &'a str) -> BoxFuture<'a, Result<(), CalendarError>>;

  /// Removes the event; an event that is already gone is not an error.
  fn delete_event<'a>(&'a self, object_name: &'a str) -> BoxFuture<'a, Result<(), CalendarError>>;

  /// When the owner of `email` is busy between `from` and `to`, according to their own calendar.
  /// Calendars that cannot tell answer with no periods.
  fn busy_periods<'a>(
    &'a self,
    _email: &'a str,
    _from: OffsetDateTime,
    _to: OffsetDateTime,
  ) -> BoxFuture<'a, Result<Vec<BusyPeriod>, CalendarError>> {
    Box::pin(async { Ok(Vec::new()) })
  }
}

/// A time span in which somebody is busy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusyPeriod {
  pub start: OffsetDateTime,
  pub end: OffsetDateTime,
}

#[derive(Debug, thiserror::Error)]
pub enum CalendarError {
  #[error("the calendar server could not be reached: {0}")]
  Unreachable(String),
  #[error("the calendar server answered with HTTP status {0}")]
  Rejected(u16),
}

/// CalDAV over HTTP with basic authentication (an app password in the reference setup).
pub struct CalDavStore {
  client: reqwest::Client,
  calendar_url: String,
  username: String,
  password: secrecy::SecretString,
  maybe_free_busy_url_template: Option<String>,
}

impl CalDavStore {
  pub fn new(config: &CalDavConfig) -> Result<Self, reqwest::Error> {
    let client = reqwest::Client::builder()
      .timeout(Duration::from_secs(config.request_timeout_secs))
      .build()?;
    Ok(Self {
      client,
      calendar_url: config.calendar_url.trim_end_matches('/').to_owned(),
      username: config.username.clone(),
      password: config.password.clone(),
      maybe_free_busy_url_template: config.maybe_free_busy_url_template.clone(),
    })
  }

  fn object_url(&self, object_name: &str) -> String {
    format!("{}/{object_name}.ics", self.calendar_url)
  }
}

impl CalendarStore for CalDavStore {
  fn put_event<'a>(&'a self, object_name: &'a str, ics: &'a str) -> BoxFuture<'a, Result<(), CalendarError>> {
    Box::pin(async move {
      let response = self
        .client
        .put(self.object_url(object_name))
        .basic_auth(&self.username, Some(self.password.expose_secret()))
        .header(reqwest::header::CONTENT_TYPE, "text/calendar; charset=utf-8")
        .body(ics.to_owned())
        .send()
        .await
        .map_err(|error| CalendarError::Unreachable(error.to_string()))?;
      match response.status() {
        StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => Ok(()),
        other => Err(CalendarError::Rejected(other.as_u16())),
      }
    })
  }

  fn busy_periods<'a>(
    &'a self,
    email: &'a str,
    from: OffsetDateTime,
    to: OffsetDateTime,
  ) -> BoxFuture<'a, Result<Vec<BusyPeriod>, CalendarError>> {
    Box::pin(async move {
      let Some(template) = &self.maybe_free_busy_url_template else {
        return Ok(Vec::new());
      };
      let url = template.replace("{email}", &url_encode(email));
      let body = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<C:free-busy-query xmlns:C=\"urn:ietf:params:xml:ns:caldav\">\n  <C:time-range start=\"{}\" end=\"{}\"/>\n</C:free-busy-query>\n",
        ics_utc(from),
        ics_utc(to),
      );
      let response = self
        .client
        .request(
          reqwest::Method::from_bytes(b"REPORT")
            .map_err(|error| CalendarError::Unreachable(error.to_string()))?,
          url,
        )
        .basic_auth(&self.username, Some(self.password.expose_secret()))
        .header(reqwest::header::CONTENT_TYPE, "application/xml; charset=utf-8")
        .header("Depth", "1")
        .body(body)
        .send()
        .await
        .map_err(|error| CalendarError::Unreachable(error.to_string()))?;
      let status = response.status();
      // 200 is the normal answer; some servers use 207 for REPORT.
      if !(status == StatusCode::OK || status.as_u16() == 207) {
        return Err(CalendarError::Rejected(status.as_u16()));
      }
      let text = response
        .text()
        .await
        .map_err(|error| CalendarError::Unreachable(error.to_string()))?;
      Ok(parse_free_busy(&text))
    })
  }

  fn delete_event<'a>(&'a self, object_name: &'a str) -> BoxFuture<'a, Result<(), CalendarError>> {
    Box::pin(async move {
      let response = self
        .client
        .delete(self.object_url(object_name))
        .basic_auth(&self.username, Some(self.password.expose_secret()))
        .send()
        .await
        .map_err(|error| CalendarError::Unreachable(error.to_string()))?;
      match response.status() {
        StatusCode::OK | StatusCode::NO_CONTENT | StatusCode::NOT_FOUND => Ok(()),
        other => Err(CalendarError::Rejected(other.as_u16())),
      }
    })
  }
}

/// `20260101T090000Z`
fn ics_utc(time: OffsetDateTime) -> String {
  let time = time.to_offset(time::UtcOffset::UTC);
  format!(
    "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
    time.year(),
    u8::from(time.month()),
    time.day(),
    time.hour(),
    time.minute(),
    time.second()
  )
}

/// Percent-encodes everything but unreserved characters (an address goes into a URL path).
fn url_encode(text: &str) -> String {
  let mut out = String::new();
  for byte in text.bytes() {
    if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'@') {
      out.push(char::from(byte));
    } else {
      out.push_str(&format!("%{byte:02X}"));
    }
  }
  out
}

/// The periods in which a VFREEBUSY object says its owner is busy. `FREE` and `BUSY-TENTATIVE`
/// periods do not count; a period is `start/end` or `start/duration`. Anything unreadable is
/// skipped.
pub fn parse_free_busy(ics: &str) -> Vec<BusyPeriod> {
  // Content lines may be folded: a line starting with a space continues the previous one.
  let mut lines: Vec<String> = Vec::new();
  for raw in ics.lines() {
    match raw.strip_prefix([' ', '\t']) {
      Some(rest) => {
        if let Some(last) = lines.last_mut() {
          last.push_str(rest);
        }
      },
      None => lines.push(raw.to_owned()),
    }
  }
  let mut periods = Vec::new();
  for line in lines {
    let Some((name, value)) = line.split_once(':') else {
      continue;
    };
    let mut parts = name.split(';');
    if !parts
      .next()
      .is_some_and(|property| property.eq_ignore_ascii_case("FREEBUSY"))
    {
      continue;
    }
    let free_busy_type = parts
      .find_map(|parameter| parameter.strip_prefix("FBTYPE="))
      .unwrap_or("BUSY");
    if !(free_busy_type.eq_ignore_ascii_case("BUSY")
      || free_busy_type.eq_ignore_ascii_case("BUSY-UNAVAILABLE"))
    {
      continue;
    }
    for period in value.split(',') {
      if let Some(period) = parse_period(period.trim()) {
        periods.push(period);
      }
    }
  }
  periods.sort_by_key(|period| period.start);
  periods
}

fn parse_period(text: &str) -> Option<BusyPeriod> {
  let (start, end) = text.split_once('/')?;
  let start = parse_utc(start)?;
  let end = match parse_utc(end) {
    Some(end) => end,
    None => start + parse_duration(end)?,
  };
  (end > start).then_some(BusyPeriod { start, end })
}

/// `20260101T090000Z`
fn parse_utc(text: &str) -> Option<OffsetDateTime> {
  let bytes = text.as_bytes();
  if bytes.len() != 16 || bytes[8] != b'T' || bytes[15] != b'Z' {
    return None;
  }
  let number = |range: std::ops::Range<usize>| -> Option<i32> {
    let digits = text.get(range)?;
    if digits.bytes().all(|b| b.is_ascii_digit()) {
      digits.parse().ok()
    } else {
      None
    }
  };
  let date = time::Date::from_calendar_date(
    number(0..4)?,
    time::Month::try_from(u8::try_from(number(4..6)?).ok()?).ok()?,
    u8::try_from(number(6..8)?).ok()?,
  )
  .ok()?;
  let time = time::Time::from_hms(
    u8::try_from(number(9..11)?).ok()?,
    u8::try_from(number(11..13)?).ok()?,
    u8::try_from(number(13..15)?).ok()?,
  )
  .ok()?;
  Some(time::PrimitiveDateTime::new(date, time).assume_utc())
}

/// `PT1H30M`, `P1D`, `PT45M` (no weeks, months or years).
fn parse_duration(text: &str) -> Option<TimeDuration> {
  let rest = text.strip_prefix('P')?;
  let (days_part, time_part) = match rest.split_once('T') {
    Some((days, time)) => (days, time),
    None => (rest, ""),
  };
  let mut total = TimeDuration::ZERO;
  for (part, units) in [
    (days_part, [('D', 86_400i64)].as_slice()),
    (time_part, [('H', 3600), ('M', 60), ('S', 1)].as_slice()),
  ] {
    let mut number = String::new();
    for ch in part.chars() {
      if ch.is_ascii_digit() {
        number.push(ch);
      } else {
        let seconds = units.iter().find(|(unit, _)| *unit == ch)?.1;
        total += TimeDuration::seconds(number.parse::<i64>().ok()?.checked_mul(seconds)?);
        number.clear();
      }
    }
    if !number.is_empty() {
      return None;
    }
  }
  Some(total)
}

#[cfg(test)]
mod free_busy_tests {
  use time::macros::datetime;

  use super::*;

  #[test]
  fn busy_periods_are_read_from_a_vfreebusy_object() {
    let ics = "BEGIN:VCALENDAR\r\nBEGIN:VFREEBUSY\r\n\
      FREEBUSY;FBTYPE=BUSY:20260105T090000Z/20260105T100000Z,20260105T130000Z/PT1H30M\r\n\
      FREEBUSY;FBTYPE=BUSY-TENTATIVE:20260105T110000Z/20260105T120000Z\r\n\
      FREEBUSY;FBTYPE=FREE:20260105T140000Z/20260105T150000Z\r\n\
      FREEBUSY:20260106T080000Z/20260106T090000Z\r\n\
      END:VFREEBUSY\r\nEND:VCALENDAR\r\n";
    assert_eq!(
      parse_free_busy(ics),
      vec![
        BusyPeriod {
          start: datetime!(2026-01-05 09:00 UTC),
          end: datetime!(2026-01-05 10:00 UTC)
        },
        BusyPeriod {
          start: datetime!(2026-01-05 13:00 UTC),
          end: datetime!(2026-01-05 14:30 UTC)
        },
        BusyPeriod {
          start: datetime!(2026-01-06 08:00 UTC),
          end: datetime!(2026-01-06 09:00 UTC)
        },
      ]
    );
  }

  #[test]
  fn folded_lines_and_garbage_are_handled() {
    let ics = "FREEBUSY:20260105T090000Z/20260105T1000\r\n 00Z\r\nFREEBUSY:nonsense\r\nFREEBUSY:20260105T100000Z/20260105T090000Z\r\nFREEBUSY:20261301T090000Z/PT1H\r\nFREEBUSY:20260105T090000Z/P1X\r\n";
    let periods = parse_free_busy(ics);
    assert_eq!(periods.len(), 1);
    assert_eq!(periods[0].end, datetime!(2026-01-05 10:00 UTC));
    assert!(parse_free_busy("").is_empty());
  }

  #[tokio::test]
  async fn the_report_goes_to_the_users_calendar_with_a_time_range_and_the_answer_is_parsed() {
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::extract::{Path, Request, State};
    use axum::routing::any;

    type Seen = Arc<Mutex<Vec<(String, String, String, String)>>>;
    async fn handler(State(seen): State<Seen>, Path(email): Path<String>, request: Request) -> String {
      let method = request.method().to_string();
      let auth = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
      let body = axum::body::to_bytes(request.into_body(), 4096).await.unwrap();
      seen
        .lock()
        .unwrap()
        .push((method, email, auth, String::from_utf8_lossy(&body).into_owned()));
      "BEGIN:VCALENDAR\r\nBEGIN:VFREEBUSY\r\nFREEBUSY:20260105T090000Z/20260105T100000Z\r\nEND:VFREEBUSY\r\nEND:VCALENDAR\r\n".to_owned()
    }
    let seen: Seen = Arc::default();
    let app = Router::new()
      .route("/dav/calendars/{email}/personal/", any(handler))
      .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let store = CalDavStore::new(&CalDavConfig {
      calendar_url: format!("http://{addr}/dav/calendars/teamcraft/meetings/"),
      username: "teamcraft".into(),
      password: "app-password".to_owned().into(),
      request_timeout_secs: 2,
      maybe_free_busy_url_template: Some(format!("http://{addr}/dav/calendars/{{email}}/personal/")),
    })
    .unwrap();

    let periods = store
      .busy_periods(
        "ada@example.com",
        datetime!(2026-01-05 08:00 UTC),
        datetime!(2026-01-05 18:00 UTC),
      )
      .await
      .unwrap();
    assert_eq!(periods.len(), 1);
    assert_eq!(periods[0].end, datetime!(2026-01-05 10:00 UTC));
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].0, "REPORT");
    assert_eq!(seen[0].1, "ada@example.com");
    assert!(seen[0].2.starts_with("Basic "));
    assert!(
      seen[0].3.contains(r#"start="20260105T080000Z""#) && seen[0].3.contains(r#"end="20260105T180000Z""#)
    );
  }

  #[tokio::test]
  async fn calendars_without_a_template_report_nobody_busy() {
    let store = CalDavStore::new(&CalDavConfig {
      calendar_url: "http://127.0.0.1:1/".into(),
      username: "u".into(),
      password: "p".to_owned().into(),
      request_timeout_secs: 1,
      maybe_free_busy_url_template: None,
    })
    .unwrap();
    let periods = store
      .busy_periods(
        "ada@example.com",
        datetime!(2026-01-05 08:00 UTC),
        datetime!(2026-01-05 18:00 UTC),
      )
      .await
      .unwrap();
    assert!(periods.is_empty());
  }

  #[test]
  fn request_values_are_escaped_and_formatted() {
    assert_eq!(url_encode("a.b+c@example.com"), "a.b%2Bc@example.com");
    assert_eq!(ics_utc(datetime!(2026-01-05 09:03:07 +01:00)), "20260105T080307Z");
  }
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeMap;
  use std::sync::{Arc, Mutex};

  use axum::Router;
  use axum::body::Bytes;
  use axum::extract::{Path, State};
  use axum::http::{HeaderMap, StatusCode as HttpStatus};
  use axum::routing::put;

  use super::*;

  type Objects = Arc<Mutex<BTreeMap<String, (String, String)>>>;

  #[tokio::test]
  async fn events_are_put_and_deleted_with_basic_auth_and_calendar_content_type() {
    let objects = Objects::default();
    let store = store_for(serve(objects.clone(), false).await);

    store
      .put_event("42", "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n")
      .await
      .unwrap();

    let stored = objects.lock().unwrap().get("42.ics").cloned().unwrap();
    assert_eq!(stored.0, "text/calendar; charset=utf-8");
    // "teamcraft:app-password" as basic credentials.
    assert_eq!(stored.1, "Basic dGVhbWNyYWZ0OmFwcC1wYXNzd29yZA==");
    store.delete_event("42").await.unwrap();
    assert!(objects.lock().unwrap().is_empty());
    store.delete_event("42").await.unwrap();
  }

  #[tokio::test]
  async fn rejections_and_unreachable_servers_are_reported() {
    let rejecting = store_for(serve(Objects::default(), true).await);
    assert!(matches!(
      rejecting.put_event("1", "x").await,
      Err(CalendarError::Rejected(403))
    ));
    assert!(matches!(
      rejecting.delete_event("1").await,
      Err(CalendarError::Rejected(403))
    ));

    let closed = {
      let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
      listener.local_addr().unwrap()
    };
    assert!(matches!(
      store_for(closed).put_event("1", "x").await,
      Err(CalendarError::Unreachable(_))
    ));
  }

  fn store_for(addr: std::net::SocketAddr) -> CalDavStore {
    CalDavStore::new(&CalDavConfig {
      calendar_url: format!("http://{addr}/dav/calendars/teamcraft/meetings/"),
      username: "teamcraft".into(),
      password: "app-password".to_owned().into(),
      request_timeout_secs: 2,
      maybe_free_busy_url_template: None,
    })
    .unwrap()
  }

  async fn serve(objects: Objects, is_forbidding: bool) -> std::net::SocketAddr {
    async fn put_object(
      State((objects, is_forbidding)): State<(Objects, bool)>,
      Path(name): Path<String>,
      headers: HeaderMap,
      _body: Bytes,
    ) -> HttpStatus {
      if is_forbidding {
        return HttpStatus::FORBIDDEN;
      }
      let header = |key: &str| {
        headers
          .get(key)
          .and_then(|v| v.to_str().ok())
          .unwrap_or("")
          .to_owned()
      };
      objects
        .lock()
        .unwrap()
        .insert(name, (header("content-type"), header("authorization")));
      HttpStatus::CREATED
    }
    async fn delete_object(
      State((objects, is_forbidding)): State<(Objects, bool)>,
      Path(name): Path<String>,
    ) -> HttpStatus {
      if is_forbidding {
        return HttpStatus::FORBIDDEN;
      }
      match objects.lock().unwrap().remove(&name) {
        Some(_) => HttpStatus::NO_CONTENT,
        None => HttpStatus::NOT_FOUND,
      }
    }
    let app = Router::new()
      .route(
        "/dav/calendars/teamcraft/meetings/{name}",
        put(put_object).delete(delete_object),
      )
      .with_state((objects, is_forbidding));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    addr
  }
}
