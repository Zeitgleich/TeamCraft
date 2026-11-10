//! A crash or error report and its envelope (the Sentry event payload, reduced to what
//! docs/crash-reports.md lists).

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::Environment;

/// Product name in the release (`teamcraft@1.0.9`), so all apps of a version group together.
pub(super) const RELEASE_PREFIX: &str = "teamcraft@";

/// One report, before scrubbing. Only the fields listed here exist: there is no user, no device
/// name, no host name, no request and no free-form context a caller could fill with content.
#[derive(Clone, Debug, PartialEq)]
pub struct CrashEvent {
  pub event_id: EventId,
  pub timestamp_ms: i64,
  pub level: CrashLevel,
  pub source: ErrorSource,
  /// The error's type (`panic`, `TypeError`, an error enum's variant).
  pub error_type: String,
  /// The panic or error message; scrubbed before it is queued.
  pub message: String,
  /// Oldest call first, as the protocol wants them.
  pub frames: Vec<StackFrame>,
  /// Recent lines of the redacted debug log ring, oldest first.
  pub breadcrumbs: Vec<String>,
}

/// How bad it is: fatal reports are always sent, the others are sampled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrashLevel {
  /// The process or page died (a panic, an uncaught exception).
  Fatal,
  /// A handled error worth seeing (a failed send after retries, a store migration failure).
  Error,
  Warning,
}

/// What raised it; decides the protocol's `platform` and how frames are read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorSource {
  Rust,
  JavaScript,
}

/// One stack frame.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StackFrame {
  pub maybe_function: Option<String>,
  /// The source file, relative to its crate or package (scrubbing removes everything above it).
  pub maybe_file: Option<String>,
  pub maybe_line: Option<u32>,
  pub maybe_column: Option<u32>,
  /// Whether the frame is TeamCraft's own code (not std or a dependency).
  pub is_in_app: bool,
}

/// The app that sends: the same for every report of a process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppInfo {
  /// The app version (`1.0.9`); the release is `teamcraft@<version>`.
  pub version: String,
  pub platform: AppPlatform,
  /// The OS name as the platform names it (`Linux`, `Windows`, `Android`).
  pub os_name: String,
  pub maybe_os_version: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppPlatform {
  Desktop,
  Web,
  Android,
  Ios,
  Server,
}

/// A report's ID: 16 random bytes, written as 32 hex digits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct EventId([u8; 16]);

/// A random ID per installation, the only identifier a report carries: it counts how many
/// installations a crash hits without naming anyone. Rotated when the user turns reports off and
/// on again; never derived from the user, the device or the account.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct InstallationId([u8; 16]);

impl CrashEvent {
  /// The envelope (header, item header, payload) for this event; `self` must be scrubbed.
  pub(super) fn to_envelope(
    &self,
    app: &AppInfo,
    environment: &Environment,
    installation: InstallationId,
  ) -> String {
    let payload = self.payload(app, environment, installation).to_string();
    let header = json!({ "event_id": self.event_id.to_string() });
    let item = json!({ "type": "event", "length": payload.len() });
    format!("{header}\n{item}\n{payload}\n")
  }

  fn payload(&self, app: &AppInfo, environment: &Environment, installation: InstallationId) -> Value {
    let timestamp = seconds(self.timestamp_ms);
    let mut tags = BTreeMap::new();
    tags.insert("app.platform", app.platform.as_str());
    let mut os = json!({ "name": app.os_name });
    if let Some(version) = &app.maybe_os_version {
      os["version"] = json!(version);
    }
    json!({
      "event_id": self.event_id.to_string(),
      "timestamp": timestamp,
      "platform": self.source.protocol_platform(),
      "level": self.level,
      "logger": "teamcraft",
      "release": format!("{RELEASE_PREFIX}{}", app.version),
      "environment": environment.as_str(),
      "tags": tags,
      "user": { "id": installation.to_string() },
      "contexts": {
        "os": os,
        "app": { "app_version": app.version },
      },
      "exception": { "values": [{
        "type": self.error_type,
        "value": self.message,
        "mechanism": { "type": self.source.mechanism(), "handled": self.level != CrashLevel::Fatal },
        "stacktrace": { "frames": self.frames.iter().map(StackFrame::to_json).collect::<Vec<_>>() },
      }]},
      "breadcrumbs": { "values": self.breadcrumbs.iter().map(|line| json!({
        "timestamp": timestamp,
        "category": "log",
        "message": line,
      })).collect::<Vec<_>>() },
    })
  }
}

impl ErrorSource {
  fn protocol_platform(self) -> &'static str {
    match self {
      Self::Rust => "native",
      Self::JavaScript => "javascript",
    }
  }

  fn mechanism(self) -> &'static str {
    match self {
      Self::Rust => "panic",
      Self::JavaScript => "onerror",
    }
  }
}

impl StackFrame {
  /// The frame of an uncaught error event of a page (`window.onerror`): the script position it
  /// names, if any. An error the browser raises itself ("ResizeObserver loop completed with
  /// undelivered notifications") has no line, and its file is the page's address. That is where the
  /// reader was (`dm/<id>`), not where the error came from: as a frame it split one error into an
  /// issue per conversation (crash-reports.md, "Scrubbing").
  pub fn from_error_event(file: &str, line: u32, column: u32) -> Option<Self> {
    (line > 0).then(|| Self {
      maybe_function: None,
      maybe_file: Some(file.to_owned()).filter(|file| !file.is_empty()),
      maybe_line: Some(line),
      maybe_column: Some(column).filter(|column| *column > 0),
      is_in_app: true,
    })
  }

  fn to_json(&self) -> Value {
    let mut frame = json!({ "in_app": self.is_in_app });
    if let Some(function) = &self.maybe_function {
      frame["function"] = json!(function);
    }
    if let Some(file) = &self.maybe_file {
      frame["filename"] = json!(file);
    }
    if let Some(line) = self.maybe_line {
      frame["lineno"] = json!(line);
    }
    if let Some(column) = self.maybe_column {
      frame["colno"] = json!(column);
    }
    frame
  }
}

impl AppPlatform {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Desktop => "desktop",
      Self::Web => "web",
      Self::Android => "android",
      Self::Ios => "ios",
      Self::Server => "server",
    }
  }
}

impl EventId {
  /// From 16 bytes of the platform's secure random source.
  pub fn from_random_bytes(bytes: [u8; 16]) -> Self {
    Self(bytes)
  }
}

impl InstallationId {
  /// From 16 bytes of the platform's secure random source.
  pub fn from_random_bytes(bytes: [u8; 16]) -> Self {
    Self(bytes)
  }
}

macro_rules! hex_id {
  ($name:ident) => {
    impl fmt::Display for $name {
      fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex(&self.0))
      }
    }

    impl TryFrom<String> for $name {
      type Error = &'static str;

      fn try_from(text: String) -> Result<Self, Self::Error> {
        parse_hex16(&text).map(Self).ok_or("expected 32 hex digits")
      }
    }

    impl From<$name> for String {
      fn from(id: $name) -> Self {
        id.to_string()
      }
    }
  };
}

hex_id!(EventId);
hex_id!(InstallationId);

/// Lowercase hex digits of `bytes`.
pub(super) fn hex(bytes: &[u8]) -> String {
  use fmt::Write;
  bytes
    .iter()
    .fold(String::with_capacity(bytes.len() * 2), |mut text, byte| {
      // Writing to a String can't fail.
      let _ = write!(text, "{byte:02x}");
      text
    })
}

fn parse_hex16(text: &str) -> Option<[u8; 16]> {
  if text.len() != 32 || !text.is_ascii() {
    return None;
  }
  let mut bytes = [0; 16];
  for (index, byte) in bytes.iter_mut().enumerate() {
    *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
  }
  Some(bytes)
}

/// Unix time in seconds with milliseconds, as the protocol takes it.
fn seconds(timestamp_ms: i64) -> f64 {
  // Precision: an i64 of milliseconds fits an f64 exactly until the year 287396.
  seconds_f64(timestamp_ms as f64)
}

/// Unix time in seconds from milliseconds with a fraction (performance data).
pub(super) fn seconds_f64(timestamp_ms: f64) -> f64 {
  timestamp_ms / 1000.0
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn an_error_event_without_a_line_has_no_frame() {
    // WebKit's ResizeObserver loop error names the page, not a script.
    assert_eq!(
      StackFrame::from_error_event("https://chat.example.com/dm/123456789", 0, 0),
      None
    );
    assert_eq!(StackFrame::from_error_event("", 0, 0), None);
  }

  #[test]
  fn an_error_event_of_a_script_keeps_its_position() {
    assert_eq!(
      StackFrame::from_error_event("https://chat.example.com/app.js", 12, 0),
      Some(StackFrame {
        maybe_function: None,
        maybe_file: Some("https://chat.example.com/app.js".to_owned()),
        maybe_line: Some(12),
        maybe_column: None,
        is_in_app: true,
      })
    );
  }

  #[test]
  fn ids_are_32_hex_digits_and_round_trip() {
    let id = EventId::from_random_bytes([0xab; 16]);
    assert_eq!(id.to_string(), "ab".repeat(16));
    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(serde_json::from_str::<EventId>(&json).unwrap(), id);
    assert!(serde_json::from_str::<InstallationId>("\"xyz\"").is_err());
  }

  #[test]
  fn envelope_has_header_item_and_payload() {
    let event = sample_event();
    let envelope = event.to_envelope(
      &sample_app(),
      &Environment::default(),
      InstallationId::from_random_bytes([1; 16]),
    );
    let lines: Vec<&str> = envelope.lines().collect();
    assert_eq!(lines.len(), 3);
    let header: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(header["event_id"], json!("00".repeat(16)));
    let item: Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(item["type"], json!("event"));
    assert_eq!(item["length"], json!(lines[2].len()));
    let payload: Value = serde_json::from_str(lines[2]).unwrap();
    assert_eq!(payload["release"], json!("teamcraft@1.0.9"));
    assert_eq!(payload["platform"], json!("native"));
    assert_eq!(payload["level"], json!("fatal"));
    assert_eq!(payload["user"], json!({ "id": "01".repeat(16) }));
    assert_eq!(payload["tags"]["app.platform"], json!("desktop"));
    assert_eq!(
      payload["exception"]["values"][0]["mechanism"]["handled"],
      json!(false)
    );
    assert_eq!(
      payload["exception"]["values"][0]["stacktrace"]["frames"][0],
      json!({ "function": "teamcraft_core::run", "filename": "teamcraft-core/src/runner.rs", "lineno": 12, "in_app": true })
    );
    assert_eq!(payload["timestamp"], json!(1_700_000_000.5));
    // Nothing a report must never carry has a field at all.
    for field in ["server_name", "request", "extra", "modules"] {
      assert!(payload.get(field).is_none(), "{field}");
    }
  }

  fn sample_event() -> CrashEvent {
    CrashEvent {
      event_id: EventId::from_random_bytes([0; 16]),
      timestamp_ms: 1_700_000_000_500,
      level: CrashLevel::Fatal,
      source: ErrorSource::Rust,
      error_type: "panic".to_owned(),
      message: "called `Option::unwrap()` on a `None` value".to_owned(),
      frames: vec![StackFrame {
        maybe_function: Some("teamcraft_core::run".to_owned()),
        maybe_file: Some("teamcraft-core/src/runner.rs".to_owned()),
        maybe_line: Some(12),
        maybe_column: None,
        is_in_app: true,
      }],
      breadcrumbs: vec!["2026-11-08T10:00:00Z INFO teamcraft_core: connected".to_owned()],
    }
  }

  fn sample_app() -> AppInfo {
    AppInfo {
      version: "1.0.9".to_owned(),
      platform: AppPlatform::Desktop,
      os_name: "Linux".to_owned(),
      maybe_os_version: None,
    }
  }
}
