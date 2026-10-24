//! A directory below the build's temp dir for tests, removed again.

use std::path::{Path, PathBuf};

pub struct TempDir(pub PathBuf);

impl TempDir {
  pub fn new(name: &str) -> Self {
    let nanos = std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .map(|elapsed| elapsed.as_nanos())
      .unwrap_or_default();
    Self(std::env::temp_dir().join(format!("teamcraft-desktop-{name}-{}-{nanos}", std::process::id())))
  }

  pub fn path(&self) -> &Path {
    &self.0
  }
}

impl Drop for TempDir {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}
