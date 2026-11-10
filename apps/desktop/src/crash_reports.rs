//! Panics of the desktop shell (the core, calls, the shell's own code) for the crash reports of
//! docs/crash-reports.md. The hook only writes them to a small file in the data directory: the
//! page reports them on its next start, with the instance's setting and the user's choice, which
//! the shell doesn't know. A process that panics aborts right after the hook, so the file is
//! written synchronously.

use std::backtrace::Backtrace;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use teamcraft_client_api::crash_reporting::trace::{self, TraceSettings, WireSpan, WireTraceTree};
use teamcraft_client_api::crash_reporting::{MAX_PENDING_PANICS, PanicParts, PendingPanic, SampleRate};
use teamcraft_client_api::debug_log;

const PENDING_FILE: &str = "crash-reports-pending.json";

static PENDING_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Records panics from now on (written once the data directory is known), after the previous hook.
pub fn install_hook() {
  let previous = std::panic::take_hook();
  std::panic::set_hook(Box::new(move |info| {
    if let Some(path) = PENDING_PATH.get() {
      let pending = PendingPanic {
        parts: PanicParts::from_hook(info, Backtrace::force_capture().to_string()),
        timestamp_ms: now_ms(),
        breadcrumbs: breadcrumbs(),
      };
      append(path, pending);
    }
    previous(info);
  }));
}

/// The redacted debug log, then the shell's last spans and the ones still open: where it hung.
fn breadcrumbs() -> Vec<String> {
  let mut lines = debug_log::recent_lines();
  // Precision: Unix milliseconds fit an f64 exactly.
  lines.extend(trace::breadcrumb_lines(now_ms() as f64));
  lines
}

/// The data directory, from the app's setup.
pub fn set_data_dir(dir: &Path) {
  // Set once; a second call keeps the first directory.
  let _ = PENDING_PATH.set(dir.join(PENDING_FILE));
}

/// The shell's closed spans between two Unix milliseconds (docs/tracing.md): the page attaches
/// them to the measure it finishes, so a slow switch on desktop shows what the shell's core did.
#[tauri::command(rename_all = "snake_case")]
pub fn perf_spans(start_ms: f64, end_ms: f64) -> Vec<WireSpan> {
  trace::recent(start_ms, end_ms)
    .iter()
    .map(WireSpan::from)
    .collect()
}

/// The shell's kept trees (slow, failed or sampled roots) since the last call: only the page sends.
#[tauri::command(rename_all = "snake_case")]
pub fn perf_traces() -> Vec<WireTraceTree> {
  trace::take_kept().iter().map(WireTraceTree::from).collect()
}

/// The page's crash report settings, which the shell doesn't know: its trace layer follows them.
#[tauri::command(rename_all = "snake_case")]
pub fn perf_configure(is_on: bool, trace_rate: f64, slow_ms: u32) {
  trace::configure(TraceSettings {
    is_on,
    trace_rate: SampleRate::new(trace_rate).unwrap_or(SampleRate::NONE),
    slow_ms,
  });
}

/// The page takes the shell's pending panics to report them; the file is gone afterwards.
#[tauri::command(rename_all = "snake_case")]
pub fn crash_reports_take_native() -> Vec<PendingPanic> {
  let Some(path) = PENDING_PATH.get() else {
    return Vec::new();
  };
  let pending = read(path);
  if !pending.is_empty()
    && let Err(error) = std::fs::remove_file(path)
  {
    tracing::debug!(%error, "could not remove the pending crash reports");
  }
  pending
}

fn append(path: &Path, pending: PendingPanic) {
  let mut all = read(path);
  all.push(pending);
  let excess = all.len().saturating_sub(MAX_PENDING_PANICS);
  all.drain(..excess);
  if let Ok(json) = serde_json::to_vec(&all) {
    // Inside a panic hook there is nobody left to tell; a lost report is all that happens.
    let _ = std::fs::write(path, json);
  }
}

fn read(path: &Path) -> Vec<PendingPanic> {
  std::fs::read(path)
    .ok()
    .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    .unwrap_or_default()
}

fn now_ms() -> i64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_or(0, |elapsed| {
      i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
    })
}

#[cfg(test)]
mod tests {
  use super::*;

  /// A file of its own per test, removed with the test's scratch directory.
  fn scratch_file() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
      "teamcraft-crash-reports-{}-{}",
      std::process::id(),
      NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(PENDING_FILE)
  }

  #[test]
  fn keeps_the_newest_few() {
    let path = scratch_file();
    for index in 0..8 {
      append(
        &path,
        PendingPanic {
          parts: PanicParts {
            message: format!("panic {index}"),
            ..PanicParts::default()
          },
          timestamp_ms: index,
          breadcrumbs: Vec::new(),
        },
      );
    }
    let pending = read(&path);
    assert_eq!(pending.len(), MAX_PENDING_PANICS);
    assert_eq!(pending.last().unwrap().parts.message, "panic 7");
  }

  #[test]
  fn a_broken_file_reads_as_empty() {
    let path = scratch_file();
    std::fs::write(&path, b"not json").unwrap();
    assert!(read(&path).is_empty());
  }
}
