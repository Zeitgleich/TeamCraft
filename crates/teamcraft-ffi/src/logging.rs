//! The core's `tracing` events in the system log (issue #1067): logcat on Android, os_log on iOS,
//! in release builds too, so support can read why a sign-in or recovery failed. Warnings and
//! errors from everywhere, plus the lifecycle steps (open, enroll, recover, link, sync) at `info`.
//! Never secrets (security.md): events carry IDs and error causes, no content.

use std::sync::Once;

#[cfg(any(target_os = "android", target_os = "ios", test))]
use tracing::Level;
#[cfg(any(target_os = "android", target_os = "ios", test))]
use tracing_subscriber::filter::Targets;

use crate::TeamCraftError;

/// The target of lifecycle events, logged at `info` while everything else needs `warn`.
pub(crate) const LIFECYCLE: &str = "teamcraft_lifecycle";

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
  // Ignored: another subscriber (a test harness) already owns the process.
  let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry().with(layer));
}

#[cfg(target_os = "ios")]
fn install_system_log() {
  use tracing_oslog::OsLogger;
  use tracing_subscriber::layer::{Layer as _, SubscriberExt as _};

  let layer = OsLogger::new(SUBSYSTEM, TAG).with_filter(system_log_filter());
  // Ignored: another subscriber (a test harness) already owns the process.
  let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry().with(layer));
}

/// Desktop builds of this crate are tests and tools: they bring their own subscriber.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn install_system_log() {}

#[cfg(test)]
mod tests {
  use super::*;

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
}
