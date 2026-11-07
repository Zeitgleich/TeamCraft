//! The recent `tracing` events of this process, for "Report a problem" (docs/bug-reports.md) and
//! the mobile diagnostics export (issue #1094): a bounded ring in memory, never on disk.
//!
//! Every line passes [`redact`] before it is stored. security.md already forbids logging content,
//! file names, keys, tokens and passwords; the redaction is defence in depth for a line that breaks
//! that rule, so a report can't carry what a careless log line put there.

use std::collections::VecDeque;
use std::fmt::{self, Write as _};
use std::sync::{Arc, Mutex, OnceLock};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::filter::{Filtered, Targets};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

/// Lines the ring keeps unless the shell asks for another size; older ones drop out.
pub const DEFAULT_MAX_LINES: usize = 500;
/// Longest line the ring keeps, in characters; the rest is cut so the ring's memory stays bounded.
pub const DEFAULT_MAX_LINE_CHARS: usize = 1024;

/// What stands in for a removed value.
const REDACTED: &str = "[redacted]";
/// A run of hex or base64 at least this long is treated as a key, hash or token.
const SECRET_RUN_CHARS: usize = 24;
/// Target prefix of TeamCraft's own crates (and the FFI's lifecycle target): kept from `info`.
const OWN_TARGET: &str = "teamcraft";

/// Field names whose values are never kept: they name content or secrets.
const SECRET_FIELDS: &[&str] = &[
  "authorization",
  "body",
  "caption",
  "code",
  "comment",
  "content",
  "cookie",
  "description",
  "draft",
  "email",
  "file_name",
  "filename",
  "key",
  "label",
  "name",
  "note",
  "passphrase",
  "password",
  "path",
  "payload",
  "preview",
  "query",
  "quote",
  "secret",
  "signature",
  "subject",
  "summary",
  "text",
  "title",
  "token",
  "topic",
  "url",
];
/// Field name endings that name content or secrets (`access_token`, `display_name`, …).
const SECRET_FIELD_SUFFIXES: &[&str] = &[
  "_body",
  "_code",
  "_email",
  "_key",
  "_name",
  "_passphrase",
  "_password",
  "_payload",
  "_secret",
  "_text",
  "_token",
  "_url",
];

/// Field names whose text values are kept (after [`redact`]): IDs, errors and the words of the
/// app's own state. Any other text field is written as `[redacted]`; numbers and flags are always
/// kept. An allowlist, so a field added later with content in it doesn't leak by default.
const PLAIN_FIELDS: &[&str] = &[
  "attempt",
  "attempts",
  "cause",
  "codec",
  "direction",
  "error",
  "error_code",
  "event",
  "interception",
  "job",
  "kind",
  "layer",
  "level",
  "location",
  "method",
  "mode",
  "network",
  "outcome",
  "phase",
  "platform",
  "policy",
  "reason",
  "resolution",
  "role",
  "state",
  "status",
  "step",
  "version",
];
/// Field name endings whose text values are kept (`conversation_id`, `waited_ms`, `error_kind`).
const PLAIN_FIELD_SUFFIXES: &[&str] = &[
  "_bytes", "_count", "_error", "_id", "_ids", "_kind", "_mode", "_ms", "_secs", "_state", "_status",
];
/// Path beginnings that may follow a separator inside a value; the path runs to the closing quote,
/// or to the end of the value, so names with spaces go as a whole.
const PATH_STARTS: &[&str] = &["file://", "content://", "~/", "~\\", "\\\\"];

/// How large the ring is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DebugLogSettings {
  pub max_lines: usize,
  pub max_line_chars: usize,
}

impl Default for DebugLogSettings {
  fn default() -> Self {
    Self {
      max_lines: DEFAULT_MAX_LINES,
      max_line_chars: DEFAULT_MAX_LINE_CHARS,
    }
  }
}

/// A bounded, redacted copy of the recent log; also the `tracing` layer that fills it. Clones share
/// the ring.
#[derive(Clone, Debug)]
pub struct DebugLog {
  settings: DebugLogSettings,
  lines: Arc<Mutex<VecDeque<String>>>,
}

/// The process's ring, created with the default size on first use. Shells add it to their
/// subscriber ([`DebugLog::layer`]) and read it for a report ([`recent_lines`]).
pub fn global() -> &'static DebugLog {
  static RING: OnceLock<DebugLog> = OnceLock::new();
  RING.get_or_init(|| DebugLog::new(DebugLogSettings::default()))
}

/// The process's recent log lines, oldest first.
pub fn recent_lines() -> Vec<String> {
  global().lines()
}

/// The process's ring as the layer shells add to their subscriber, with its own filter: events it
/// doesn't keep are never built for it, and the other layers keep their own levels.
pub fn layer<S>() -> Filtered<DebugLog, Targets, S>
where
  S: Subscriber + for<'span> LookupSpan<'span>,
{
  global().clone().with_filter(kept_targets())
}

/// [`DebugLog::is_kept`] as a filter.
fn kept_targets() -> Targets {
  Targets::new()
    .with_default(Level::WARN)
    .with_target(OWN_TARGET, Level::INFO)
}

impl DebugLog {
  pub fn new(settings: DebugLogSettings) -> Self {
    Self {
      settings,
      lines: Arc::new(Mutex::new(VecDeque::with_capacity(
        settings.max_lines.min(DEFAULT_MAX_LINES),
      ))),
    }
  }

  /// The kept lines, oldest first.
  pub fn lines(&self) -> Vec<String> {
    self.lock().iter().cloned().collect()
  }

  /// Whether an event is worth a line: `info` and above from TeamCraft's crates, `warn` and above
  /// from dependencies (they log protocol detail at `info`).
  pub fn is_kept(target: &str, level: &Level) -> bool {
    let floor = if target.starts_with(OWN_TARGET) {
      Level::INFO
    } else {
      Level::WARN
    };
    *level <= floor
  }

  fn push(&self, line: String) {
    if self.settings.max_lines == 0 {
      return;
    }
    let line = truncate(line, self.settings.max_line_chars);
    let mut lines = self.lock();
    while lines.len() >= self.settings.max_lines {
      lines.pop_front();
    }
    lines.push_back(line);
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<String>> {
    // A panic while pushing leaves a valid deque; the log must keep working after it.
    self.lines.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }
}

impl<S: Subscriber> Layer<S> for DebugLog {
  // No `enabled`: a layer's `false` there would silence the event for the console and the other
  // layers too. Events are judged in `on_event`.
  fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
    let metadata = event.metadata();
    if !Self::is_kept(metadata.target(), metadata.level()) {
      return;
    }
    let mut line = format!(
      "{} {:>5} {}:",
      timestamp(now_ms()),
      metadata.level(),
      metadata.target()
    );
    event.record(&mut LineVisitor(&mut line));
    self.push(line);
  }
}

/// Appends an event's message and `name=value` fields to a line, redacted.
struct LineVisitor<'a>(&'a mut String);

impl LineVisitor<'_> {
  fn field_text(&mut self, field: &Field, value: &str) {
    // Writing to a String can't fail.
    let name = field.name();
    if name == "message" {
      let _ = write!(self.0, " {}", redact(value));
    } else if is_plain_field(name) && !is_secret_field(name) {
      let _ = write!(self.0, " {name}={}", redact(value));
    } else {
      let _ = write!(self.0, " {name}={REDACTED}");
    }
  }
}

impl Visit for LineVisitor<'_> {
  fn record_i64(&mut self, field: &Field, value: i64) {
    let _ = write!(self.0, " {}={value}", field.name());
  }

  fn record_u64(&mut self, field: &Field, value: u64) {
    let _ = write!(self.0, " {}={value}", field.name());
  }

  fn record_bool(&mut self, field: &Field, value: bool) {
    let _ = write!(self.0, " {}={value}", field.name());
  }

  fn record_f64(&mut self, field: &Field, value: f64) {
    let _ = write!(self.0, " {}={value}", field.name());
  }

  fn record_str(&mut self, field: &Field, value: &str) {
    self.field_text(field, value);
  }

  fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
    // The message is `format_args!`, whose Debug is its text without quotes.
    self.field_text(field, &format!("{value:?}"));
  }
}

/// Whether a field's name says it holds content or a secret.
pub fn is_secret_field(name: &str) -> bool {
  let name = name.to_ascii_lowercase();
  SECRET_FIELDS.contains(&name.as_str()) || SECRET_FIELD_SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
}

/// Whether a text field's value may be kept (redacted by shape): see [`PLAIN_FIELDS`].
pub fn is_plain_field(name: &str) -> bool {
  let name = name.to_ascii_lowercase();
  PLAIN_FIELDS.contains(&name.as_str()) || PLAIN_FIELD_SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
}

/// `text` with everything that looks like a secret, an address or a path replaced; see the module
/// docs and docs/bug-reports.md, "Redaction".
pub fn redact(text: &str) -> String {
  redact_words(&redact_spans(text))
}

/// Spans that may hold spaces, before the text is cut into words: a quoted value after a secret
/// key (`body: "meet me at …"` in a `Debug` struct) and a path (`/home/x/My Documents/a b.pdf`).
fn redact_spans(text: &str) -> String {
  let mut out = String::with_capacity(text.len());
  let mut index = 0;
  while index < text.len() {
    let rest = &text[index..];
    let previous = text[..index].chars().next_back();
    if let Some((kept, skipped)) = quoted_secret(&text[..index], rest) {
      out.push_str(kept);
      out.push_str(REDACTED);
      out.push('"');
      index += skipped;
      continue;
    }
    if starts_path(rest, previous) {
      let end = match previous {
        Some(quote @ ('"' | '\'')) => rest.find(quote).unwrap_or(rest.len()),
        _ => rest.len(),
      };
      out.push_str("[path]");
      index += end;
      continue;
    }
    // Invariant: `index` is inside `text`, so there is a next char.
    let Some(c) = rest.chars().next() else { break };
    out.push(c);
    index += c.len_utf8();
  }
  out
}

/// At an opening `"` whose key before it is secret (`body: "`, `token="`): what to write before
/// the redaction (the quote) and how many bytes the quoted value takes, closing quote included.
fn quoted_secret<'a>(before: &str, rest: &'a str) -> Option<(&'a str, usize)> {
  if !rest.starts_with('"') {
    return None;
  }
  let key_end = before.trim_end_matches([' ', ':', '=']);
  if key_end.len() == before.len() {
    return None;
  }
  let key_start = key_end
    .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
    .map_or(0, |at| at + 1);
  if !is_secret_field(&key_end[key_start..]) {
    return None;
  }
  // The closing quote, skipping escaped ones.
  let mut is_escaped = false;
  for (at, c) in rest.char_indices().skip(1) {
    match c {
      '\\' if !is_escaped => is_escaped = true,
      '"' if !is_escaped => return Some((&rest[..1], at + 1)),
      _ => is_escaped = false,
    }
  }
  Some((&rest[..1], rest.len()))
}

/// Whether a path begins here: after a separator (not inside a word or a URL).
fn starts_path(rest: &str, previous: Option<char>) -> bool {
  let is_boundary =
    previous.is_none_or(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '(' | '[' | '{' | '=' | ','));
  if !is_boundary {
    return false;
  }
  if PATH_STARTS.iter().any(|start| rest.starts_with(start)) {
    return true;
  }
  let bytes = rest.as_bytes();
  let is_windows = bytes.len() > 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\';
  let is_absolute = rest.starts_with('/')
    && rest[1..].starts_with(|c: char| c.is_alphanumeric() || c == '.' || c == '_')
    && rest[1..]
      .split(char::is_whitespace)
      .next()
      .is_some_and(|word| word.contains('/'))
    && !rest.starts_with("/api/");
  is_windows || is_absolute
}

/// The word pass: URLs, e-mail addresses, tokens and `key=value` pairs, word by word.
fn redact_words(text: &str) -> String {
  let mut out = String::with_capacity(text.len());
  let mut redacts_next = false;
  for piece in split_keep_whitespace(text) {
    if piece.chars().all(char::is_whitespace) {
      out.push_str(piece);
      continue;
    }
    if redacts_next {
      // The credentials after an authorization scheme.
      redacts_next = false;
      out.push_str(REDACTED);
      out.push_str(trailing_punctuation(piece));
      continue;
    }
    let bare = piece.trim_matches(|c: char| !c.is_alphanumeric());
    if bare.eq_ignore_ascii_case("bearer") || bare.eq_ignore_ascii_case("basic") {
      out.push_str(piece);
      redacts_next = true;
      continue;
    }
    out.push_str(&redact_word(piece));
  }
  out
}

/// One whitespace-free word, redacted.
fn redact_word(word: &str) -> String {
  if let Some(scheme_at) = word.find("://") {
    return redact_url(word, scheme_at);
  }
  let (lead, core, trail) = split_punctuation(word);
  if core.is_empty() {
    return word.to_owned();
  }
  if looks_like_email(core) {
    return format!("{lead}[email]{trail}");
  }
  if looks_like_path(core) {
    return format!("{lead}[path]{trail}");
  }
  if looks_like_jwt(core) {
    return format!("{lead}{REDACTED}{trail}");
  }
  if let Some((name, value)) = core.split_once(['=', ':'])
    && !name.is_empty()
    && !value.is_empty()
    && is_secret_field(name.trim_matches(|c: char| !c.is_alphanumeric() && c != '_'))
  {
    let separator = &core[name.len()..=name.len()];
    return format!("{lead}{name}{separator}{REDACTED}{trail}");
  }
  format!("{lead}{}{trail}", redact_secret_runs(core))
}

/// Keeps scheme, host and path; drops user info, query and fragment. A file or content URL names
/// a file on the device: its path goes too.
fn redact_url(word: &str, scheme_at: usize) -> String {
  let scheme_start = word[..scheme_at]
    .rfind(|c: char| !c.is_ascii_alphanumeric())
    .map_or(0, |at| at + 1);
  if matches!(&word[scheme_start..scheme_at], "file" | "content") {
    return format!("{}[path]", &word[..scheme_start]);
  }
  let (_, _, trail) = split_punctuation(word);
  let body = &word[..word.len() - trail.len()];
  let authority_start = scheme_at + 3;
  let mut kept = body.to_owned();
  if let Some(cut) = kept[authority_start..].find(['?', '#']) {
    kept.truncate(authority_start + cut);
    kept.push_str("?[redacted]");
  }
  // `user:password@host`: the credentials go.
  let authority_end = kept[authority_start..]
    .find('/')
    .map_or(kept.len(), |end| authority_start + end);
  if let Some(at) = kept[authority_start..authority_end].rfind('@') {
    kept.replace_range(authority_start..authority_start + at + 1, "[redacted]@");
  }
  format!("{kept}{trail}")
}

/// Replaces runs of hex or base64 long enough to be a key, hash or token. IDs are decimal and stay.
fn redact_secret_runs(word: &str) -> String {
  let mut out = String::with_capacity(word.len());
  let mut run = String::new();
  let flush = |run: &mut String, out: &mut String| {
    let has_digit = run.chars().any(|c| c.is_ascii_digit());
    let has_letter = run.chars().any(|c| c.is_ascii_alphabetic());
    if run.chars().count() >= SECRET_RUN_CHARS && has_digit && has_letter && !is_route(run) {
      out.push_str(REDACTED);
    } else {
      out.push_str(run);
    }
    run.clear();
  };
  for c in word.chars() {
    if c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-') {
      run.push(c);
    } else {
      flush(&mut run, &mut out);
      out.push(c);
    }
  }
  flush(&mut run, &mut out);
  out
}

/// `/api/v1/conversations/7346/messages`: segments of words, IDs and short tokens, as routes and
/// module paths are. Base64 with slashes has long mixed segments and isn't one.
fn is_route(run: &str) -> bool {
  run.contains('/')
    && run.split('/').all(|segment| {
      segment.len() <= 3
        || segment.chars().all(|c| c.is_ascii_digit())
        || segment
          .chars()
          .all(|c| c.is_ascii_lowercase() || c == '_' || c == '-')
    })
}

fn looks_like_email(word: &str) -> bool {
  let Some((local, domain)) = word.split_once('@') else {
    return false;
  };
  !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

fn looks_like_path(word: &str) -> bool {
  let bytes = word.as_bytes();
  let is_windows = bytes.len() > 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\';
  let is_home = word.starts_with("~/") || word.starts_with("~\\");
  // An absolute path of two segments or more; the API's own paths name IDs only.
  let is_absolute = word.starts_with('/') && word[1..].contains('/') && !word.starts_with("/api/");
  is_windows || is_home || is_absolute || word.starts_with("\\\\")
}

fn looks_like_jwt(word: &str) -> bool {
  word.starts_with("eyJ") && word.matches('.').count() == 2
}

/// Splits into words and the whitespace between them, keeping both.
fn split_keep_whitespace(text: &str) -> Vec<&str> {
  let mut pieces = Vec::new();
  let mut start = 0;
  let mut is_space = None;
  for (index, c) in text.char_indices() {
    let space = c.is_whitespace();
    if is_space.is_some_and(|previous| previous != space) {
      pieces.push(&text[start..index]);
      start = index;
    }
    is_space = Some(space);
  }
  if start < text.len() {
    pieces.push(&text[start..]);
  }
  pieces
}

/// Quotes and brackets around a word, as the word's `Debug` form or a sentence puts them there.
fn split_punctuation(word: &str) -> (&str, &str, &str) {
  let is_wrap = |c: char| {
    matches!(
      c,
      '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | '<' | '>' | '`'
    )
  };
  let core_start = word.find(|c: char| !is_wrap(c)).unwrap_or(word.len());
  let rest = &word[core_start..];
  let core_len = rest.rfind(|c: char| !is_wrap(c) && c != '.').map_or(0, |end| {
    end + rest[end..].chars().next().map_or(0, char::len_utf8)
  });
  (&word[..core_start], &rest[..core_len], &rest[core_len..])
}

fn trailing_punctuation(word: &str) -> &str {
  split_punctuation(word).2
}

fn truncate(line: String, max_chars: usize) -> String {
  match line.char_indices().nth(max_chars) {
    Some((end, _)) => format!("{}...", &line[..end]),
    None => line,
  }
}

/// Milliseconds since the Unix epoch on this device's clock (`Date.now()` in the browser, where
/// `SystemTime` isn't available).
#[cfg(not(target_arch = "wasm32"))]
fn now_ms() -> i64 {
  let since_epoch = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default();
  i64::try_from(since_epoch.as_millis()).unwrap_or(i64::MAX)
}

#[cfg(target_arch = "wasm32")]
fn now_ms() -> i64 {
  // Whole milliseconds since the epoch fit an f64 exactly.
  js_sys::Date::now() as i64
}

/// `2026-11-01T12:00:00Z` (UTC, whole seconds).
pub fn timestamp(now_ms: i64) -> String {
  let seconds = now_ms.max(0) / 1000;
  let (days, rest) = (seconds / 86_400, seconds % 86_400);
  // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
  let shifted = days + 719_468;
  let era = shifted.div_euclid(146_097);
  let day_of_era = shifted.rem_euclid(146_097);
  let year_of_era = (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
  let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
  let month_index = (5 * day_of_year + 2) / 153;
  let day = day_of_year - (153 * month_index + 2) / 5 + 1;
  let month = if month_index < 10 {
    month_index + 3
  } else {
    month_index - 9
  };
  let year = year_of_era + era * 400 + i64::from(month <= 2);
  format!(
    "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
    rest / 3_600,
    rest % 3_600 / 60,
    rest % 60
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  use tracing_subscriber::layer::SubscriberExt as _;

  const BODY: &str = "meet me at the old harbour";
  const TOKEN: &str = "eyJhbGciOiJFUzI1NiJ9.eyJzdWIiOiI0MiJ9.c2lnbmF0dXJlMTIzNDU2Nzg5MA";
  const KEY_HEX: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
  const KEY_BASE64: &str = "q83vEjRWeJASNFZ4kBI0VniQEjRWeJASNFZ4kBI0Vng=";
  const KEY_WITH_SLASHES: &str = "kBI0/VniQEjRWeJASNFZ4kBI0Vng+q83vEjRW/eJA=";

  mod ring {
    use super::*;

    #[test]
    fn keeps_only_the_newest_lines_in_order() {
      let log = DebugLog::new(DebugLogSettings {
        max_lines: 3,
        ..DebugLogSettings::default()
      });
      record(&log, || {
        for number in 0..5 {
          tracing::warn!(number, "event");
        }
      });
      let lines = log.lines();
      assert_eq!(lines.len(), 3);
      assert!(lines[0].ends_with("event number=2"), "{lines:?}");
      assert!(lines[2].ends_with("event number=4"), "{lines:?}");
    }

    #[test]
    fn cuts_overlong_lines() {
      let log = DebugLog::new(DebugLogSettings {
        max_lines: 2,
        max_line_chars: 100,
      });
      record(&log, || tracing::warn!("{}", "word ".repeat(1_000)));
      let line = &log.lines()[0];
      assert_eq!(line.chars().count(), 103);
      assert!(line.ends_with("..."));
    }

    #[test]
    fn a_ring_of_zero_lines_keeps_nothing() {
      let log = DebugLog::new(DebugLogSettings {
        max_lines: 0,
        ..DebugLogSettings::default()
      });
      record(&log, || tracing::error!("broken"));
      assert!(log.lines().is_empty());
    }

    #[test]
    fn keeps_info_of_own_crates_and_warnings_of_dependencies() {
      let log = DebugLog::new(DebugLogSettings::default());
      record(&log, || {
        tracing::info!(target: "teamcraft_core::sync", "connected");
        tracing::debug!(target: "teamcraft_core::sync", "detail");
        tracing::info!(target: "hyper::client", "chatter");
        tracing::warn!(target: "rustls::conn", "odd certificate");
      });
      let lines = log.lines();
      assert_eq!(lines.len(), 2, "{lines:?}");
      assert!(lines[0].contains("INFO teamcraft_core::sync: connected"));
      assert!(lines[1].contains("WARN rustls::conn: odd certificate"));
    }

    #[test]
    fn the_shells_filter_keeps_what_the_ring_keeps() {
      let filter = kept_targets();
      for (target, level) in [
        ("teamcraft_core::sync", Level::INFO),
        ("teamcraft_core::sync", Level::DEBUG),
        ("teamcraft_lifecycle", Level::INFO),
        ("hyper::client", Level::INFO),
        ("rustls::conn", Level::WARN),
      ] {
        assert_eq!(
          filter.would_enable(target, &level),
          DebugLog::is_kept(target, &level),
          "{target} {level}"
        );
      }
    }

    #[test]
    fn keeps_numbers_flags_and_ids() {
      let log = DebugLog::new(DebugLogSettings::default());
      record(&log, || {
        tracing::warn!(target: "teamcraft_media", conversation_id = %"7346012345678", rtt_ms = 41_u64, is_secured = true, "call degraded");
      });
      let line = &log.lines()[0];
      assert!(
        line.ends_with("call degraded conversation_id=7346012345678 rtt_ms=41 is_secured=true"),
        "{line}"
      );
    }

    #[test]
    fn formats_the_timestamp_in_utc() {
      assert_eq!(timestamp(1_793_534_400_000), "2026-11-01T12:00:00Z");
      assert_eq!(timestamp(0), "1970-01-01T00:00:00Z");
    }
  }

  /// Content and secrets that reach a log line by mistake don't reach the ring.
  mod redaction {
    use super::*;

    #[test]
    fn content_fields_are_never_kept() {
      let lines = logged(|| {
        tracing::warn!(target: "teamcraft_core", body = BODY, "send failed");
        tracing::warn!(target: "teamcraft_core", text = %BODY, "edit failed");
        tracing::warn!(target: "teamcraft_core", file_name = "salary 2026.xlsx", "upload failed");
        tracing::warn!(target: "teamcraft_core", display_name = "Alex Example", "renamed");
      });
      assert_absent(&lines, &[BODY, "salary", "Alex Example"]);
      assert!(lines[0].ends_with("send failed body=[redacted]"), "{lines:?}");
    }

    #[test]
    fn secret_fields_are_never_kept() {
      let lines = logged(|| {
        tracing::error!(target: "teamcraft_core", token = TOKEN, "refresh failed");
        tracing::error!(target: "teamcraft_core", access_token = "opaque-1234", "refresh failed");
        tracing::error!(target: "teamcraft_core", password = "hunter2", "sign-in failed");
        tracing::error!(target: "teamcraft_core", content_key = ?KEY_HEX.as_bytes(), "rotated");
        tracing::error!(target: "teamcraft_core", payload = "{\"title\":\"hi\"}", "push");
      });
      assert_absent(&lines, &[TOKEN, "opaque-1234", "hunter2", "\"hi\""]);
    }

    #[test]
    fn secrets_in_messages_are_removed() {
      let lines = logged(|| {
        tracing::warn!(target: "teamcraft_core", "token {TOKEN} expired");
        tracing::warn!(target: "teamcraft_core", "header Authorization: Bearer abc.def-ghi rejected");
        tracing::warn!(target: "teamcraft_core", "derived key {KEY_HEX}");
        tracing::warn!(target: "teamcraft_core", "backup key {KEY_BASE64} stored");
        tracing::warn!(target: "teamcraft_core", "retrying with password=hunter2&token=xyz");
        tracing::warn!(target: "teamcraft_core", "signature {KEY_WITH_SLASHES} refused");
      });
      assert_absent(
        &lines,
        &[
          TOKEN,
          "abc.def-ghi",
          KEY_HEX,
          KEY_BASE64,
          "hunter2",
          "xyz",
          "VniQEjRWeJASNFZ4kBI0Vng",
        ],
      );
    }

    #[test]
    fn addresses_lose_their_secrets_but_keep_the_host() {
      let url = "https://s3.example.com/bucket/7346?X-Amz-Signature=deadbeef&X-Amz-Credential=AKIA";
      let lines = logged(|| {
        tracing::warn!(target: "teamcraft_core", error = %format!("request to {url} failed"), "upload");
        tracing::warn!(target: "teamcraft_core", "share link https://chat.example.com/s/7346#k=secretfragment");
        tracing::warn!(target: "teamcraft_core", "proxy https://alice:pw123@proxy.example.com:8080/ refused");
      });
      assert_absent(&lines, &["deadbeef", "AKIA", "secretfragment", "alice", "pw123"]);
      assert!(
        lines[0].contains("https://s3.example.com/bucket/7346?[redacted]"),
        "{lines:?}"
      );
      assert!(
        lines[2].contains("https://[redacted]@proxy.example.com:8080/"),
        "{lines:?}"
      );
    }

    #[test]
    fn errors_formatted_with_debug_lose_their_urls_query() {
      let error = Some("https://api.example.com/x?code=1234567");
      let lines = logged(|| tracing::error!(target: "teamcraft_core", ?error, "failed"));
      assert_absent(&lines, &["1234567"]);
    }

    #[test]
    fn emails_and_paths_are_removed() {
      let lines = logged(|| {
        tracing::warn!(target: "teamcraft_core", "invite to alex@example.com bounced");
        tracing::warn!(target: "teamcraft_core", "could not read /home/alex/Documents/contract.pdf");
        tracing::warn!(target: "teamcraft_core", r"could not read C:\Users\alex\contract.pdf");
        tracing::warn!(target: "teamcraft_core", "could not read ~/notes.txt");
      });
      assert_absent(&lines, &["alex", "contract.pdf", "notes.txt"]);
      assert!(lines[0].contains("[email]"), "{lines:?}");
      assert!(lines[1].contains("[path]"), "{lines:?}");
    }

    #[test]
    fn debug_structs_lose_whole_quoted_values() {
      #[derive(Debug)]
      #[allow(dead_code)]
      struct Draft {
        body: &'static str,
        conversation_id: u64,
      }
      let draft = Draft {
        body: "meet me at the \"old\" harbour",
        conversation_id: 7,
      };
      let lines = logged(|| tracing::warn!(target: "teamcraft_core", error = ?draft, "send failed"));
      assert_absent(&lines, &["meet", "harbour", "old"]);
      assert!(lines[0].contains("conversation_id: 7"), "{lines:?}");
    }

    #[test]
    fn paths_with_spaces_go_as_a_whole() {
      let lines = logged(|| {
        tracing::warn!(target: "teamcraft_core", "could not read /home/x/My Documents/salary 2026.xlsx");
        tracing::warn!(target: "teamcraft_core", error = ?"C:\\Users\\Alex Example\\plan b.docx", "open failed");
        tracing::warn!(target: "teamcraft_core", error = %"io error at \"/data/user/0/app/files/Holiday Plans.pdf\": denied", "save failed");
      });
      assert_absent(
        &lines,
        &[
          "salary",
          "2026.xlsx",
          "Alex",
          "Example",
          "plan b",
          "Holiday",
          "Plans",
        ],
      );
      assert!(
        lines[2].ends_with("save failed error=io error at \"[path]\": denied"),
        "{lines:?}"
      );
    }

    #[test]
    fn file_and_content_urls_lose_their_path() {
      let lines = logged(|| {
        tracing::warn!(target: "teamcraft_core", "picked file:///home/alex/Pictures/beach%20day.png");
        tracing::warn!(target: "teamcraft_core", error = %"no access to content://com.android.providers.media.documents/document/image%3A12%2Fname.pdf", "pick failed");
      });
      assert_absent(&lines, &["alex", "beach", "name.pdf", "image%3A12"]);
    }

    #[test]
    fn unknown_text_fields_are_never_kept() {
      let lines = logged(|| {
        tracing::warn!(target: "teamcraft_core", topic = "salary talks", subject = "layoffs", channel = "board", "changed");
        tracing::warn!(target: "teamcraft_core", panic = "index out of range: secret plan", "panicked");
      });
      assert_absent(&lines, &["salary", "layoffs", "board", "secret plan"]);
      assert!(
        lines[0].ends_with("changed topic=[redacted] subject=[redacted] channel=[redacted]"),
        "{lines:?}"
      );
    }

    #[test]
    fn ordinary_words_ids_and_api_paths_stay() {
      let text =
        "GET /api/v1/conversations/7346012345678/messages failed: 503 service_unavailable (attempt 3)";
      assert_eq!(redact(text), text);
      assert_eq!(
        redact("teamcraft_core::client::sync_trace"),
        "teamcraft_core::client::sync_trace"
      );
    }

    #[test]
    fn field_names_are_judged_case_insensitively() {
      assert!(is_secret_field("Authorization"));
      assert!(is_secret_field("refresh_token"));
      assert!(is_secret_field("conversation_name"));
      assert!(!is_secret_field("conversation_id"));
      assert!(!is_secret_field("error"));
      assert!(!is_secret_field("error_kind"));
      assert!(is_plain_field("conversation_id"));
      assert!(is_plain_field("Error"));
      assert!(!is_plain_field("topic"));
      assert!(!is_plain_field("panic"));
    }

    fn logged(events: impl FnOnce()) -> Vec<String> {
      let log = DebugLog::new(DebugLogSettings::default());
      record(&log, events);
      log.lines()
    }
  }

  fn record(log: &DebugLog, events: impl FnOnce()) {
    let subscriber = tracing_subscriber::registry().with(log.clone());
    tracing::subscriber::with_default(subscriber, events);
  }

  fn assert_absent(lines: &[String], secrets: &[&str]) {
    for line in lines {
      for secret in secrets {
        assert!(!line.contains(secret), "{secret:?} leaked into {line:?}");
      }
    }
  }
}
