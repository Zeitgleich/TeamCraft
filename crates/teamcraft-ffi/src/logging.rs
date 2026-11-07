//! The core's `tracing` events in the system log (issue #1067): logcat on Android, os_log on iOS,
//! in release builds too, so support can read why a sign-in or recovery failed. Warnings and
//! errors from everywhere, plus the lifecycle steps (open, enroll, recover, link, sync) at `info`.
//! Never secrets (security.md): events carry IDs and error causes, no content.

use std::sync::Once;
use std::time::Duration;

#[cfg(any(target_os = "android", target_os = "ios", test))]
use tracing::Level;
#[cfg(any(target_os = "android", target_os = "ios", test))]
use tracing_subscriber::filter::Targets;

use crate::TeamCraftError;

/// The target of lifecycle events, logged at `info` while everything else needs `warn`.
pub(crate) const LIFECYCLE: &str = "teamcraft_lifecycle";

/// The core's module that sums up each gateway connection's phases.
#[cfg(any(target_os = "android", target_os = "ios", test))]
const SYNC_TRACE: &str = "teamcraft_core::client::sync_trace";

/// From here on an engine job is worth a line in the system log.
const SLOW_JOB: Duration = Duration::from_millis(250);

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

/// Sends `tracing` and panics to the system log, once per process; later calls do nothing.
pub(crate) fn install() {
  static INSTALLED: Once = Once::new();
  INSTALLED.call_once(|| {
    install_system_log();
    install_panic_log();
  });
}

/// Rust's panic output goes nowhere on Android: panics of any thread (their message and location)
/// go to the system log, and with it the diagnostics export (issue #1142).
fn install_panic_log() {
  let previous = std::panic::take_hook();
  std::panic::set_hook(Box::new(move |info| {
    tracing::error!(target: LIFECYCLE, panic = %info, "panicked");
    previous(info);
  }));
}

/// Logs an engine job that held the engine thread, or waited for it, for long. Everything the apps
/// ask for runs on that thread beside the sync loop, so this names what a slow start waits for
/// (issue #1142). `job` is the job's type name: code, never data.
pub(crate) fn slow_job(job: &str, waited: Duration, ran: Duration) {
  if waited >= SLOW_JOB || ran >= SLOW_JOB {
    tracing::info!(
      target: LIFECYCLE,
      job,
      waited_ms = waited.as_millis(),
      ran_ms = ran.as_millis(),
      "slow engine job"
    );
  }
}

/// Logs a read on the read thread that took long or waited long, which should be rare: nothing but
/// another read and another thread's group of writes holds it up.
pub(crate) fn slow_read(job: &str, waited: Duration, ran: Duration) {
  if waited >= SLOW_JOB || ran >= SLOW_JOB {
    tracing::info!(
      target: LIFECYCLE,
      job,
      waited_ms = waited.as_millis(),
      ran_ms = ran.as_millis(),
      "slow read"
    );
  }
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

/// The recent log lines, oldest first, for the diagnostics export (issue #1094) and "Report a
/// problem" (docs/bug-reports.md): the core's redacted ring (`teamcraft_client_api::debug_log`), which
/// holds TeamCraft's own events from `info` and everything else from `warn`.
pub(crate) fn recent_lines() -> Vec<String> {
  teamcraft_client_api::debug_log::recent_lines()
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

/// What reaches the system log: `warn` and above, lifecycle steps from `info`.
#[cfg(any(target_os = "android", target_os = "ios", test))]
fn system_log_filter() -> Targets {
  Targets::new()
    .with_default(Level::WARN)
    .with_target(LIFECYCLE, Level::INFO)
    // One line per connection: where the time of the last connect went (issue #1142).
    .with_target(SYNC_TRACE, Level::INFO)
}

#[cfg(target_os = "android")]
fn install_system_log() {
  use tracing_subscriber::layer::{Layer as _, SubscriberExt as _};

  // logcat has its own time and priority columns; the layer leaves them out.
  let layer = paranoid_android::layer(TAG)
    .with_ansi(false)
    .with_filter(system_log_filter());
  let ring = teamcraft_client_api::debug_log::layer();
  // Ignored: another subscriber (a test harness) already owns the process.
  let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry().with(layer).with(ring));
}

#[cfg(target_os = "ios")]
fn install_system_log() {
  use tracing_oslog::OsLogger;
  use tracing_subscriber::layer::{Layer as _, SubscriberExt as _};

  let layer = OsLogger::new(SUBSYSTEM, TAG).with_filter(system_log_filter());
  let ring = teamcraft_client_api::debug_log::layer();
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

    use teamcraft_client_api::debug_log::{DebugLog, DebugLogSettings};

    #[test]
    fn carries_the_failed_step_like_the_system_log() {
      let ring = DebugLog::new(DebugLogSettings::default());
      let subscriber = tracing_subscriber::registry().with(ring.clone());
      tracing::subscriber::with_default(subscriber, || {
        let result: Result<(), TeamCraftError> = Err(TeamCraftError::InvalidInput("bad".to_owned()));
        let _ = outcome(LifecycleStep::Open, result);
      });
      let lines = ring.lines();
      assert!(lines[0].contains("step=Open"), "{lines:?}");
      assert!(lines[0].contains("failed"), "{lines:?}");
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
  }
}
