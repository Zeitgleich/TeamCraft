//! Log lines: their level (`ERROR`, `[warn]`, `level=info`, `"level":"debug"`, glog's `E1027`) and
//! the time they start with, both looked for near the start of a line only. A line without a level
//! belongs to the entry above it (a stack trace, a wrapped message) unless it starts with a time of
//! its own; the levels match the integration log view's (`LogLevel`).

use std::ops::Range;

use super::Span;
use crate::model::LogLevel;

/// How far into a line its level is looked for.
const HEAD_BYTES: usize = 160;
/// Non-empty lines looked at to decide whether a plain text file is a log.
const SAMPLE_LINES: usize = 500;
/// A plain text file is a log when at least this many of its sampled lines have a level…
const MIN_LEVEL_LINES: usize = 3;
/// …and at least one in this many.
const LEVEL_LINE_DIVISOR: usize = 4;

/// The level a line names near its start, and where the word is. Upper-case words count anywhere
/// in the head (`2026-10-27 12:00:00 ERROR …`); other spellings only in brackets (`[error]`), as a
/// key's value (`level=warn`, `"severity":"info"`) or before a colon (`Warning: …`), so ordinary
/// words in a sentence don't.
pub fn level_of(line: &str) -> Option<(LogLevel, Range<usize>)> {
  if let Some(level) = glog_level(line) {
    return Some((level, 0..1));
  }
  let head = head(line);
  let bytes = head.as_bytes();
  let mut start = 0;
  while start < bytes.len() {
    if !bytes[start].is_ascii_alphabetic() {
      start += 1;
      continue;
    }
    let end = bytes[start..]
      .iter()
      .position(|byte| !byte.is_ascii_alphabetic())
      .map_or(bytes.len(), |found| start + found);
    let word = &head[start..end];
    if let Some(level) = level_word(word).filter(|_| is_level_spot(head, start, end)) {
      return Some((level, start..end));
    }
    start = end;
  }
  None
}

/// Where a line's time is, when it starts with one (inside a leading `[` too): an ISO 8601 or
/// slashed date with or without a time, a time of day (`12:00:00.123`), or a syslog stamp
/// (`Oct 27 12:00:00`).
pub fn timestamp_of(line: &str) -> Option<Range<usize>> {
  let bytes = line.as_bytes();
  let start = usize::from(bytes.first() == Some(&b'['));
  date_time(bytes, start)
    .or_else(|| time_of_day(bytes, start, true))
    .or_else(|| syslog_stamp(bytes, start))
    .map(|end| start..end)
}

/// The level of each of `lines`, continuing from `previous` (the level of the line before the
/// first; `Info` at the start of a file).
pub fn entry_levels(text: &str, lines: &[Span], mut previous: LogLevel) -> Vec<LogLevel> {
  lines
    .iter()
    .map(|span| {
      let line = text.get(span.range()).unwrap_or_default();
      previous = match level_of(line) {
        Some((level, _)) => level,
        None if timestamp_of(line).is_some() => LogLevel::Info,
        None => previous,
      };
      previous
    })
    .collect()
}

/// Whether a file that didn't say it is a log (`.txt`, `text/plain`) reads like one: enough of
/// its first lines carry a level.
pub fn looks_like_log(text: &str, lines: &[Span]) -> bool {
  let (sampled, with_level) = lines
    .iter()
    .filter_map(|span| text.get(span.range()))
    .filter(|line| !line.trim().is_empty())
    .take(SAMPLE_LINES)
    .fold((0, 0), |(sampled, with_level), line| {
      (sampled + 1, with_level + usize::from(level_of(line).is_some()))
    });
  with_level >= MIN_LEVEL_LINES && with_level * LEVEL_LINE_DIVISOR >= sampled
}

/// The level a log word names, in any case; the integration log view's words plus the usual
/// abbreviations (`DBG`, `WRN`, `CRIT`, …).
fn level_word(word: &str) -> Option<LogLevel> {
  if word.len() > 11 {
    return None;
  }
  match word.to_ascii_lowercase().as_str() {
    "trc" | "dbg" | "verbose" => Some(LogLevel::Debug),
    "inf" | "information" => Some(LogLevel::Info),
    "wrn" => Some(LogLevel::Warn),
    "crit" | "emerg" | "panic" | "severe" => Some(LogLevel::Error),
    other => LogLevel::from_word(other),
  }
}

/// Whether the word at `start..end` of `head` stands where a log puts its level.
fn is_level_spot(head: &str, start: usize, end: usize) -> bool {
  let bytes = head.as_bytes();
  let before = start.checked_sub(1).map(|at| bytes[at]);
  let after = bytes.get(end).copied();
  let is_word_char = |byte: Option<u8>| byte.is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
  if is_word_char(before) || is_word_char(after) {
    return false;
  }
  let word = &head[start..end];
  let is_upper = word.bytes().all(|byte| byte.is_ascii_uppercase());
  let is_bracketed =
    matches!(before, Some(b'[' | b'<' | b'(')) && matches!(after, Some(b']' | b'>' | b')' | b':'));
  let is_capitalized_with_colon = word.as_bytes()[0].is_ascii_uppercase() && after == Some(b':');
  is_upper || is_bracketed || is_capitalized_with_colon || is_keyed_value(&head[..start])
}

/// Whether `before` ends with a level key and its separator: `level=`, `lvl: `, `"severity":"`.
fn is_keyed_value(before: &str) -> bool {
  let key = before.trim_end_matches(['"', '\'', ' ', '=', ':']);
  if key.len() == before.len() {
    return false;
  }
  let key = key.to_ascii_lowercase();
  ["level", "lvl", "severity", "loglevel"]
    .iter()
    .any(|name| key.ends_with(name))
}

/// glog and its relatives: `E1027 12:00:00.123456 …`.
fn glog_level(line: &str) -> Option<LogLevel> {
  let bytes = line.as_bytes();
  let is_stamp = bytes.len() > 6 && bytes[1..5].iter().all(u8::is_ascii_digit) && bytes[5] == b' ';
  if !is_stamp {
    return None;
  }
  match bytes[0] {
    b'I' => Some(LogLevel::Info),
    b'W' => Some(LogLevel::Warn),
    b'E' | b'F' => Some(LogLevel::Error),
    _ => None,
  }
}

/// The first [`HEAD_BYTES`] of a line, cut at a character boundary.
fn head(line: &str) -> &str {
  let mut end = line.len().min(HEAD_BYTES);
  while !line.is_char_boundary(end) {
    end -= 1;
  }
  &line[..end]
}

/// `2026-10-27`, `2026/10/27`, then optionally `T` or a space and a time of day: the end.
fn date_time(bytes: &[u8], start: usize) -> Option<usize> {
  let year = digits(bytes, start, 4)?;
  let separator = *bytes.get(year)?;
  if separator != b'-' && separator != b'/' {
    return None;
  }
  let month = digits(bytes, year + 1, 2)?;
  if bytes.get(month) != Some(&separator) {
    return None;
  }
  let day = digits(bytes, month + 1, 2)?;
  match bytes.get(day) {
    Some(b'T' | b' ') => Some(time_of_day(bytes, day + 1, false).unwrap_or(day)),
    _ => Some(day),
  }
}

/// `12:00`, `12:00:00`, `12:00:00.123` or `,123`, then optionally `Z` or an offset: the end. With
/// `needs_seconds` (a line that starts with a bare time) `12:00` alone is no time.
fn time_of_day(bytes: &[u8], start: usize, needs_seconds: bool) -> Option<usize> {
  let hours = digits(bytes, start, 2)?;
  let minutes = (bytes.get(hours) == Some(&b':'))
    .then(|| digits(bytes, hours + 1, 2))
    .flatten()?;
  let mut end = minutes;
  if bytes.get(end) == Some(&b':') {
    match digits(bytes, end + 1, 2) {
      Some(seconds) => end = seconds,
      None if needs_seconds => return None,
      None => return Some(end),
    }
    if matches!(bytes.get(end), Some(b'.' | b',')) {
      let fraction = bytes[end + 1..]
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
      if fraction > 0 {
        end += 1 + fraction;
      }
    }
  } else if needs_seconds {
    return None;
  }
  Some(zone(bytes, end))
}

/// After a time: `Z`, `+02:00`, `-0500` or nothing.
fn zone(bytes: &[u8], start: usize) -> usize {
  match bytes.get(start) {
    Some(b'Z') => start + 1,
    Some(b'+' | b'-') => {
      let Some(hours) = digits(bytes, start + 1, 2) else {
        return start;
      };
      let minutes_at = hours + usize::from(bytes.get(hours) == Some(&b':'));
      digits(bytes, minutes_at, 2).unwrap_or(hours)
    },
    _ => start,
  }
}

/// `Oct 27 12:00:00` (a day below ten has a space or a zero first): the end.
fn syslog_stamp(bytes: &[u8], start: usize) -> Option<usize> {
  let month = bytes.get(start..start + 3)?;
  let is_month = month[0].is_ascii_uppercase() && month[1..].iter().all(u8::is_ascii_lowercase);
  if !is_month || bytes.get(start + 3) != Some(&b' ') {
    return None;
  }
  let day_start = start + 4;
  let day_end = match bytes.get(day_start) {
    Some(b' ') => digits(bytes, day_start + 1, 1)?,
    _ => digits(bytes, day_start, 2)?,
  };
  if bytes.get(day_end) != Some(&b' ') {
    return None;
  }
  time_of_day(bytes, day_end + 1, true)
}

/// The end of exactly `count` ASCII digits at `start`.
fn digits(bytes: &[u8], start: usize, count: usize) -> Option<usize> {
  let end = start + count;
  bytes
    .get(start..end)
    .filter(|found| found.iter().all(u8::is_ascii_digit))
    .map(|_| end)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::text_preview::index_lines;

  mod levels {
    use super::*;

    #[test]
    fn upper_case_level_words_anywhere_in_the_head() {
      assert_eq!(
        level("2026-10-27T10:00:00.123Z  INFO teamcraft_server: started"),
        Some((LogLevel::Info, "INFO"))
      );
      assert_eq!(
        level("2026-10-27 10:00:00,001 WARNING [main] disk"),
        Some((LogLevel::Warn, "WARNING"))
      );
      assert_eq!(level("ERROR:root:failed"), Some((LogLevel::Error, "ERROR")));
      assert_eq!(level("10:00:00 DBG cache miss"), Some((LogLevel::Debug, "DBG")));
      assert_eq!(
        level("[12:00:01] FATAL: out of memory"),
        Some((LogLevel::Error, "FATAL"))
      );
    }

    #[test]
    fn other_spellings_only_where_a_log_puts_them() {
      assert_eq!(
        level("2026/10/27 10:00:00 [error] 12#0: upstream"),
        Some((LogLevel::Error, "error"))
      );
      assert_eq!(
        level("time=1 level=warn msg=\"slow\""),
        Some((LogLevel::Warn, "warn"))
      );
      assert_eq!(
        level("{\"severity\": \"Info\", \"msg\": \"x\"}"),
        Some((LogLevel::Info, "Info"))
      );
      assert_eq!(
        level("{\"level\":\"debug\",\"message\":\"x\"}"),
        Some((LogLevel::Debug, "debug"))
      );
      assert_eq!(
        level("Warning: deprecated option"),
        Some((LogLevel::Warn, "Warning"))
      );
      assert_eq!(level("an error happened, info follows"), None);
      assert_eq!(level("Error handling is fine"), None);
    }

    #[test]
    fn parts_of_longer_words_are_no_levels() {
      assert_eq!(level("ERRORS: 0, INFOS: 2"), None);
      assert_eq!(level("ERROR1 WARN_X information"), None);
    }

    #[test]
    fn glog_letters() {
      assert_eq!(
        level_of("E1027 10:00:00.123456  1234 main.cc:12] boom").map(|(level, _)| level),
        Some(LogLevel::Error)
      );
      assert_eq!(level_of("I1027 10:00:00 ok"), Some((LogLevel::Info, 0..1)));
      assert_eq!(level_of("X1027 x"), None);
    }

    #[test]
    fn only_the_head_of_a_long_line_is_searched() {
      let line = format!("{} ERROR late", "x ".repeat(200));
      assert_eq!(level_of(&line), None);
      // The cut falls inside a character.
      let multibyte = format!("a{} ERROR", "ä".repeat(80));
      assert_eq!(level_of(&multibyte), None);
    }

    #[test]
    fn lines_without_a_level_belong_to_the_entry_above() {
      let text = "plain start\n2026-10-27 10:00:00 ERROR boom\n  at main.rs:1\n\n2026-10-27 10:00:01 next\nWARN x\ncontinued";
      let mut lines = Vec::new();
      index_lines(text, 0, usize::MAX, &mut lines);
      assert_eq!(
        entry_levels(text, &lines, LogLevel::Info),
        [
          LogLevel::Info,
          LogLevel::Error,
          LogLevel::Error,
          LogLevel::Error,
          LogLevel::Info,
          LogLevel::Warn,
          LogLevel::Warn,
        ]
      );
      // In steps: the level of the line before carries into the next step.
      assert_eq!(
        entry_levels(text, &lines[2..3], LogLevel::Error),
        [LogLevel::Error]
      );
    }
  }

  mod times {
    use super::*;

    #[test]
    fn stamps_at_the_start_of_a_line() {
      assert_eq!(
        stamp("2026-10-27T10:00:00.123Z INFO x"),
        Some("2026-10-27T10:00:00.123Z")
      );
      assert_eq!(
        stamp("2026-10-27 10:00:00,001 x"),
        Some("2026-10-27 10:00:00,001")
      );
      assert_eq!(stamp("2026/10/27 10:00:00 [error]"), Some("2026/10/27 10:00:00"));
      assert_eq!(
        stamp("2026-10-27T10:00:00+02:00 x"),
        Some("2026-10-27T10:00:00+02:00")
      );
      assert_eq!(stamp("2026-10-27 report"), Some("2026-10-27"));
      assert_eq!(stamp("[10:00:01.5] x"), Some("10:00:01.5"));
      assert_eq!(stamp("Oct 27 10:00:00 host sshd[1]: x"), Some("Oct 27 10:00:00"));
      assert_eq!(stamp("Oct  7 10:00:00 host"), Some("Oct  7 10:00:00"));
    }

    #[test]
    fn things_that_only_look_a_bit_like_times() {
      assert_eq!(stamp("10:30 meeting"), None);
      assert_eq!(stamp("2026-1-27"), None);
      assert_eq!(stamp("Total 12:00:00"), None);
      assert_eq!(stamp(""), None);
      assert_eq!(stamp("2026-10-27T"), Some("2026-10-27"));
    }
  }

  mod detection {
    use super::*;

    #[test]
    fn a_text_with_levels_reads_like_a_log() {
      assert!(looks("INFO a\nINFO b\n  more\nWARN c\nplain"));
      assert!(!looks(
        "INFO a\nplain\nplain\nplain\nplain\nplain\nplain\nINFO b\nWARN c\nplain\nplain\nplain\nplain"
      ));
      assert!(!looks("Dear team,\nthe report is attached.\nRegards"));
      assert!(!looks("ERROR once\n"));
    }

    fn looks(text: &str) -> bool {
      let mut lines = Vec::new();
      index_lines(text, 0, usize::MAX, &mut lines);
      looks_like_log(text, &lines)
    }
  }

  fn level(line: &str) -> Option<(LogLevel, &str)> {
    level_of(line).map(|(level, at)| (level, &line[at]))
  }

  fn stamp(line: &str) -> Option<&str> {
    timestamp_of(line).map(|at| &line[at])
  }
}
