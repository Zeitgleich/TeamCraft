//! The core's `tracing` events in the system log (issue #1067): logcat on Android, os_log on iOS,
//! in release builds too, so support can read why a sign-in or recovery failed. Warnings and
//! errors from everywhere, plus the lifecycle steps (open, enroll, recover, link, sync) at `info`.
//! Never secrets (security.md): events carry IDs and error causes, no content.

use std::collections::VecDeque;
use std::fmt::{self, Write as _};
use std::sync::{Arc, Mutex, Once, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(any(target_os = "android", target_os = "ios", test))]
use tracing::Level;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
#[cfg(any(target_os = "android", target_os = "ios", test))]
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::{Context, Layer};

use crate::TeamCraftError;

/// The target of lifecycle events, logged at `info` while everything else needs `warn`.
pub(crate) const LIFECYCLE: &str = "teamcraft_lifecycle";

/// Lines the in-memory ring keeps for the diagnostics export (issue #1094); older ones drop out.
pub(crate) const RING_CAPACITY: usize = 300;

/// Longest line the ring keeps, in characters; the rest is cut so the ring's memory stays bounded.
const MAX_LINE_CHARS: usize = 1024;

/// The logcat tag; on iOS the os_log category (the subsystem is the app identifier).
#[cfg(any(target_os = "android", target_os = "ios"))]
const TAG: &str = "TeamCraft";

#[cfg(target_os = "ios")]
const SUBSYSTEM: &str = "de.zeitgleich.teamcraft";

/// A step of the device's life the system log records with its outcome.
#[derive(Clone, Copy, Debug)]
pub(crate) enum LifecycleStep {
  Open,
  ResetStore,
  Enroll,
  StartLink,
  JoinScannedLink,
  FinishLink,
  ApproveLink,
  ConfirmLink,
  StartOrgRecovery,
  FinishOrgRecovery,
  StartSync,
  StopSync,
}

/// Sends `tracing` to the system log, once per process; later calls do nothing.
pub(crate) fn install() {
  static INSTALLED: Once = Once::new();
  INSTALLED.call_once(install_system_log);
}

/// Logs how `step` ended (the error's variant and message on failure) and passes the result on.
pub(crate) fn outcome<T>(
  step: LifecycleStep,
  result: Result<T, TeamCraftError>,
) -> Result<T, TeamCraftError> {
  match &result {
    Ok(_) => tracing::info!(target: LIFECYCLE, ?step, "done"),
    Err(error) => tracing::warn!(target: LIFECYCLE, ?step, ?error, "failed"),
  }
  result
}

/// The recent system-log lines, oldest first, for the diagnostics export (issue #1094). The same
/// events as logcat and os_log, so never secrets (security.md). Empty where no system log is
/// installed (desktop builds of this crate).
pub(crate) fn recent_lines() -> Vec<String> {
  global_ring().lines()
}

/// The text file the diagnostics export shares: a header with the app and OS version, then the
/// recent log lines (oldest first), one per line.
pub(crate) fn diagnostics_text(app_version: &str, os_version: &str, lines: &[String]) -> String {
  let mut text = format!(
    "TeamCraft diagnostics\nApp version: {app_version}\nOS version: {os_version}\nLog lines: {}\n\n",
    lines.len()
  );
  for line in lines {
    text.push_str(line);
    text.push('\n');
  }
  text
}

fn global_ring() -> &'static LogRing {
  static RING: OnceLock<LogRing> = OnceLock::new();
  RING.get_or_init(|| LogRing::new(RING_CAPACITY))
}

/// A bounded in-memory copy of the system log. Events are formatted before the lock is taken, so
/// the lock only covers a push, and logging stays cheap.
#[derive(Clone, Debug)]
pub(crate) struct LogRing {
  capacity: usize,
  lines: Arc<Mutex<VecDeque<String>>>,
}

impl LogRing {
  pub(crate) fn new(capacity: usize) -> Self {
    Self {
      capacity,
      lines: Arc::new(Mutex::new(VecDeque::with_capacity(capacity))),
    }
  }

  /// The kept lines, oldest first.
  pub(crate) fn lines(&self) -> Vec<String> {
    self.lock().iter().cloned().collect()
  }

  fn push(&self, line: String) {
    let mut lines = self.lock();
    while lines.len() >= self.capacity {
      lines.pop_front();
    }
    lines.push_back(line);
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<String>> {
    // A panic while pushing leaves a valid deque; the log must keep working after it.
    self.lines.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }
}

impl<S: Subscriber> Layer<S> for LogRing {
  fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
    if self.capacity == 0 {
      return;
    }
    let metadata = event.metadata();
    let mut line = format!(
      "{} {:>5} {}:",
      timestamp(SystemTime::now()),
      metadata.level(),
      metadata.target()
    );
    event.record(&mut LineVisitor(&mut line));
    self.push(truncate(line));
  }
}

/// Appends an event's message and `name=value` fields to a line.
struct LineVisitor<'a>(&'a mut String);

impl Visit for LineVisitor<'_> {
  fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
    // Writing to a String can't fail.
    if field.name() == "message" {
      let _ = write!(self.0, " {value:?}");
    } else {
      let _ = write!(self.0, " {}={value:?}", field.name());
    }
  }
}

fn truncate(line: String) -> String {
  match line.char_indices().nth(MAX_LINE_CHARS) {
    Some((end, _)) => format!("{}...", &line[..end]),
    None => line,
  }
}

/// `2026-11-01T12:00:00Z` (UTC, whole seconds).
fn timestamp(time: SystemTime) -> String {
  let seconds = time
    .duration_since(UNIX_EPOCH)
    .map_or(0, |elapsed| elapsed.as_secs());
  let (days, rest) = (seconds / 86_400, seconds % 86_400);
  // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
  let shifted = days as i64 + 719_468;
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

/// What reaches the system log: `warn` and above, lifecycle steps from `info`.
#[cfg(any(target_os = "android", target_os = "ios", test))]
fn system_log_filter() -> Targets {
  Targets::new()
    .with_default(Level::WARN)
    .with_target(LIFECYCLE, Level::INFO)
}

#[cfg(target_os = "android")]
fn install_system_log() {
  use tracing_subscriber::layer::{Layer as _, SubscriberExt as _};

  // logcat has its own time and priority columns; the layer leaves them out.
  let layer = paranoid_android::layer(TAG)
    .with_ansi(false)
    .with_filter(system_log_filter());
  let ring = global_ring().clone().with_filter(system_log_filter());
  // Ignored: another subscriber (a test harness) already owns the process.
  let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry().with(layer).with(ring));
}

#[cfg(target_os = "ios")]
fn install_system_log() {
  use tracing_oslog::OsLogger;
  use tracing_subscriber::layer::{Layer as _, SubscriberExt as _};

  let layer = OsLogger::new(SUBSYSTEM, TAG).with_filter(system_log_filter());
  let ring = global_ring().clone().with_filter(system_log_filter());
  // Ignored: another subscriber (a test harness) already owns the process.
  let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry().with(layer).with(ring));
}

/// Desktop builds of this crate are tests and tools: they bring their own subscriber.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn install_system_log() {}

#[cfg(test)]
mod tests {
  use super::*;

  use tracing_subscriber::layer::SubscriberExt as _;

  #[test]
  fn warnings_reach_the_system_log_from_everywhere() {
    let filter = system_log_filter();
    assert!(filter.would_enable("teamcraft_core::sync", &Level::WARN));
    assert!(filter.would_enable("teamcraft_ffi::engine", &Level::ERROR));
  }

  #[test]
  fn info_reaches_it_only_for_lifecycle_steps() {
    let filter = system_log_filter();
    assert!(filter.would_enable(LIFECYCLE, &Level::INFO));
    assert!(!filter.would_enable("teamcraft_core::sync", &Level::INFO));
    assert!(!filter.would_enable(LIFECYCLE, &Level::DEBUG));
  }

  mod ring {
    use super::*;

    #[test]
    fn keeps_only_the_newest_lines_in_order() {
      let ring = LogRing::new(3);
      record(&ring, || {
        for number in 0..5 {
          tracing::warn!(number, "event");
        }
      });
      let lines = ring.lines();
      assert_eq!(lines.len(), 3);
      assert!(lines[0].ends_with("event number=2"), "{lines:?}");
      assert!(lines[2].ends_with("event number=4"), "{lines:?}");
    }

    #[test]
    fn applies_the_system_log_filter() {
      let ring = LogRing::new(10);
      record(&ring, || {
        tracing::info!(target: "teamcraft_core::sync", "chatter");
        tracing::debug!(target: LIFECYCLE, "detail");
        tracing::info!(target: LIFECYCLE, "lifecycle");
        tracing::error!("broken");
      });
      let lines = ring.lines();
      assert_eq!(lines.len(), 2, "{lines:?}");
      assert!(lines[0].contains("INFO teamcraft_lifecycle: lifecycle"));
      assert!(lines[1].contains("ERROR"));
    }

    #[test]
    fn carries_the_failed_step_like_the_system_log() {
      let ring = LogRing::new(10);
      record(&ring, || {
        let result: Result<(), TeamCraftError> = Err(TeamCraftError::InvalidInput("bad".to_owned()));
        let _ = outcome(LifecycleStep::Open, result);
      });
      let lines = ring.lines();
      assert!(lines[0].contains("step=Open"), "{lines:?}");
      assert!(lines[0].contains("failed"), "{lines:?}");
    }

    #[test]
    fn cuts_overlong_lines() {
      let ring = LogRing::new(2);
      record(&ring, || tracing::warn!("{}", "x".repeat(5_000)));
      let line = &ring.lines()[0];
      assert_eq!(line.chars().count(), MAX_LINE_CHARS + 3);
      assert!(line.ends_with("..."));
    }

    #[test]
    fn formats_the_timestamp_in_utc() {
      let time = UNIX_EPOCH + std::time::Duration::from_secs(1_793_534_400);
      assert_eq!(timestamp(time), "2026-11-01T12:00:00Z");
      assert_eq!(timestamp(UNIX_EPOCH), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn export_text_has_versions_then_the_lines() {
      let lines = vec!["a".to_owned(), "b".to_owned()];
      assert_eq!(
        diagnostics_text("1.2.3", "Android 15", &lines),
        "TeamCraft diagnostics\nApp version: 1.2.3\nOS version: Android 15\nLog lines: 2\n\na\nb\n"
      );
      assert!(diagnostics_text("1", "2", &[]).ends_with("Log lines: 0\n\n"));
    }

    fn record(ring: &LogRing, events: impl FnOnce()) {
      let subscriber = tracing_subscriber::registry().with(ring.clone().with_filter(system_log_filter()));
      tracing::subscriber::with_default(subscriber, events);
    }
  }
}
