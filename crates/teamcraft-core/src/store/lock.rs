//! One process per store (e2ee.md §9.4): an OS file lock next to the database.
//!
//! The store is loaded into memory and written back; two processes on the same file would each
//! keep their own MLS state and outbox, and the device's state forks. The desktop app runs as a
//! single instance, and this lock is the backstop when that fails (no session bus, a second build
//! of the app). The OS drops the lock when the process ends, also after a crash.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use super::StoreError;

/// Held while a process owns the store at a path; dropping it releases the store.
#[derive(Debug)]
pub struct StoreLock {
  _file: File,
}

impl StoreLock {
  /// Takes the lock of the store at `database_path`, or fails with [`StoreError::InUse`] when
  /// another process holds it.
  pub fn acquire(database_path: &Path) -> Result<Self, StoreError> {
    let file = OpenOptions::new()
      .create(true)
      .truncate(false)
      .write(true)
      .open(lock_path(database_path))
      .map_err(|error| StoreError::Database(format!("store lock: {error}")))?;
    match file.try_lock() {
      Ok(()) => Ok(Self { _file: file }),
      Err(TryLockError::WouldBlock) => Err(StoreError::InUse),
      Err(TryLockError::Error(error)) => Err(StoreError::Database(format!("store lock: {error}"))),
    }
  }
}

fn lock_path(database_path: &Path) -> PathBuf {
  let mut name = database_path.as_os_str().to_owned();
  name.push(".lock");
  PathBuf::from(name)
}

#[cfg(test)]
mod tests {
  use rand_core::{OsRng, RngCore};

  use super::*;

  #[test]
  fn a_second_holder_is_refused_until_the_first_lets_go() {
    let directory = std::env::temp_dir().join(format!("teamcraft-lock-{}", OsRng.next_u64()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("store.db");
    let first = StoreLock::acquire(&path).unwrap();
    assert!(matches!(StoreLock::acquire(&path), Err(StoreError::InUse)));
    drop(first);
    let again = StoreLock::acquire(&path).unwrap();
    drop(again);
    std::fs::remove_dir_all(&directory).unwrap();
  }
}
