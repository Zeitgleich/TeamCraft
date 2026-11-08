//! Reading holidays out of an iCalendar feed (RFC 5545): the dates of its all-day `VEVENT`s
//! (`DTSTART;VALUE=DATE`, over several days with `DTEND` or `DURATION`), repeated by a yearly
//! `RRULE` where there is one, without the dates of `EXDATE`. Timed and cancelled events are no
//! holidays and are left out.
//!
//! The feed comes from the network, so the parser trusts nothing: a feed that is cut off, nests
//! its components wrongly or has no all-day event at all is refused as a whole, so that the last
//! good copy stays in use rather than being replaced by part of a calendar.

use std::collections::BTreeSet;
use std::ops::RangeInclusive;

use chrono::{Datelike, Duration, NaiveDate, Weekday};

/// Most holidays one copy keeps.
pub const MAX_FEED_DATES: usize = 5000;
/// An all-day event longer than this is no holiday (a school term, a project phase).
const MAX_EVENT_DAYS: i64 = 31;
/// Most components open inside each other (`VCALENDAR` > `VEVENT` > `VALARM` is three).
const MAX_DEPTH: usize = 8;

/// Why a feed was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
  /// Not an iCalendar object at all (an HTML page, JSON, ...).
  #[error("not an iCalendar feed")]
  NotACalendar,
  /// Cut off, or components that don't close in order.
  #[error("a broken iCalendar feed")]
  Malformed,
  /// No all-day event in the years kept.
  #[error("no all-day events")]
  Empty,
  #[error("more holidays than kept")]
  TooMany,
}

/// The holidays of a feed within `window` (both ends included).
pub fn parse_holidays(
  text: &str,
  window: &RangeInclusive<NaiveDate>,
) -> Result<BTreeSet<NaiveDate>, ParseError> {
  let text = text.strip_prefix('\u{feff}').unwrap_or(text);
  let starts_as_calendar = text
    .trim_start()
    .get(..15)
    .is_some_and(|start| start.eq_ignore_ascii_case("BEGIN:VCALENDAR"));
  if !starts_as_calendar {
    return Err(ParseError::NotACalendar);
  }
  let mut parser = Parser {
    window,
    stack: Vec::new(),
    maybe_event: None,
    is_closed: false,
    dates: BTreeSet::new(),
  };
  for line in unfolded(text) {
    parser.line(&line)?;
    if parser.dates.len() > MAX_FEED_DATES {
      return Err(ParseError::TooMany);
    }
  }
  if !parser.is_closed {
    return Err(ParseError::Malformed);
  }
  if parser.dates.is_empty() {
    return Err(ParseError::Empty);
  }
  Ok(parser.dates)
}

struct Parser<'a> {
  window: &'a RangeInclusive<NaiveDate>,
  /// The components open, outermost first, upper case.
  stack: Vec<String>,
  /// The `VEVENT` being read.
  maybe_event: Option<Event>,
  /// `END:VCALENDAR` was read.
  is_closed: bool,
  dates: BTreeSet<NaiveDate>,
}

/// What a `VEVENT` says about its dates.
#[derive(Default)]
struct Event {
  maybe_start: Option<Start>,
  maybe_end: Option<NaiveDate>,
  maybe_days: Option<i64>,
  maybe_rule: Option<String>,
  excluded: BTreeSet<NaiveDate>,
  is_cancelled: bool,
}

enum Start {
  Date(NaiveDate),
  /// A date with a time: an appointment, not a holiday.
  Timed,
}

/// A yearly rule as far as holiday feeds use it.
struct Yearly {
  interval: i32,
  maybe_count: Option<u32>,
  maybe_until: Option<NaiveDate>,
  /// The day in each year; `None`: the start's month and day.
  maybe_day: Option<YearDay>,
}

enum YearDay {
  /// `BYMONTH` and `BYMONTHDAY`.
  Fixed { month: u32, day: u32 },
  /// `BYMONTH` and `BYDAY` with an ordinal: the `n`th (negative: from the end) weekday.
  Weekday { month: u32, weekday: Weekday, n: i8 },
}

impl Parser<'_> {
  fn line(&mut self, line: &str) -> Result<(), ParseError> {
    if line.trim().is_empty() {
      return Ok(());
    }
    if self.is_closed {
      // Something after the calendar ended: a second object or garbage. Ignored.
      return Ok(());
    }
    let (name, params, value) = content_line(line).ok_or(ParseError::Malformed)?;
    match name.as_str() {
      "BEGIN" => self.begin(value),
      "END" => self.end(value),
      _ => {
        if self.stack.len() == 2
          && let Some(event) = self.maybe_event.as_mut()
        {
          event.property(&name, params, value);
        }
        Ok(())
      },
    }
  }

  fn begin(&mut self, value: &str) -> Result<(), ParseError> {
    let component = value.trim().to_ascii_uppercase();
    if self.stack.is_empty() != (component == "VCALENDAR") || self.stack.len() >= MAX_DEPTH {
      return Err(ParseError::Malformed);
    }
    if self.stack.len() == 1 && component == "VEVENT" {
      self.maybe_event = Some(Event::default());
    }
    self.stack.push(component);
    Ok(())
  }

  fn end(&mut self, value: &str) -> Result<(), ParseError> {
    let component = value.trim().to_ascii_uppercase();
    if self.stack.pop().as_deref() != Some(component.as_str()) {
      return Err(ParseError::Malformed);
    }
    match self.stack.len() {
      0 => self.is_closed = true,
      1 if component == "VEVENT" => {
        if let Some(event) = self.maybe_event.take() {
          event.dates(self.window, &mut self.dates);
        }
      },
      _ => {},
    }
    Ok(())
  }
}

impl Event {
  fn property(&mut self, name: &str, params: &str, value: &str) {
    match name {
      "DTSTART" => self.maybe_start = Some(start_of(params, value)),
      "DTEND" => self.maybe_end = date_of(value.trim()),
      "DURATION" => self.maybe_days = days_of(value.trim()),
      "RRULE" => self.maybe_rule = Some(value.trim().to_ascii_uppercase()),
      "EXDATE" => self
        .excluded
        .extend(value.split(',').filter_map(|part| date_prefix(part.trim()))),
      "STATUS" => self.is_cancelled = value.trim().eq_ignore_ascii_case("CANCELLED"),
      _ => {},
    }
  }

  /// Adds the event's days within `window` to `into`.
  fn dates(self, window: &RangeInclusive<NaiveDate>, into: &mut BTreeSet<NaiveDate>) {
    let Some(Start::Date(start)) = self.maybe_start else {
      return;
    };
    if self.is_cancelled {
      return;
    }
    let days = match (self.maybe_end, self.maybe_days) {
      (Some(end), _) => (end - start).num_days(),
      (None, Some(days)) => days,
      (None, None) => 1,
    }
    // An end on or before the start: the start alone (RFC 5545 §3.6.1).
    .max(1);
    if days > MAX_EVENT_DAYS {
      return;
    }
    let starts: Vec<NaiveDate> = match self.maybe_rule.as_deref().and_then(yearly) {
      Some(rule) => rule.starts(start, window),
      None => vec![start],
    };
    for first in starts.into_iter().filter(|first| !self.excluded.contains(first)) {
      let days_of_event = (0..days).filter_map(|offset| first.checked_add_signed(Duration::days(offset)));
      for day in days_of_event.filter(|day| window.contains(day)) {
        into.insert(day);
        if into.len() > MAX_FEED_DATES {
          return;
        }
      }
    }
  }
}

impl Yearly {
  /// The starts of the series up to the end of `window`; the first is `start`.
  fn starts(&self, start: NaiveDate, window: &RangeInclusive<NaiveDate>) -> Vec<NaiveDate> {
    let last = self
      .maybe_until
      .map_or(*window.end(), |until| until.min(*window.end()));
    let mut starts = vec![start];
    let mut year = start.year();
    let mut count = 1;
    loop {
      year = match year.checked_add(self.interval) {
        Some(next) => next,
        None => break,
      };
      if year > last.year() || self.maybe_count.is_some_and(|max| count >= max) {
        break;
      }
      // A day the year doesn't have (29 February) is no occurrence and doesn't count.
      let Some(date) = self.day_in(year, start) else {
        continue;
      };
      if date > last {
        break;
      }
      count += 1;
      if date >= *window.start() {
        starts.push(date);
      }
    }
    starts
  }

  fn day_in(&self, year: i32, start: NaiveDate) -> Option<NaiveDate> {
    match self.maybe_day {
      None => NaiveDate::from_ymd_opt(year, start.month(), start.day()),
      Some(YearDay::Fixed { month, day }) => NaiveDate::from_ymd_opt(year, month, day),
      Some(YearDay::Weekday { month, weekday, n }) => nth_weekday_of_month(year, month, weekday, n),
    }
  }
}

/// A yearly `RRULE` this parser can follow; `None` for another frequency or parts it does not
/// know, so that only the event's own date counts (never a wrong series).
fn yearly(rule: &str) -> Option<Yearly> {
  let mut rule_parts = Yearly {
    interval: 1,
    maybe_count: None,
    maybe_until: None,
    maybe_day: None,
  };
  let (mut is_yearly, mut maybe_month, mut maybe_month_day, mut maybe_by_day) = (false, None, None, None);
  for part in rule.split(';').filter(|part| !part.is_empty()) {
    let (key, value) = part.split_once('=')?;
    match key {
      "FREQ" => is_yearly = value == "YEARLY",
      "INTERVAL" => {
        rule_parts.interval = value
          .parse()
          .ok()
          .filter(|interval| (1..=100).contains(interval))?
      },
      "COUNT" => rule_parts.maybe_count = Some(value.parse().ok().filter(|count| *count >= 1)?),
      "UNTIL" => rule_parts.maybe_until = Some(date_prefix(value)?),
      "BYMONTH" => {
        maybe_month = Some(
          value
            .parse::<u32>()
            .ok()
            .filter(|month| (1..=12).contains(month))?,
        )
      },
      "BYMONTHDAY" => maybe_month_day = Some(value.parse::<u32>().ok().filter(|day| (1..=31).contains(day))?),
      "BYDAY" => maybe_by_day = Some(ordinal_weekday(value)?),
      "WKST" => {},
      _ => return None,
    }
  }
  if !is_yearly {
    return None;
  }
  rule_parts.maybe_day = match (maybe_month, maybe_month_day, maybe_by_day) {
    (None, None, None) => None,
    (Some(month), Some(day), None) => Some(YearDay::Fixed { month, day }),
    (Some(month), None, Some((n, weekday))) => Some(YearDay::Weekday { month, weekday, n }),
    // `BYMONTH` alone repeats on the start's day of that month.
    (Some(_), None, None) => None,
    _ => return None,
  };
  Some(rule_parts)
}

/// `4TH`, `-1MO`: the ordinal and the weekday of a single `BYDAY`.
fn ordinal_weekday(value: &str) -> Option<(i8, Weekday)> {
  let split = value.len().checked_sub(2)?;
  let (number, day) = (value.get(..split)?, value.get(split..)?);
  let n: i8 = number.strip_prefix('+').unwrap_or(number).parse().ok()?;
  if n == 0 || !(-5..=5).contains(&n) {
    return None;
  }
  let weekday = match day {
    "MO" => Weekday::Mon,
    "TU" => Weekday::Tue,
    "WE" => Weekday::Wed,
    "TH" => Weekday::Thu,
    "FR" => Weekday::Fri,
    "SA" => Weekday::Sat,
    "SU" => Weekday::Sun,
    _ => return None,
  };
  Some((n, weekday))
}

fn nth_weekday_of_month(year: i32, month: u32, weekday: Weekday, n: i8) -> Option<NaiveDate> {
  if n > 0 {
    return NaiveDate::from_weekday_of_month_opt(year, month, weekday, n.unsigned_abs());
  }
  let first_of_next = if month == 12 {
    NaiveDate::from_ymd_opt(year.checked_add(1)?, 1, 1)?
  } else {
    NaiveDate::from_ymd_opt(year, month + 1, 1)?
  };
  let last = first_of_next.pred_opt()?;
  let back = (7 + last.weekday().num_days_from_monday() - weekday.num_days_from_monday()) % 7;
  let from_end = i64::from(n.unsigned_abs() - 1) * 7 + i64::from(back);
  let date = last.checked_sub_signed(Duration::days(from_end))?;
  (date.month() == month).then_some(date)
}

/// `DTSTART`: a date (`VALUE=DATE`, or eight digits) or a date with a time.
fn start_of(params: &str, value: &str) -> Start {
  let value = value.trim();
  let is_date = params
    .split(';')
    .any(|param| param.eq_ignore_ascii_case("VALUE=DATE"))
    || value.len() == 8;
  match date_of(value).filter(|_| is_date) {
    Some(date) => Start::Date(date),
    None => Start::Timed,
  }
}

/// A `DATE` value: exactly eight digits.
fn date_of(value: &str) -> Option<NaiveDate> {
  if value.len() != 8 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
    return None;
  }
  NaiveDate::parse_from_str(value, "%Y%m%d").ok()
}

/// The date of a `DATE` or `DATE-TIME` value (`20261225`, `20261225T000000Z`).
fn date_prefix(value: &str) -> Option<NaiveDate> {
  date_of(value.get(..8)?).filter(|_| value.len() == 8 || value.as_bytes().get(8) == Some(&b'T'))
}

/// The whole days of a `DURATION` (`P1D`, `P2W`); `None` for one with a time part.
fn days_of(value: &str) -> Option<i64> {
  let rest = value.strip_prefix('+').unwrap_or(value).strip_prefix('P')?;
  let (number, unit) = rest.split_at(rest.len().checked_sub(1)?);
  let count: i64 = number.parse().ok()?;
  match unit {
    "D" => Some(count),
    "W" => count.checked_mul(7),
    _ => None,
  }
}

/// Name (upper case), parameters and value of a content line; `None` without a colon.
fn content_line(line: &str) -> Option<(String, &str, &str)> {
  // The value starts at the first colon outside a quoted parameter value.
  let mut is_quoted = false;
  let colon = line.char_indices().find_map(|(index, char)| match char {
    '"' => {
      is_quoted = !is_quoted;
      None
    },
    ':' if !is_quoted => Some(index),
    _ => None,
  })?;
  let (head, value) = (&line[..colon], &line[colon + 1..]);
  let (name, params) = head.split_once(';').unwrap_or((head, ""));
  if name.is_empty() {
    return None;
  }
  Some((name.trim().to_ascii_uppercase(), params, value))
}

/// The content lines of the text, folded lines joined (RFC 5545 §3.1).
fn unfolded(text: &str) -> impl Iterator<Item = String> + '_ {
  let mut lines = text
    .split('\n')
    .map(|line| line.strip_suffix('\r').unwrap_or(line))
    .peekable();
  std::iter::from_fn(move || {
    let mut line = lines.next()?.to_owned();
    while let Some(next) = lines.peek() {
      let Some(continued) = next.strip_prefix([' ', '\t']) else {
        break;
      };
      line.push_str(continued);
      lines.next();
    }
    Some(line)
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
  }

  fn window() -> RangeInclusive<NaiveDate> {
    date(2020, 1, 1)..=date(2035, 12, 31)
  }

  fn calendar(events: &str) -> String {
    format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\n{events}END:VCALENDAR\r\n")
  }

  fn event(lines: &str) -> String {
    format!("BEGIN:VEVENT\r\nUID:x\r\n{lines}END:VEVENT\r\n")
  }

  fn parse(text: &str) -> Result<Vec<NaiveDate>, ParseError> {
    parse_holidays(text, &window()).map(|dates| dates.into_iter().collect())
  }

  #[test]
  fn all_day_events_are_holidays_and_timed_ones_are_not() {
    let text = calendar(&format!(
      "{}{}{}",
      event("DTSTART;VALUE=DATE:20261225\r\nDTEND;VALUE=DATE:20261226\r\nSUMMARY:Christmas\r\n"),
      event("DTSTART:20261226\r\nSUMMARY:Boxing Day\r\n"),
      event("DTSTART:20261224T100000Z\r\nDTEND:20261224T110000Z\r\nSUMMARY:Party\r\n"),
    ));
    assert_eq!(parse(&text), Ok(vec![date(2026, 12, 25), date(2026, 12, 26)]));
  }

  #[test]
  fn an_event_over_several_days_counts_each_but_a_long_one_none() {
    let text = calendar(&format!(
      "{}{}{}",
      event("DTSTART;VALUE=DATE:20260403\r\nDTEND;VALUE=DATE:20260407\r\n"),
      event("DTSTART;VALUE=DATE:20260601\r\nDURATION:P2D\r\n"),
      event("DTSTART;VALUE=DATE:20260701\r\nDTEND;VALUE=DATE:20260901\r\nSUMMARY:Summer term\r\n"),
    ));
    assert_eq!(
      parse(&text),
      Ok(vec![
        date(2026, 4, 3),
        date(2026, 4, 4),
        date(2026, 4, 5),
        date(2026, 4, 6),
        date(2026, 6, 1),
        date(2026, 6, 2),
      ])
    );
  }

  #[test]
  fn yearly_rules_repeat_within_the_window_with_count_until_and_exdate() {
    let text = calendar(&format!(
      "{}{}{}",
      event("DTSTART;VALUE=DATE:20300101\r\nRRULE:FREQ=YEARLY\r\nEXDATE;VALUE=DATE:20320101\r\n"),
      event("DTSTART;VALUE=DATE:20200501\r\nRRULE:FREQ=YEARLY;COUNT=2\r\n"),
      event("DTSTART;VALUE=DATE:20201003\r\nRRULE:FREQ=YEARLY;INTERVAL=5;UNTIL=20301231T000000Z\r\n"),
    ));
    let found = parse(&text).unwrap();
    assert!(found.contains(&date(2030, 1, 1)) && found.contains(&date(2035, 1, 1)));
    assert!(!found.contains(&date(2032, 1, 1)), "EXDATE");
    assert!(!found.contains(&date(2036, 1, 1)), "past the window");
    assert!(
      found.contains(&date(2021, 5, 1)) && !found.contains(&date(2022, 5, 1)),
      "COUNT"
    );
    for year in [2020, 2025, 2030] {
      assert!(found.contains(&date(year, 10, 3)), "{year}");
    }
    assert!(!found.contains(&date(2026, 10, 3)) && !found.contains(&date(2035, 10, 3)));
  }

  #[test]
  fn a_yearly_rule_on_a_weekday_of_a_month_follows_it() {
    // Thanksgiving: the fourth Thursday of November; Memorial Day: the last Monday of May.
    let text = calendar(&format!(
      "{}{}",
      event("DTSTART;VALUE=DATE:20251127\r\nRRULE:FREQ=YEARLY;BYMONTH=11;BYDAY=4TH\r\n"),
      event("DTSTART;VALUE=DATE:20250526\r\nRRULE:FREQ=YEARLY;BYMONTH=5;BYDAY=-1MO\r\n"),
    ));
    let found = parse(&text).unwrap();
    assert!(found.contains(&date(2026, 11, 26)) && found.contains(&date(2027, 11, 25)));
    assert!(found.contains(&date(2026, 5, 25)) && found.contains(&date(2027, 5, 31)));
  }

  #[test]
  fn rules_it_cannot_follow_keep_only_the_first_date() {
    let text = calendar(&format!(
      "{}{}",
      event("DTSTART;VALUE=DATE:20260105\r\nRRULE:FREQ=WEEKLY\r\n"),
      event("DTSTART;VALUE=DATE:20260301\r\nRRULE:FREQ=YEARLY;BYWEEKNO=9\r\n"),
    ));
    assert_eq!(parse(&text), Ok(vec![date(2026, 1, 5), date(2026, 3, 1)]));
  }

  #[test]
  fn leap_days_repeat_only_in_leap_years() {
    let text = calendar(&event(
      "DTSTART;VALUE=DATE:20240229\r\nRRULE:FREQ=YEARLY;COUNT=3\r\n",
    ));
    // 2024, 2028, 2032: the years without the day don't count.
    assert_eq!(
      parse(&text),
      Ok(vec![date(2024, 2, 29), date(2028, 2, 29), date(2032, 2, 29)])
    );
  }

  #[test]
  fn folded_lines_lower_case_and_alarms_are_read() {
    let text = "\u{feff}begin:vcalendar\nVERSION:2.0\nBEGIN:VEVENT\nSUMMARY:Very long\n  summary\nDTSTART;\n VALUE=DATE:20261003\nBEGIN:VALARM\nDTSTART:20260101\nEND:VALARM\nEND:VEVENT\nend:vcalendar\n";
    assert_eq!(parse(text), Ok(vec![date(2026, 10, 3)]));
  }

  #[test]
  fn cancelled_events_are_no_holidays() {
    let text = calendar(&format!(
      "{}{}",
      event("DTSTART;VALUE=DATE:20261003\r\n"),
      event("DTSTART;VALUE=DATE:20261004\r\nSTATUS:CANCELLED\r\n"),
    ));
    assert_eq!(parse(&text), Ok(vec![date(2026, 10, 3)]));
  }

  #[test]
  fn html_and_other_text_are_not_calendars() {
    for text in [
      "<!DOCTYPE html><html><body>Please sign in to the hotel wifi</body></html>",
      "<html>BEGIN:VCALENDAR</html>",
      "{\"holidays\": []}",
      "",
      "   ",
    ] {
      assert_eq!(parse(text), Err(ParseError::NotACalendar), "{text}");
    }
  }

  #[test]
  fn broken_and_cut_off_feeds_are_refused() {
    let good_event = event("DTSTART;VALUE=DATE:20261003\r\n");
    for text in [
      // Cut off by a size limit or a broken connection.
      format!("BEGIN:VCALENDAR\r\n{good_event}"),
      format!("BEGIN:VCALENDAR\r\n{}", &good_event[..30]),
      // Components that don't close in order.
      "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nDTSTART;VALUE=DATE:20261003\r\nEND:VTODO\r\nEND:VCALENDAR\r\n"
        .to_owned(),
      format!("BEGIN:VCALENDAR\r\nEND:VEVENT\r\n{good_event}END:VCALENDAR\r\n"),
      format!("BEGIN:VCALENDAR\r\nBEGIN:VCALENDAR\r\n{good_event}END:VCALENDAR\r\nEND:VCALENDAR\r\n"),
      // A line without a colon.
      format!("BEGIN:VCALENDAR\r\nthis is not a content line\r\n{good_event}END:VCALENDAR\r\n"),
      // Nested too deep.
      format!(
        "BEGIN:VCALENDAR\r\n{}{}END:VCALENDAR\r\n",
        "BEGIN:X\r\n".repeat(10),
        "END:X\r\n".repeat(10)
      ),
    ] {
      assert_eq!(parse(&text), Err(ParseError::Malformed), "{text}");
    }
  }

  #[test]
  fn broken_values_skip_the_event_not_the_feed() {
    let text = calendar(&format!(
      "{}{}{}{}",
      event("DTSTART;VALUE=DATE:20261399\r\n"),
      event("DTSTART;VALUE=DATE:2026-10-03\r\n"),
      event("DTSTART;VALUE=DATE:20261225\r\nRRULE:FREQ=YEARLY;COUNT=zero\r\n"),
      event("DTSTART;VALUE=DATE:20261003\r\nDTEND;VALUE=DATE:garbage\r\n"),
    ));
    assert_eq!(parse(&text), Ok(vec![date(2026, 10, 3), date(2026, 12, 25)]));
  }

  #[test]
  fn a_calendar_without_holidays_in_the_window_is_empty() {
    assert_eq!(parse(&calendar("")), Err(ParseError::Empty));
    let timed_only = calendar(&event("DTSTART:20261003T090000\r\n"));
    assert_eq!(parse(&timed_only), Err(ParseError::Empty));
    let long_ago = calendar(&event("DTSTART;VALUE=DATE:19991231\r\n"));
    assert_eq!(parse(&long_ago), Err(ParseError::Empty));
  }

  #[test]
  fn too_many_holidays_are_refused() {
    // 31 days per event, 200 events a year apart: more than the copy keeps.
    let events: String = (0..200)
      .map(|index| {
        let start = date(2020, 1, 1) + Duration::days(index * 31);
        event(&format!(
          "DTSTART;VALUE=DATE:{:04}{:02}{:02}\r\nDURATION:P31D\r\n",
          start.year(),
          start.month(),
          start.day()
        ))
      })
      .collect();
    assert_eq!(parse(&calendar(&events)), Err(ParseError::TooMany));
  }

  #[test]
  fn quoted_colons_in_parameters_do_not_split_the_line() {
    let text = calendar(&event(
      "DTSTART;VALUE=DATE:20261003\r\nATTENDEE;CN=\"Doe: John\":mailto:x@example.com\r\n",
    ));
    assert_eq!(parse(&text), Ok(vec![date(2026, 10, 3)]));
  }
}
