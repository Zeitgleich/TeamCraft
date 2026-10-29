//! The container's health check. The notetaker listens on no port, so instead of probing an
//! endpoint like the bridge, `teamcraft-notetaker healthcheck` reads a liveness file the worker
//! rewrites every few seconds while its loop runs: missing or too old means unhealthy (the loop
//! hangs or the process is gone).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use thiserror::Error;
use tracing::warn;

/// How often the worker rewrites the file.
pub const TOUCH_INTERVAL: Duration = Duration::from_secs(10);

/// The worker's side: rewrites the file now and then.
pub struct Liveness {
  path: PathBuf,
  maybe_last: Option<Instant>,
  has_warned: bool,
}

#[derive(Debug, Error)]
pub enum HealthError {
  #[error("the liveness file {path} can't be read")]
  Unreadable {
    path: String,
    #[source]
    source: std::io::Error,
  },
  #[error("the liveness file holds no time")]
  Malformed,
  #[error("the worker last ran {age_secs} s ago")]
  Stale { age_secs: u64 },
}

impl Liveness {
  pub fn new(path: PathBuf) -> Self {
    Self {
      path,
      maybe_last: None,
      has_warned: false,
    }
  }

  /// Writes the current time into the file, at most every [`TOUCH_INTERVAL`].
  pub fn touch(&mut self, now: Instant) {
    if self
      .maybe_last
      .is_some_and(|last| now.saturating_duration_since(last) < TOUCH_INTERVAL)
    {
      return;
    }
    self.maybe_last = Some(now);
    let seconds = unix_seconds(SystemTime::now());
    if let Err(error) = std::fs::write(&self.path, format!("{seconds}\n")) {
      if !self.has_warned {
        warn!(%error, path = %self.path.display(), "the liveness file can't be written; the health check will fail");
        self.has_warned = true;
      }
    } else {
      self.has_warned = false;
    }
  }
}

/// Healthy when `path` holds a time no older than `max_age` at `now`.
pub fn check(path: &Path, max_age: Duration, now: SystemTime) -> Result<(), HealthError> {
  let text = std::fs::read_to_string(path).map_err(|source| HealthError::Unreadable {
    path: path.display().to_string(),
    source,
  })?;
  let written: u64 = text.trim().parse().map_err(|_| HealthError::Malformed)?;
  let age_secs = unix_seconds(now).saturating_sub(written);
  if age_secs > max_age.as_secs() {
    return Err(HealthError::Stale { age_secs });
  }
  Ok(())
}

fn unix_seconds(time: SystemTime) -> u64 {
  time.duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_fresh_file_is_healthy_and_an_old_or_missing_one_is_not() {
    let path = scratch_file("fresh");
    let mut liveness = Liveness::new(path.clone());
    liveness.touch(Instant::now());
    let max_age = Duration::from_secs(120);
    assert!(check(&path, max_age, SystemTime::now()).is_ok());
    let later = SystemTime::now() + Duration::from_secs(600);
    assert!(matches!(
      check(&path, max_age, later),
      Err(HealthError::Stale { age_secs }) if age_secs >= 599
    ));
    std::fs::remove_file(&path).unwrap();
    assert!(matches!(
      check(&path, max_age, SystemTime::now()),
      Err(HealthError::Unreadable { .. })
    ));
  }

  #[test]
  fn the_file_is_rewritten_only_every_interval_and_garbage_is_unhealthy() {
    let path = scratch_file("interval");
    let mut liveness = Liveness::new(path.clone());
    let start = Instant::now();
    liveness.touch(start);
    std::fs::write(&path, "garbage").unwrap();
    liveness.touch(start + Duration::from_secs(1));
    assert!(matches!(
      check(&path, Duration::from_secs(120), SystemTime::now()),
      Err(HealthError::Malformed)
    ));
    liveness.touch(start + TOUCH_INTERVAL);
    assert!(check(&path, Duration::from_secs(120), SystemTime::now()).is_ok());
    std::fs::remove_file(&path).unwrap();
  }

  fn scratch_file(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!("teamcraft-notetaker-health-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    directory.join(format!("{name}.alive"))
  }
}
