//! The level of a log line (`docs/bridge.md`, "Log streams"): one detection for the bridge (which
//! alerts on it), the integration log view and the text preview, so a line has the same level
//! everywhere. Levels are looked for near the start of a line only, after a timestamp or a
//! `host unit[pid]:` prefix: `2026-11-12T10:00:00Z ERROR disk full`, `[warn] slow`, `level=error`,
//! glog's `E1027 …`.

use std::ops::Range;

use serde::{Deserialize, Serialize};

/// How far into a line its level is looked for.
const HEAD_BYTES: usize = 160;
/// Longest level word (`information`).
const MAX_WORD_BYTES: usize = 11;

/// Ordered from least to most severe, so `level >= minimum` filters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum LogLevel {
  Debug,
  Info,
  Warn,
  Error,
}

/// A level found in a line: its coarse level, the canonical lower-case name (what a source's alert
/// levels list; `crit` is `critical`) and where the word stands in the line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LevelMatch {
  pub level: LogLevel,
  pub name: &'static str,
  pub range: Range<usize>,
}

impl LogLevel {
  pub fn slug(self) -> &'static str {
    match self {
      Self::Debug => "debug",
      Self::Info => "info",
      Self::Warn => "warn",
      Self::Error => "error",
    }
  }

  pub fn from_slug(slug: &str) -> Option<Self> {
    [Self::Debug, Self::Info, Self::Warn, Self::Error]
      .into_iter()
      .find(|level| level.slug() == slug)
  }

  /// The level named by a log word in any case (`warning`, `ERR`, `fatal`, `WRN`, …); `None` for
  /// anything else.
  pub fn from_word(word: &str) -> Option<Self> {
    level_word(word).map(|(level, _)| level)
  }
}

/// The first level in the head of `line`. Upper-case words count anywhere in the head
/// (`2026-10-27 12:00:00 ERROR …`, `host unit[42]: WARN …`); other spellings only in brackets
/// (`[error]`), as a key's value (`level=warn`, `"severity":"info"`) or before a colon
/// (`Warning: …`, `error: …`), so ordinary words in a sentence ("no error found") don't count.
pub fn find_level(line: &str) -> Option<LevelMatch> {
  if let Some((level, name)) = glog_level(line) {
    return Some(LevelMatch {
      level,
      name,
      range: 0..1,
    });
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
    if let Some((level, name)) = level_word(&head[start..end]).filter(|_| is_level_spot(head, start, end)) {
      return Some(LevelMatch {
        level,
        name,
        range: start..end,
      });
    }
    start = end;
  }
  None
}

/// The level a number names, for structured logs: syslog priorities `0..=7` (journald's
/// `PRIORITY`) and the `10..=60` scale of bunyan and pino.
pub fn level_from_number(number: u64) -> Option<(LogLevel, &'static str)> {
  match number {
    0 => Some((LogLevel::Error, "emergency")),
    1 => Some((LogLevel::Error, "alert")),
    2 => Some((LogLevel::Error, "critical")),
    3 => Some((LogLevel::Error, "error")),
    4 => Some((LogLevel::Warn, "warning")),
    5 => Some((LogLevel::Info, "notice")),
    6 => Some((LogLevel::Info, "info")),
    7 => Some((LogLevel::Debug, "debug")),
    10 => Some((LogLevel::Debug, "trace")),
    20 => Some((LogLevel::Debug, "debug")),
    30 => Some((LogLevel::Info, "info")),
    40 => Some((LogLevel::Warn, "warn")),
    50 => Some((LogLevel::Error, "error")),
    60 => Some((LogLevel::Error, "fatal")),
    _ => None,
  }
}

/// The level of a structured log's level field: a word (`"ERROR"`, `"warning"`) or a number, also
/// as text (journald sends `"PRIORITY":"3"`).
pub fn parse_level_field(text: &str) -> Option<(LogLevel, &'static str)> {
  let text = text.trim();
  match text.parse::<u64>() {
    Ok(number) => level_from_number(number),
    Err(_) => level_word(text),
  }
}

/// The level a log word names, in any case, with its canonical name: the usual words plus
/// abbreviations (`DBG`, `WRN`, `CRIT`, …).
fn level_word(word: &str) -> Option<(LogLevel, &'static str)> {
  if word.len() > MAX_WORD_BYTES {
    return None;
  }
  let level = match word.to_ascii_lowercase().as_str() {
    "trace" | "trc" => (LogLevel::Debug, "trace"),
    "debug" | "dbg" | "verbose" => (LogLevel::Debug, "debug"),
    "info" | "inf" | "information" => (LogLevel::Info, "info"),
    "notice" => (LogLevel::Info, "notice"),
    "warn" | "wrn" => (LogLevel::Warn, "warn"),
    "warning" => (LogLevel::Warn, "warning"),
    "error" | "severe" => (LogLevel::Error, "error"),
    "err" => (LogLevel::Error, "err"),
    "fatal" | "panic" => (LogLevel::Error, "fatal"),
    "critical" | "crit" => (LogLevel::Error, "critical"),
    "alert" => (LogLevel::Error, "alert"),
    "emergency" | "emerg" => (LogLevel::Error, "emergency"),
    _ => return None,
  };
  Some(level)
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
  let is_before_colon = after == Some(b':') && head[..start].trim_start_matches(['[', '<', '(']).is_empty();
  let is_capitalized_with_colon = word.as_bytes()[0].is_ascii_uppercase() && after == Some(b':');
  is_upper || is_bracketed || is_before_colon || is_capitalized_with_colon || is_keyed_value(&head[..start])
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
fn glog_level(line: &str) -> Option<(LogLevel, &'static str)> {
  let bytes = line.as_bytes();
  let is_stamp = bytes.len() > 6 && bytes[1..5].iter().all(u8::is_ascii_digit) && bytes[5] == b' ';
  if !is_stamp {
    return None;
  }
  match bytes[0] {
    b'I' => Some((LogLevel::Info, "info")),
    b'W' => Some((LogLevel::Warn, "warning")),
    b'E' => Some((LogLevel::Error, "error")),
    b'F' => Some((LogLevel::Error, "fatal")),
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

#[cfg(test)]
mod tests {
  use super::*;

  mod real_world_lines {
    use super::*;

    #[test]
    fn level_after_a_timestamp_or_host_prefix() {
      let cases = [
        ("2026-11-12T10:00:00Z ERROR disk full", LogLevel::Error, "error"),
        (
          "2026-11-12T10:00:00+0100 host unit[42]: ERROR disk full",
          LogLevel::Error,
          "error",
        ),
        (
          "Nov 12 10:00:00 host sshd[42]: WARN too many tries",
          LogLevel::Warn,
          "warn",
        ),
        (
          "2026-10-27T10:00:00.123Z  INFO teamcraft_server: started",
          LogLevel::Info,
          "info",
        ),
        (
          "2026-10-27 10:00:00,001 WARNING [main] disk",
          LogLevel::Warn,
          "warning",
        ),
        (
          "2026/10/27 10:00:00 [crit] 12#0: upstream",
          LogLevel::Error,
          "critical",
        ),
        ("10:00:00 DBG cache miss", LogLevel::Debug, "debug"),
        ("[12:00:01] FATAL: out of memory", LogLevel::Error, "fatal"),
        ("ERROR:root:failed", LogLevel::Error, "error"),
        (
          "time=2026-11-12T10:00:00Z level=error msg=\"boom\"",
          LogLevel::Error,
          "error",
        ),
        ("ts=1 lvl=warn msg=slow", LogLevel::Warn, "warn"),
        ("{\"severity\": \"Info\", \"msg\": \"x\"}", LogLevel::Info, "info"),
        ("warning: slow query", LogLevel::Warn, "warning"),
        ("error: it broke", LogLevel::Error, "error"),
        ("[error] it broke", LogLevel::Error, "error"),
        (
          "E1027 10:00:00.123456  1234 main.cc:12] boom",
          LogLevel::Error,
          "error",
        ),
      ];
      for (line, level, name) in cases {
        let found = find_level(line).unwrap_or_else(|| panic!("no level in {line:?}"));
        assert_eq!((found.level, found.name), (level, name), "{line:?}");
      }
    }

    #[test]
    fn the_range_is_the_word() {
      let found = find_level("2026-11-12T10:00:00Z ERROR disk full").unwrap();
      assert_eq!(&"2026-11-12T10:00:00Z ERROR disk full"[found.range], "ERROR");
    }
  }

  mod false_positives {
    use super::*;

    #[test]
    fn level_words_in_prose_are_no_levels() {
      for line in [
        "no error found",
        "an error happened, info follows",
        "Error handling is fine",
        "2026-11-12T10:00:00Z host app[1]: no error found",
        "ERRORS: 0, INFOS: 2",
        "ERROR1 WARN_X information",
        "started",
        "",
      ] {
        assert_eq!(find_level(line), None, "{line:?}");
      }
    }

    #[test]
    fn only_the_head_of_a_long_line_is_searched() {
      let line = format!("{} ERROR late", "x ".repeat(200));
      assert_eq!(find_level(&line), None);
      // The cut falls inside a character.
      let multibyte = format!("a{} ERROR", "ä".repeat(80));
      assert_eq!(find_level(&multibyte), None);
    }

    #[test]
    fn garbage_and_oversized_input_do_not_panic() {
      let huge = "[".repeat(1_000_000);
      assert_eq!(find_level(&huge), None);
      for line in [
        "\0\0\0",
        "E",
        "E1",
        "E1234 ",
        "\u{feff}[",
        "[]",
        "level=",
        "level=\"",
        "ÄÖÜ ERROR",
      ] {
        let _ = find_level(line);
      }
    }
  }

  mod numbers_and_fields {
    use super::*;

    #[test]
    fn syslog_priorities_and_json_scales() {
      assert_eq!(parse_level_field("3"), Some((LogLevel::Error, "error")));
      assert_eq!(parse_level_field("0"), Some((LogLevel::Error, "emergency")));
      assert_eq!(parse_level_field("4"), Some((LogLevel::Warn, "warning")));
      assert_eq!(parse_level_field("6"), Some((LogLevel::Info, "info")));
      assert_eq!(parse_level_field("7"), Some((LogLevel::Debug, "debug")));
      assert_eq!(parse_level_field("50"), Some((LogLevel::Error, "error")));
      assert_eq!(parse_level_field("60"), Some((LogLevel::Error, "fatal")));
      assert_eq!(parse_level_field("8"), None);
      assert_eq!(parse_level_field("99999999999999999999999"), None);
    }

    #[test]
    fn words_in_any_case() {
      assert_eq!(parse_level_field(" ERROR "), Some((LogLevel::Error, "error")));
      assert_eq!(parse_level_field("Warning"), Some((LogLevel::Warn, "warning")));
      assert_eq!(parse_level_field("verbose"), Some((LogLevel::Debug, "debug")));
      assert_eq!(parse_level_field("loud"), None);
    }

    #[test]
    fn slugs_round_trip() {
      for level in [LogLevel::Debug, LogLevel::Info, LogLevel::Warn, LogLevel::Error] {
        assert_eq!(LogLevel::from_slug(level.slug()), Some(level));
      }
      assert_eq!(LogLevel::from_slug("nope"), None);
    }
  }
}
