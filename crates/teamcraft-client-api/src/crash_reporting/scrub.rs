//! The last step before a report is queued (docs/crash-reports.md, "Scrubbing"): whatever a
//! careless panic message or a debug log line put into a report, content, names, file names,
//! tokens and URL queries don't leave the device.
//!
//! Defence in depth on top of security.md (nothing logs content) and the debug log ring's own
//! redaction: the same [`redact`] runs again, and double-quoted text (the values a `Debug`
//! struct or a `{:?}` prints) is removed whole, whatever it looks like.

use crate::debug_log::redact;

use super::event::{CrashEvent, StackFrame};

/// What stands in for removed quoted text.
const REDACTED: &str = "[redacted]";
/// Longest message kept, in characters.
const MAX_MESSAGE_CHARS: usize = 512;
/// Longest error type kept, in characters.
const MAX_TYPE_CHARS: usize = 128;
/// Longest function name kept, in characters.
const MAX_FUNCTION_CHARS: usize = 256;
/// Frames kept; the innermost (where it happened) are kept when there are more.
const MAX_FRAMES: usize = 100;
/// Breadcrumbs kept; the newest are kept when there are more.
pub(super) const MAX_BREADCRUMBS: usize = 50;
/// Longest breadcrumb kept, in characters.
const MAX_BREADCRUMB_CHARS: usize = 512;

/// `event` with everything that could carry content or a secret removed or cut.
pub fn scrub(event: CrashEvent) -> CrashEvent {
  let frames_to_skip = event.frames.len().saturating_sub(MAX_FRAMES);
  let breadcrumbs_to_skip = event.breadcrumbs.len().saturating_sub(MAX_BREADCRUMBS);
  CrashEvent {
    error_type: truncate(scrub_text(&event.error_type), MAX_TYPE_CHARS),
    message: truncate(scrub_text(&event.message), MAX_MESSAGE_CHARS),
    frames: event
      .frames
      .into_iter()
      .skip(frames_to_skip)
      .map(scrub_frame)
      .collect(),
    breadcrumbs: event
      .breadcrumbs
      .iter()
      .skip(breadcrumbs_to_skip)
      .map(|line| truncate(scrub_text(line), MAX_BREADCRUMB_CHARS))
      .collect(),
    ..event
  }
}

/// Free text: quoted values go whole, then the debug log's redaction by shape.
fn scrub_text(text: &str) -> String {
  redact(&remove_quoted(text))
}

fn scrub_frame(frame: StackFrame) -> StackFrame {
  StackFrame {
    maybe_function: frame
      .maybe_function
      .map(|function| truncate(redact(strip_symbol_hash(&function)), MAX_FUNCTION_CHARS)),
    maybe_file: frame.maybe_file.as_deref().and_then(source_path),
    ..frame
  }
}

/// `"…"` becomes `"[redacted]"`; an unclosed quote runs to the end.
fn remove_quoted(text: &str) -> String {
  let mut out = String::with_capacity(text.len());
  let mut rest = text;
  while let Some(open) = rest.find('"') {
    out.push_str(&rest[..=open]);
    out.push_str(REDACTED);
    let after = &rest[open + 1..];
    match closing_quote(after) {
      Some(close) => {
        out.push('"');
        rest = &after[close + 1..];
      },
      None => return out,
    }
  }
  out.push_str(rest);
  out
}

/// The closing quote, skipping escaped ones (`\"` in `Debug` output).
fn closing_quote(text: &str) -> Option<usize> {
  let mut is_escaped = false;
  for (index, c) in text.char_indices() {
    match c {
      '\\' if !is_escaped => is_escaped = true,
      '"' if !is_escaped => return Some(index),
      _ => is_escaped = false,
    }
  }
  None
}

/// A legacy mangled name's hash suffix (`::h0123456789abcdef`) carries nothing for a reader.
fn strip_symbol_hash(function: &str) -> &str {
  match function.rsplit_once("::h") {
    Some((name, hash)) if hash.len() == 16 && hash.chars().all(|c| c.is_ascii_hexdigit()) => name,
    _ => function,
  }
}

/// A source file as a path inside its crate or package; the build machine's directories (and
/// with them any user name in them) go. `None` when nothing safe is left.
fn source_path(file: &str) -> Option<String> {
  let path = file.replace('\\', "/");
  if let Some(scheme_end) = path.find("://") {
    return script_path(&path[scheme_end + 3..]);
  }
  let kept = if let Some((_, rest)) = path.split_once(".cargo/registry/src/") {
    skip_components(rest, 1)
  } else if let Some((_, rest)) = path.split_once(".cargo/git/checkouts/") {
    skip_components(rest, 2)
  } else if let Some((_, rest)) = path.split_once("/rustc/") {
    skip_components(rest, 1)
  } else if let Some((_, rest)) = path.rsplit_once("/crates/") {
    rest
  } else if is_relative(&path) {
    path.trim_start_matches("./")
  } else {
    path.rsplit('/').next().unwrap_or_default()
  };
  let kept = kept.trim_start_matches('/');
  (!kept.is_empty() && !kept.split('/').any(|part| part == "..")).then(|| kept.to_owned())
}

/// A script URL's path, without host, query and fragment.
fn script_path(after_scheme: &str) -> Option<String> {
  let path = after_scheme.split_once('/').map_or("", |(_, path)| path);
  let path = path.split(['?', '#']).next().unwrap_or_default();
  (!path.is_empty()).then(|| path.to_owned())
}

fn skip_components(path: &str, count: usize) -> &str {
  path.splitn(count + 1, '/').nth(count).unwrap_or_default()
}

fn is_relative(path: &str) -> bool {
  let bytes = path.as_bytes();
  let has_drive = bytes.len() > 1 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
  !path.starts_with('/') && !path.starts_with('~') && !has_drive
}

fn truncate(text: String, max_chars: usize) -> String {
  match text.char_indices().nth(max_chars) {
    Some((cut, _)) => format!("{}…", &text[..cut]),
    None => text,
  }
}

#[cfg(test)]
mod tests {
  use super::super::event::{CrashLevel, ErrorSource, EventId};
  use super::*;

  /// What a session with secret content could leave behind; none of it may reach a report.
  const SECRETS: &[&str] = &[
    "meet me at the harbour at nine",
    "Alice Example",
    "quarterly-figures.xlsx",
    "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl",
    "s3cr3t-pa55word",
    "X-Amz-Signature=",
    "Q2hhbm5lbCBrZXkgbWF0ZXJpYWwgaGVyZQ",
    "alice@example.com",
    "henning",
    "share-link-secret",
  ];

  mod secrets {
    use super::*;

    #[test]
    fn a_report_from_a_session_with_secrets_contains_none_of_them() {
      let event = scrub(leaky_event());
      let app = super::super::super::AppInfo {
        version: "1.0.9".to_owned(),
        platform: super::super::super::AppPlatform::Desktop,
        os_name: "Linux".to_owned(),
        maybe_os_version: None,
      };
      let envelope = event.to_envelope(
        &app,
        &super::super::super::Environment::default(),
        super::super::super::InstallationId::from_random_bytes([7; 16]),
      );
      for secret in SECRETS {
        assert!(!envelope.contains(secret), "{secret} leaked into {envelope}");
      }
      // What helps fixing it is still there.
      assert!(envelope.contains("index out of bounds"));
      assert!(envelope.contains("teamcraft-core/src/runner.rs"));
      assert!(envelope.contains("teamcraft_core::runner::send"));
      assert!(envelope.contains("conversation_id=1234"));
      assert!(envelope.contains("https://cdn.example.com/upload?[redacted]"));
    }

    #[test]
    fn quoted_values_go_whole() {
      assert_eq!(
        remove_quoted(r#"Person { display_name: "Alice Example", note: "say \"hi\"" }"#),
        r#"Person { display_name: "[redacted]", note: "[redacted]" }"#
      );
      assert_eq!(remove_quoted(r#"open "half"#), r#"open "[redacted]"#);
      assert_eq!(remove_quoted("no quotes here"), "no quotes here");
    }
  }

  mod frames {
    use super::*;

    #[test]
    fn source_paths_lose_the_build_machine() {
      assert_eq!(
        source_path("/home/henning/teamcraft/crates/teamcraft-core/src/runner.rs").as_deref(),
        Some("teamcraft-core/src/runner.rs")
      );
      assert_eq!(
        source_path(
          "/home/henning/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/serde-1.0.228/src/de.rs"
        )
        .as_deref(),
        Some("serde-1.0.228/src/de.rs")
      );
      assert_eq!(
        source_path("/rustc/90b35a6239c3d8bdabc530a6a0816f7ff89a0aaf/library/core/src/option.rs").as_deref(),
        Some("library/core/src/option.rs")
      );
      assert_eq!(
        source_path(r"C:\Users\henning\src\teamcraft\crates\teamcraft-ui\src\shell.rs").as_deref(),
        Some("teamcraft-ui/src/shell.rs")
      );
      assert_eq!(source_path("./src/main.rs").as_deref(), Some("src/main.rs"));
      assert_eq!(
        source_path("/home/henning/notes/secret.rs").as_deref(),
        Some("secret.rs")
      );
      assert_eq!(
        source_path("https://team.example.com/teamcraft-ui-1a2b.js?token=abc#x").as_deref(),
        Some("teamcraft-ui-1a2b.js")
      );
      assert_eq!(source_path("../../etc/passwd"), None);
      assert_eq!(source_path(""), None);
    }

    #[test]
    fn symbol_hashes_go() {
      assert_eq!(
        strip_symbol_hash("teamcraft_core::runner::send::h0123456789abcdef"),
        "teamcraft_core::runner::send"
      );
      assert_eq!(
        strip_symbol_hash("teamcraft_core::hash::h1"),
        "teamcraft_core::hash::h1"
      );
    }

    #[test]
    fn keeps_the_innermost_frames_and_newest_breadcrumbs() {
      let mut event = leaky_event();
      event.frames = (0..150)
        .map(|index| StackFrame {
          maybe_function: Some(format!("f{index}")),
          ..StackFrame::default()
        })
        .collect();
      event.breadcrumbs = (0..80).map(|index| format!("line {index}")).collect();
      let event = scrub(event);
      assert_eq!(event.frames.len(), MAX_FRAMES);
      assert_eq!(
        event.frames.last().unwrap().maybe_function.as_deref(),
        Some("f149")
      );
      assert_eq!(event.breadcrumbs.len(), MAX_BREADCRUMBS);
      assert_eq!(event.breadcrumbs.last().unwrap(), "line 79");
    }

    #[test]
    fn long_messages_are_cut() {
      let mut event = leaky_event();
      event.message = "x".repeat(2_000);
      assert_eq!(scrub(event).message.chars().count(), MAX_MESSAGE_CHARS + 1);
    }
  }

  fn leaky_event() -> CrashEvent {
    CrashEvent {
      event_id: EventId::from_random_bytes([3; 16]),
      timestamp_ms: 1_700_000_000_000,
      level: CrashLevel::Fatal,
      source: ErrorSource::Rust,
      error_type: "panic".to_owned(),
      message: concat!(
        r#"index out of bounds while sending Message { body: "meet me at the harbour at nine", "#,
        r#"author: Person { display_name: "Alice Example" }, file: "quarterly-figures.xlsx" } "#,
        "with Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl password=s3cr3t-pa55word ",
        "key Q2hhbm5lbCBrZXkgbWF0ZXJpYWwgaGVyZQ for alice@example.com from ",
        "/home/henning/Documents/quarterly-figures.xlsx",
      )
      .to_owned(),
      frames: vec![StackFrame {
        maybe_function: Some("teamcraft_core::runner::send::h0123456789abcdef".to_owned()),
        maybe_file: Some("/home/henning/teamcraft/crates/teamcraft-core/src/runner.rs".to_owned()),
        maybe_line: Some(42),
        maybe_column: Some(7),
        is_in_app: true,
      }],
      breadcrumbs: vec![
        "2026-11-08T10:00:00Z INFO teamcraft_core: upload failed conversation_id=1234 \
         url=https://cdn.example.com/upload?X-Amz-Signature=abc"
          .to_owned(),
        r#"2026-11-08T10:00:01Z WARN teamcraft_core: draft body="meet me at the harbour at nine""#.to_owned(),
        "2026-11-08T10:00:02Z INFO teamcraft_core: opened https://team.example.com/s/1#share-link-secret"
          .to_owned(),
      ],
    }
  }
}
