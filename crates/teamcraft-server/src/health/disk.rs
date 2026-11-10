//! The disk as the server measures it itself (`statvfs` of `[health] disk_paths`), beside what the
//! deployment reports in `disk_used_percent`. The fuller one counts.
//!
//! `statvfs` is one system call, but on a network file system that stopped answering it can hang:
//! it runs on the blocking pool with a time limit, and while one is still hanging no second one is
//! started, so a dead mount costs one thread, not one per evaluation.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tracing::warn;

use super::heartbeat::Reading;

/// How long one measurement of all paths may take.
const MEASURE_TIMEOUT: Duration = Duration::from_secs(2);

/// Measures the disks of the configured paths.
#[derive(Debug, Default)]
pub struct DiskGauge {
  paths: Vec<PathBuf>,
  /// A measurement that has not come back yet.
  is_in_flight: Arc<AtomicBool>,
}

impl DiskGauge {
  pub fn new(paths: Vec<PathBuf>) -> Self {
    Self {
      paths,
      is_in_flight: Arc::default(),
    }
  }

  /// The percentage in use of the fullest file system among the paths. `NotConfigured` without
  /// paths, `Invalid` when none could be measured (or the last measurement still hangs).
  pub async fn measure(&self) -> Reading {
    if self.paths.is_empty() {
      return Reading::NotConfigured;
    }
    if self.is_in_flight.swap(true, Ordering::AcqRel) {
      return Reading::Invalid;
    }
    let paths = self.paths.clone();
    let in_flight = Arc::clone(&self.is_in_flight);
    let task = tokio::task::spawn_blocking(move || {
      let reading = fullest(&paths);
      in_flight.store(false, Ordering::Release);
      reading
    });
    match tokio::time::timeout(MEASURE_TIMEOUT, task).await {
      Ok(Ok(reading)) => reading,
      Ok(Err(error)) => {
        warn!(%error, "measuring the disk failed");
        self.is_in_flight.store(false, Ordering::Release);
        Reading::Invalid
      },
      Err(_) => {
        warn!("measuring the disk timed out; a mount may hang");
        Reading::Invalid
      },
    }
  }
}

/// The fuller of what the deployment reports and what the server measured. A value beats no value;
/// without either, the one that says more (a missing file is "no data", not "not set up").
pub fn fuller(reported: Reading, measured: Reading) -> Reading {
  match (reported, measured) {
    (Reading::Value(a), Reading::Value(b)) => Reading::Value(a.max(b)),
    (Reading::Value(value), _) | (_, Reading::Value(value)) => Reading::Value(value),
    (Reading::NotConfigured, other) | (other, Reading::NotConfigured) => other,
    (Reading::Missing, _) | (_, Reading::Missing) => Reading::Missing,
    (Reading::Invalid, Reading::Invalid) => Reading::Invalid,
  }
}

fn fullest(paths: &[PathBuf]) -> Reading {
  let mut maybe_fullest: Option<i64> = None;
  for (index, path) in paths.iter().enumerate() {
    match used_percent(path) {
      Some(percent) => maybe_fullest = Some(maybe_fullest.map_or(percent, |seen| seen.max(percent))),
      // Only its position goes to the log: the path is the operator's to name.
      None => warn!(index, "a path of [health] disk_paths could not be measured"),
    }
  }
  maybe_fullest.map_or(Reading::Invalid, Reading::Value)
}

/// The share of the file system in use, as `df` shows it: of the blocks an unprivileged user can
/// have, the reserved ones left out.
#[cfg(unix)]
fn used_percent(path: &Path) -> Option<i64> {
  let stats = rustix::fs::statvfs(path).ok()?;
  percent(stats.f_blocks, stats.f_bfree, stats.f_bavail)
}

#[cfg(not(unix))]
fn used_percent(_path: &Path) -> Option<i64> {
  None
}

fn percent(blocks: u64, free: u64, available: u64) -> Option<i64> {
  let used = blocks.checked_sub(free)?;
  let usable = used.checked_add(available)?;
  if usable == 0 {
    return None;
  }
  // Rounded up, as `df` does: 84.1 % is not "84 %, fine".
  let percent = (u128::from(used) * 100).div_ceil(u128::from(usable));
  i64::try_from(percent).ok()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn it_is_the_share_of_what_users_can_have_rounded_up() {
    // 1000 blocks, 200 free of which 50 reserved: 800 used of 950.
    assert_eq!(percent(1000, 200, 150), Some(85));
    assert_eq!(percent(1000, 1000, 1000), Some(0));
    assert_eq!(percent(1000, 0, 0), Some(100));
  }

  #[test]
  fn the_fuller_reading_counts() {
    assert_eq!(fuller(Reading::Value(40), Reading::Value(90)), Reading::Value(90));
    assert_eq!(fuller(Reading::Missing, Reading::Value(30)), Reading::Value(30));
    assert_eq!(fuller(Reading::Value(70), Reading::Invalid), Reading::Value(70));
    assert_eq!(fuller(Reading::NotConfigured, Reading::Invalid), Reading::Invalid);
    assert_eq!(
      fuller(Reading::NotConfigured, Reading::NotConfigured),
      Reading::NotConfigured
    );
  }

  #[test]
  fn nonsense_counts_measure_nothing() {
    assert_eq!(percent(0, 0, 0), None);
    assert_eq!(percent(10, 20, 0), None);
  }

  #[tokio::test]
  async fn without_paths_nothing_is_measured() {
    assert_eq!(DiskGauge::new(Vec::new()).measure().await, Reading::NotConfigured);
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn the_root_file_system_can_be_measured() {
    let reading = DiskGauge::new(vec![PathBuf::from("/")]).measure().await;
    assert!(
      matches!(reading, Reading::Value(percent) if (0..=100).contains(&percent)),
      "{reading:?}"
    );
  }

  #[tokio::test]
  async fn a_path_that_does_not_exist_is_no_value() {
    let reading = DiskGauge::new(vec![PathBuf::from("/does/not/exist/at/all")])
      .measure()
      .await;
    assert_eq!(reading, Reading::Invalid);
  }
}
