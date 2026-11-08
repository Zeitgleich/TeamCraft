//! A Rust panic as a report: the shells' panic hooks hand over what the hook saw.

use std::panic::PanicHookInfo;

use super::event::{CrashEvent, CrashLevel, ErrorSource, EventId, StackFrame};

/// Prefix of TeamCraft's own crates in a function path: those frames are "in app".
const OWN_CRATE_PREFIX: &str = "teamcraft";
/// Functions of the panic machinery itself; frames up to the last of them are dropped.
const PANIC_MACHINERY: &[&str] = &[
  "rust_begin_unwind",
  "std::panicking::",
  "core::panicking::",
  "std::panic::",
  "std::backtrace::",
  "std::backtrace_rs::",
  "<alloc::boxed::Box<F,A> as core::ops::function::Fn",
];

/// What a panic hook knows, taken apart so it can be built in tests.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PanicParts {
  pub message: String,
  pub maybe_location: Option<(String, u32, u32)>,
  /// `std::backtrace::Backtrace`'s `Display` text; empty where backtraces aren't captured (wasm).
  pub backtrace: String,
}

impl PanicParts {
  /// From inside a panic hook; `backtrace` is `Backtrace::force_capture().to_string()` or empty.
  pub fn from_hook(info: &PanicHookInfo<'_>, backtrace: String) -> Self {
    let payload = info.payload();
    let message = payload
      .downcast_ref::<&str>()
      .map(|text| (*text).to_owned())
      .or_else(|| payload.downcast_ref::<String>().cloned())
      .unwrap_or_else(|| "panic with a non-text payload".to_owned());
    Self {
      message,
      maybe_location: info
        .location()
        .map(|location| (location.file().to_owned(), location.line(), location.column())),
      backtrace,
    }
  }

  /// The unscrubbed report; [`super::CrashReporter::capture`] scrubs it.
  pub fn into_event(self, event_id: EventId, timestamp_ms: i64, breadcrumbs: Vec<String>) -> CrashEvent {
    let mut frames = frames_from_backtrace(&self.backtrace);
    if frames.is_empty()
      && let Some((file, line, column)) = self.maybe_location
    {
      frames.push(StackFrame {
        maybe_function: None,
        is_in_app: file.contains("teamcraft"),
        maybe_file: Some(file),
        maybe_line: Some(line),
        maybe_column: Some(column),
      });
    }
    CrashEvent {
      event_id,
      timestamp_ms,
      level: CrashLevel::Fatal,
      source: ErrorSource::Rust,
      error_type: "panic".to_owned(),
      message: self.message,
      frames,
      breadcrumbs,
    }
  }
}

/// Frames from `std::backtrace::Backtrace`'s text, oldest call first, without the panic machinery.
///
/// ```text
///    3: teamcraft_core::runner::send
///              at ./crates/teamcraft-core/src/runner.rs:42:7
/// ```
pub fn frames_from_backtrace(text: &str) -> Vec<StackFrame> {
  let mut frames: Vec<StackFrame> = Vec::new();
  for line in text.lines().map(str::trim) {
    if let Some(location) = line.strip_prefix("at ") {
      if let Some(frame) = frames.last_mut() {
        apply_location(frame, location);
      }
    } else if let Some((index, function)) = line.split_once(": ")
      && !index.is_empty()
      && index.chars().all(|c| c.is_ascii_digit())
    {
      frames.push(StackFrame {
        is_in_app: function.starts_with(OWN_CRATE_PREFIX)
          || function.starts_with(&format!("<{OWN_CRATE_PREFIX}")),
        maybe_function: Some(function.to_owned()),
        ..StackFrame::default()
      });
    }
  }
  let machinery_end = frames
    .iter()
    .rposition(|frame| {
      frame
        .maybe_function
        .as_deref()
        .is_some_and(|function| PANIC_MACHINERY.iter().any(|prefix| function.starts_with(prefix)))
    })
    .map_or(0, |index| index + 1);
  let mut frames = frames.split_off(machinery_end);
  frames.reverse();
  frames
}

/// `path:line:column` (the path may hold `:` on Windows).
fn apply_location(frame: &mut StackFrame, location: &str) {
  let mut parts = location.rsplitn(3, ':');
  let maybe_column = parts.next().and_then(|part| part.parse().ok());
  let maybe_line = parts.next().and_then(|part| part.parse().ok());
  match (maybe_line, parts.next()) {
    (Some(line), Some(file)) => {
      frame.maybe_file = Some(file.to_owned());
      frame.maybe_line = Some(line);
      frame.maybe_column = maybe_column;
    },
    _ => frame.maybe_file = Some(location.to_owned()),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const BACKTRACE: &str = "   0: std::backtrace_rs::backtrace::libunwind::trace
             at /rustc/abc/library/std/src/../../backtrace/src/backtrace/libunwind.rs:116:5
   1: std::panicking::begin_panic_handler::{{closure}}
             at /rustc/abc/library/std/src/panicking.rs:697:13
   2: core::panicking::panic_bounds_check
             at /rustc/abc/library/core/src/panicking.rs:280:5
   3: teamcraft_core::runner::send
             at ./crates/teamcraft-core/src/runner.rs:42:7
   4: <teamcraft_core::runner::Runner as core::future::Future>::poll
             at ./crates/teamcraft-core/src/runner.rs:10:3
   5: tokio::runtime::task::harness::poll_future
             at /home/u/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.48.0/src/runtime/task/harness.rs:473:19
   6: __libc_start_main
";

  #[test]
  fn reads_frames_oldest_first_without_the_panic_machinery() {
    let frames = frames_from_backtrace(BACKTRACE);
    let functions: Vec<_> = frames
      .iter()
      .map(|frame| frame.maybe_function.as_deref().unwrap())
      .collect();
    assert_eq!(
      functions,
      [
        "__libc_start_main",
        "tokio::runtime::task::harness::poll_future",
        "<teamcraft_core::runner::Runner as core::future::Future>::poll",
        "teamcraft_core::runner::send",
      ]
    );
    let send = frames.last().unwrap();
    assert!(send.is_in_app);
    assert_eq!(
      send.maybe_file.as_deref(),
      Some("./crates/teamcraft-core/src/runner.rs")
    );
    assert_eq!((send.maybe_line, send.maybe_column), (Some(42), Some(7)));
    assert!(frames[2].is_in_app);
    assert!(!frames[1].is_in_app);
    assert_eq!(frames[0].maybe_file, None);
  }

  #[test]
  fn reads_windows_paths() {
    let frames =
      frames_from_backtrace("  0: teamcraft_desktop::main\n      at C:\\b\\crates\\x\\src\\main.rs:3:9\n");
    assert_eq!(
      frames[0].maybe_file.as_deref(),
      Some("C:\\b\\crates\\x\\src\\main.rs")
    );
    assert_eq!(frames[0].maybe_line, Some(3));
  }

  #[test]
  fn falls_back_to_the_location_without_a_backtrace() {
    let parts = PanicParts {
      message: "boom".to_owned(),
      maybe_location: Some(("crates/teamcraft-ui/src/shell.rs".to_owned(), 5, 1)),
      backtrace: String::new(),
    };
    let event = parts.into_event(EventId::from_random_bytes([0; 16]), 0, Vec::new());
    assert_eq!(event.level, CrashLevel::Fatal);
    assert_eq!(event.frames.len(), 1);
    assert_eq!(event.frames[0].maybe_line, Some(5));
    assert!(event.frames[0].is_in_app);
  }

  #[test]
  fn reads_a_real_panic() {
    let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
    let sink = captured.clone();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
      // Other tests may panic on their threads meanwhile: keep only this one's.
      let parts = PanicParts::from_hook(info, String::new());
      if parts.message == "expected 1" {
        *sink.lock().unwrap() = Some(parts);
      }
    }));
    let _ = std::panic::catch_unwind(|| panic!("expected {}", 1));
    std::panic::set_hook(previous);
    let parts = captured.lock().unwrap().take().unwrap();
    assert_eq!(parts.message, "expected 1");
    assert!(parts.maybe_location.unwrap().0.ends_with("panic.rs"));
  }
}
