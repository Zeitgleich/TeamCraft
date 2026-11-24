//! The recordings' disk: how much is free (the recorder refuses to start a take below a floor),
//! and what a crash left in the folder.

use std::path::Path;

use tracing::{debug, info};

/// Bytes free for this process on the file system of `path`; `None` when it can't be measured.
pub fn free_bytes(path: &Path) -> Option<u64> {
  let stats = rustix::fs::statvfs(path).ok()?;
  Some(stats.f_bavail.saturating_mul(stats.f_frsize))
}

/// Whether a take may start in `folder`: at least `floor_bytes` free. A disk that can't be measured
/// counts as full: no take that would fail partway.
pub fn has_room(folder: &Path, floor_bytes: u64) -> bool {
  free_bytes(folder).is_some_and(|free| free >= floor_bytes)
}

/// Deletes what a crash or a kill left in `folder`: takes whose key died with the process, which
/// nobody can read. Returns how many files went.
pub fn sweep(folder: &Path) -> usize {
  let Ok(entries) = std::fs::read_dir(folder) else {
    return 0;
  };
  let mut removed = 0;
  for entry in entries.flatten() {
    let path = entry.path();
    if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
      continue;
    }
    match std::fs::remove_file(&path) {
      Ok(()) => removed += 1,
      Err(error) => debug!(%error, "a leftover take could not be deleted"),
    }
  }
  if removed > 0 {
    info!(removed, "deleted unreadable takes a restart left");
  }
  removed
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_floor_above_the_disk_refuses_and_a_missing_folder_counts_as_full() {
    let folder = std::env::temp_dir();
    assert!(free_bytes(&folder).is_some());
    assert!(has_room(&folder, 0));
    assert!(!has_room(&folder, u64::MAX));
    assert!(!has_room(Path::new("/no/such/folder/for/the/recorder"), 0));
  }

  #[test]
  fn the_sweep_deletes_files_only() {
    let folder = std::env::temp_dir().join(format!("teamcraft-recorder-sweep-{}", std::process::id()));
    std::fs::create_dir_all(folder.join("nested")).unwrap();
    std::fs::write(folder.join("take-1-1.ogg"), b"sealed").unwrap();
    std::fs::write(folder.join("take-1-1.ogg.key"), b"x").unwrap();
    assert_eq!(sweep(&folder), 2);
    assert!(folder.join("nested").is_dir());
    assert_eq!(sweep(&folder), 0);
    std::fs::remove_dir_all(&folder).unwrap();
  }
}
