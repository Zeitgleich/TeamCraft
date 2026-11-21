//! The last step before a report is queued (docs/crash-reports.md, "Scrubbing"): whatever a
//! careless panic message or a debug log line put into a report, content, names, file names,
//! tokens and URL queries don't leave the device.
//!
//! Defence in depth on top of security.md (nothing logs content) and the debug log ring's own
//! redaction: the same [`redact`] runs again, and double-quoted text (the values a `Debug`
//! struct or a `{:?}` prints) is removed whole, unless it is plainly code: a source file of the
//! app's tree (`"crates/teamcraft-ui/src/state.rs"`) or a Rust type path
//! (`"teamcraft_ui::state::AppState"`), which a panic of `Location` or `expect_context` quotes.

use crate::debug_log::redact;

use super::call::CallEventDetail;
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
    maybe_call: event.maybe_call.map(|call| {
      Box::new(CallEventDetail {
        report: call.report.scrubbed(),
        ..*call
      })
    }),
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

/// `"…"` becomes `"[redacted]"`, unless it is code ([`code_value`]); an unclosed quote runs to the
/// end.
fn remove_quoted(text: &str) -> String {
  let mut out = String::with_capacity(text.len());
  let mut rest = text;
  while let Some(open) = rest.find('"') {
    out.push_str(&rest[..=open]);
    let after = &rest[open + 1..];
    match closing_quote(after) {
      Some(close) => {
        out.push_str(code_value(&after[..close]).as_deref().unwrap_or(REDACTED));
        out.push('"');
        rest = &after[close + 1..];
      },
      None => {
        out.push_str(REDACTED);
        return out;
      },
    }
  }
  out.push_str(rest);
  out
}

/// A quoted value that is plainly code and kept: a relative path to a Rust source file (as
/// [`source_path`] keeps a frame's) or a Rust type path. Anything else, `None`.
fn code_value(quoted: &str) -> Option<String> {
  if is_relative_source_file(quoted) {
    return source_path(quoted);
  }
  is_type_path(quoted).then(|| quoted.to_owned())
}

/// `crates/teamcraft-ui/src/state.rs`, `src/main.rs`: relative, a `.rs` file inside a source tree
/// (under `crates/` or `src/`, or with a `src/` directory), only the characters of the tree's
/// paths, no step up. A bare or other file name (`budget.rs`, `notes/plan.rs`) may be the user's.
fn is_relative_source_file(text: &str) -> bool {
  let path = text.trim_start_matches("./");
  let is_in_tree = path.starts_with("crates/") || path.starts_with("src/") || path.contains("/src/");
  text.ends_with(".rs")
    && is_in_tree
    && is_relative(text)
    && text
      .chars()
      .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))
    && !text.split('/').any(|part| part == ".." || part.is_empty())
}

/// `teamcraft_ui::state::AppState`, `alloc::vec::Vec<teamcraft_proto::UserId>`: paths of at
/// least two identifiers, with generic arguments of the same shape; no spaces, nothing else.
fn is_type_path(text: &str) -> bool {
  if !text.contains("::")
    || !text
      .chars()
      .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '<' | '>' | ','))
  {
    return false;
  }
  let mut depth = 0_u32;
  for c in text.chars() {
    match c {
      '<' => depth += 1,
      '>' if depth == 0 => return false,
      '>' => depth -= 1,
      _ => {},
    }
  }
  depth == 0
    && text
      .split(['<', '>', ','])
      .filter(|part| !part.is_empty())
      .all(|path| path.split("::").all(is_identifier))
}

fn is_identifier(part: &str) -> bool {
  let mut chars = part.chars();
  chars
    .next()
    .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
    && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
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

/// A legacy mangled name's hash suffix (`::h0123456789abcdef`), or that of a wasm-bindgen import
/// in the page's glue (`__wbg_new_8a6f238a6ece86ea`), carries nothing for a reader; left on, the
/// redaction takes the hex run for a secret and the whole name with it.
fn strip_symbol_hash(function: &str) -> &str {
  let is_hash = |hash: &str| hash.len() == 16 && hash.chars().all(|c| c.is_ascii_hexdigit());
  match (function.rsplit_once("::h"), function.rsplit_once('_')) {
    (Some((name, hash)), _) if is_hash(hash) => name,
    (_, Some((name, hash))) if is_hash(hash) => name,
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

/// `text` with every run of six or more digits (device, call and user IDs) as `[id]`, so the same
/// failure on different devices or calls groups into one issue.
pub fn without_ids(text: &str) -> String {
  let mut out = String::with_capacity(text.len());
  let mut digits = String::new();
  for c in text.chars() {
    if c.is_ascii_digit() {
      digits.push(c);
      continue;
    }
    push_digits(&mut out, &mut digits);
    out.push(c);
  }
  push_digits(&mut out, &mut digits);
  out
}

fn push_digits(out: &mut String, digits: &mut String) {
  if digits.len() >= 6 {
    out.push_str("[id]");
  } else {
    out.push_str(digits);
  }
  digits.clear();
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

  #[test]
  fn ids_go_short_numbers_stay() {
    assert_eq!(
      without_ids("call key: no verified certificate for device 113934831536046080"),
      "call key: no verified certificate for device [id]"
    );
    assert_eq!(
      without_ids("epoch 12 of 99999 at 1080p"),
      "epoch 12 of 99999 at 1080p"
    );
    assert_eq!(without_ids("123456"), "[id]");
    assert_eq!(without_ids(""), "");
  }

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

    /// TEAMCRAFT-3S end to end: a desktop page's panic as the hook keeps it, with the shell's log
    /// added on the next start, scrubbed into the envelope that is sent.
    #[test]
    fn a_desktop_page_panic_reaches_the_envelope_readable() {
      use super::super::super::panic::{PanicParts, PendingPanic, ScriptStack};
      let mut pending = PendingPanic {
        parts: PanicParts {
          message: concat!(
            r#"Location { file: "crates/teamcraft-ui/src/state.rs", line: 2435, column: 3 } "#,
            r#"expected context of type "teamcraft_ui::state::AppState" to be present "#,
            r#"while drafting "meet me at the harbour at nine""#,
          )
          .to_owned(),
          maybe_location: Some((
            "/home/runner/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/reactive_graph-0.2.16/src/owner/context.rs"
              .to_owned(),
            306,
            9,
          )),
          backtrace: String::new(),
          maybe_script_stack: Some(ScriptStack {
            text: concat!(
              "Error\n",
              "    at imports.wbg.__wbg_new_8a6f238a6ece86ea (tauri://localhost/teamcraft-ui-1.js:3890:14)\n",
              "<?>.wasm-function[17228]@[wasm code]\n",
              "    at teamcraft_ui.wasm.wasm-function[1185] (tauri://localhost/teamcraft-ui-1_bg.wasm:wasm-function[1185]:0x1234)",
            )
            .to_owned(),
            origin: "tauri://localhost".to_owned(),
          }),
        },
        timestamp_ms: 1_795_263_514_000,
        breadcrumbs: vec!["2026-11-21T12:18:33Z  INFO teamcraft_ui: composer opened conversation_id=42".to_owned()],
      };
      pending.add_log_until_panic(vec![
        "2026-11-21T12:18:32Z  INFO teamcraft_media::audio_device: audio backend backend=PipeWire".to_owned(),
        "2026-11-21T12:18:33Z  WARN teamcraft_core: draft for Alice Example".to_owned(),
      ]);
      let event = scrub(pending.parts.into_event(
        EventId::from_random_bytes([3; 16]),
        pending.timestamp_ms,
        pending.breadcrumbs,
      ));
      let app = super::super::super::AppInfo {
        version: "1.0.42".to_owned(),
        platform: super::super::super::AppPlatform::Desktop,
        os_name: "Linux".to_owned(),
        maybe_os_version: None,
      };
      let envelope = event.to_envelope(
        &app,
        &super::super::super::Environment::default(),
        super::super::super::InstallationId::from_random_bytes([7; 16]),
      );
      // Where, what, the calls that led there and what the shell did before.
      assert!(
        envelope.contains(r#"file: \"crates/teamcraft-ui/src/state.rs\""#),
        "{envelope}"
      );
      assert!(
        envelope.contains(r#"\"teamcraft_ui::state::AppState\""#),
        "{envelope}"
      );
      assert!(envelope.contains("reactive_graph-0.2.16/src/owner/context.rs"));
      assert!(envelope.contains("teamcraft_ui.wasm.wasm-function[1185]"));
      assert!(envelope.contains("imports.wbg.__wbg_new"));
      assert!(envelope.contains("teamcraft_media::audio_device: audio backend"));
      assert!(envelope.contains("composer opened"));
      assert_eq!(event.frames.len(), 4);
      assert_eq!(event.breadcrumbs.len(), 3);
      // And still nothing of the user's.
      assert!(!envelope.contains("harbour"));
      assert!(!envelope.contains("runner"));
    }

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

    /// TEAMCRAFT-3S: the panic of a missing context said where and what, both blanked.
    #[test]
    fn source_files_and_type_paths_stay() {
      assert_eq!(
        remove_quoted(concat!(
          r#"Location { file: "crates/teamcraft-ui/src/state.rs", line: 2435, column: 3 } "#,
          r#"expected context of type "teamcraft_ui::state::AppState" to be present"#
        )),
        concat!(
          r#"Location { file: "crates/teamcraft-ui/src/state.rs", line: 2435, column: 3 } "#,
          r#"expected context of type "teamcraft_ui::state::AppState" to be present"#
        )
      );
      assert_eq!(
        remove_quoted(r#"file: "./src/main.rs""#),
        r#"file: "src/main.rs""#
      );
      for kept in [
        "alloc::vec::Vec<teamcraft_proto::UserId>",
        "core::option::Option<alloc::rc::Rc<dyn_trait::X>>",
        "std::collections::HashMap<a::K,b::V>",
        "_private::Thing",
      ] {
        assert_eq!(remove_quoted(&format!(r#""{kept}""#)), format!(r#""{kept}""#));
      }
    }

    #[test]
    fn everything_else_quoted_still_goes() {
      for redacted in [
        // Names, messages, files of the user.
        "Alice Example",
        "Alice",
        "meet me at the harbour at nine",
        "quarterly-figures.xlsx",
        "notes.rs is where I keep it",
        "budget.rs",
        "notes/plan.rs",
        "./budget.rs",
        "my-src/plan.rs",
        // Paths outside the source tree, or that leave it.
        "/home/henning/teamcraft/crates/teamcraft-ui/src/state.rs",
        "~/secret.rs",
        r"C:\Users\alice\src\main.rs",
        "C:/Users/alice/main.rs",
        "../../home/alice/secret.rs",
        "crates//x.rs",
        "crates/my notes.rs",
        // URLs, mail addresses, colons that are no path.
        "https://team.example.com/s/1#share-link-secret",
        "alice@example.com",
        "a:b",
        "Alice::",
        "::Alice",
        "a::b c",
        "a::b<c",
        "a::b>c<",
        "a::1b",
        "a::b-c",
        "a::b\"x",
        "",
      ] {
        let quoted = format!("v: {redacted:?}");
        assert_eq!(
          remove_quoted(&quoted),
          r#"v: "[redacted]""#,
          "{redacted} was kept"
        );
      }
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
      assert_eq!(
        strip_symbol_hash("imports.wbg.__wbg_new_8a6f238a6ece86ea"),
        "imports.wbg.__wbg_new"
      );
      assert_eq!(strip_symbol_hash("handle_error"), "handle_error");
      let frame = scrub_frame(StackFrame {
        maybe_function: Some("imports.wbg.__wbg_new_8a6f238a6ece86ea".to_owned()),
        ..StackFrame::default()
      });
      assert_eq!(frame.maybe_function.as_deref(), Some("imports.wbg.__wbg_new"));
      let frame = scrub_frame(StackFrame {
        maybe_function: Some("teamcraft_ui.wasm.wasm-function[1185]".to_owned()),
        ..StackFrame::default()
      });
      assert_eq!(
        frame.maybe_function.as_deref(),
        Some("teamcraft_ui.wasm.wasm-function[1185]")
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
      maybe_call: None,
    }
  }
}
