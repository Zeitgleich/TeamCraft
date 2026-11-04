//! What the deployment reports about things the server cannot see from inside its container: the
//! last backup, the served certificate, the disk. Three small files in `[health] heartbeat_dir`,
//! each holding one number as text, rewritten by the deployment (docs/operations.md, "Health and
//! alerts"). Read every evaluation: three tiny reads, no query.

use std::path::{Path, PathBuf};

use tokio::io::AsyncReadExt;
use tracing::warn;

/// Unix time in seconds of the last backup that finished.
pub const BACKUP_FILE: &str = "backup_last_success";
/// Unix time in seconds when the served certificate expires (its `notAfter`).
pub const CERTIFICATE_FILE: &str = "certificate_not_after";
/// Percentage of the busiest data disk in use, 0 to 100.
pub const DISK_FILE: &str = "disk_used_percent";

/// A number is a few digits; anything longer is not one of these files.
const MAX_FILE_BYTES: u64 = 64;

/// What one heartbeat says.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Reading {
  /// No heartbeat directory is set: the deployment does not report this.
  #[default]
  NotConfigured,
  /// The directory is set, the file is not there (yet).
  Missing,
  /// The file is there but holds no number.
  Invalid,
  Value(i64),
}

/// The three heartbeats.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Heartbeats {
  pub backup: Reading,
  pub certificate: Reading,
  pub disk_percent: Reading,
}

impl Heartbeats {
  /// Reads the files; without a directory, nothing is reported.
  pub async fn read(maybe_dir: Option<&Path>) -> Self {
    let Some(dir) = maybe_dir else {
      return Self::default();
    };
    Self {
      backup: read_file(dir.join(BACKUP_FILE)).await,
      certificate: read_file(dir.join(CERTIFICATE_FILE)).await,
      disk_percent: read_file(dir.join(DISK_FILE)).await,
    }
  }
}

async fn read_file(path: PathBuf) -> Reading {
  let file = match tokio::fs::File::open(&path).await {
    Ok(file) => file,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Reading::Missing,
    Err(error) => {
      warn!(%error, file = ?path.file_name(), "could not open a health heartbeat");
      return Reading::Invalid;
    },
  };
  let mut text = String::new();
  match file.take(MAX_FILE_BYTES).read_to_string(&mut text).await {
    Ok(_) => parse(&text),
    Err(error) => {
      warn!(%error, file = ?path.file_name(), "could not read a health heartbeat");
      Reading::Invalid
    },
  }
}

/// A whole or decimal number, surrounded by whitespace at most. Decimals are rounded down: the
/// deployment may write `83.5` for the disk.
fn parse(text: &str) -> Reading {
  match text.trim().parse::<f64>() {
    Ok(number) if number.is_finite() && number.abs() < 1e15 => {
      // The range is checked above; the cast cannot overflow.
      Reading::Value(number.floor() as i64)
    },
    _ => Reading::Invalid,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn numbers_are_read_with_whitespace_and_decimals() {
    assert_eq!(parse("1800000000\n"), Reading::Value(1_800_000_000));
    assert_eq!(parse(" 83.5 "), Reading::Value(83));
    assert_eq!(parse("0"), Reading::Value(0));
  }

  #[test]
  fn anything_else_is_invalid() {
    for text in ["", "yesterday", "NaN", "inf", "1e300", "12 13", "--1"] {
      assert_eq!(parse(text), Reading::Invalid, "{text:?}");
    }
  }

  #[tokio::test]
  async fn the_files_of_a_directory_are_read_and_a_missing_one_is_missing() {
    let dir = directory("read");
    std::fs::write(dir.join(BACKUP_FILE), "1700000000\n").unwrap();
    std::fs::write(dir.join(DISK_FILE), "not a number").unwrap();
    let heartbeats = Heartbeats::read(Some(&dir)).await;
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(
      heartbeats,
      Heartbeats {
        backup: Reading::Value(1_700_000_000),
        certificate: Reading::Missing,
        disk_percent: Reading::Invalid,
      }
    );
  }

  #[tokio::test]
  async fn without_a_directory_nothing_is_reported() {
    assert_eq!(Heartbeats::read(None).await, Heartbeats::default());
  }

  #[tokio::test]
  async fn a_file_that_is_far_too_long_is_cut_and_not_a_number() {
    let dir = directory("long");
    std::fs::write(dir.join(BACKUP_FILE), "1".repeat(10_000)).unwrap();
    let heartbeats = Heartbeats::read(Some(&dir)).await;
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(heartbeats.backup, Reading::Invalid);
  }

  fn directory(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("teamcraft-heartbeat-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
  }
}
