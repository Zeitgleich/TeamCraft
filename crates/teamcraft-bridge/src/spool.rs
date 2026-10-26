//! Where the files of a webhook wait while they go up (docs/bridge.md, "Files"): one file each in
//! the spool directory, created for the bridge's user alone, and deleted as soon as the request is
//! answered, whatever the answer. The server needs a file's size before its upload begins, and a
//! form doesn't say it, so the file is read to its end first; on disk, not in memory.
//!
//! A bridge that stops while files are on their way leaves them behind; the next start deletes
//! them ([`SpoolDir::sweep`]).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::io::AsyncWriteExt;
use tracing::warn;

/// Names of spooled files: `upload-<process>-<number>.part`. Only such files are ever deleted, so a
/// spool directory set to a directory with other files in it loses none of them.
const FILE_PREFIX: &str = "upload-";
const FILE_SUFFIX: &str = ".part";

/// Numbers the files of this process.
static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

/// The spool directory.
#[derive(Debug, Clone)]
pub struct SpoolDir {
  path: PathBuf,
}

/// A file being written into the spool directory.
pub struct SpoolWriter {
  file: tokio::fs::File,
  spooled: SpooledFile,
}

/// A file in the spool directory, with what its upload needs. Dropping it deletes the file.
#[derive(Debug)]
pub struct SpooledFile {
  path: PathBuf,
  pub file_name: String,
  pub media_type: String,
  pub size_bytes: u64,
}

impl SpoolDir {
  pub fn new(path: PathBuf) -> Self {
    Self { path }
  }

  /// Deletes the files an earlier run left behind. Returns how many.
  pub fn sweep(&self) -> io::Result<usize> {
    let entries = match std::fs::read_dir(&self.path) {
      Ok(entries) => entries,
      Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
      Err(error) => return Err(error),
    };
    let mut removed = 0;
    for entry in entries {
      let entry = entry?;
      // `file_type` does not follow links: a link is never a spooled file.
      if entry.file_type()?.is_file() && is_spooled_name(&entry.file_name().to_string_lossy()) {
        std::fs::remove_file(entry.path())?;
        removed += 1;
      }
    }
    Ok(removed)
  }

  /// Creates a new, empty file (the directory too, the first time).
  pub async fn create(&self, file_name: String, media_type: String) -> io::Result<SpoolWriter> {
    create_private_dir(&self.path).await?;
    loop {
      let number = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
      let path = self.path.join(format!(
        "{FILE_PREFIX}{}-{number}{FILE_SUFFIX}",
        std::process::id()
      ));
      match open_new_private(&path).await {
        Ok(file) => {
          return Ok(SpoolWriter {
            file,
            spooled: SpooledFile {
              path,
              file_name,
              media_type,
              size_bytes: 0,
            },
          });
        },
        // A leftover of an earlier process with the same ID: take the next number.
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
        Err(error) => return Err(error),
      }
    }
  }
}

impl SpoolWriter {
  pub async fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
    self.file.write_all(bytes).await?;
    self.spooled.size_bytes += bytes.len() as u64;
    Ok(())
  }

  /// Writes out what is buffered and closes the file.
  pub async fn finish(mut self) -> io::Result<SpooledFile> {
    self.file.flush().await?;
    Ok(self.spooled)
  }
}

impl SpooledFile {
  pub fn path(&self) -> &Path {
    &self.path
  }
}

impl Drop for SpooledFile {
  fn drop(&mut self) {
    if let Err(error) = std::fs::remove_file(&self.path)
      && error.kind() != io::ErrorKind::NotFound
    {
      warn!(%error, "a spooled file could not be deleted");
    }
  }
}

fn is_spooled_name(name: &str) -> bool {
  name
    .strip_prefix(FILE_PREFIX)
    .and_then(|rest| rest.strip_suffix(FILE_SUFFIX))
    .is_some_and(|middle| {
      middle
        .split('-')
        .all(|number| !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()))
    })
}

#[cfg(unix)]
async fn create_private_dir(path: &Path) -> io::Result<()> {
  let mut builder = tokio::fs::DirBuilder::new();
  builder.recursive(true).mode(0o700);
  builder.create(path).await
}

#[cfg(not(unix))]
async fn create_private_dir(path: &Path) -> io::Result<()> {
  tokio::fs::create_dir_all(path).await
}

async fn open_new_private(path: &Path) -> io::Result<tokio::fs::File> {
  let mut options = tokio::fs::OpenOptions::new();
  options.write(true).create_new(true);
  #[cfg(unix)]
  options.mode(0o600);
  options.open(path).await
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn a_spooled_file_holds_what_was_written_until_it_is_dropped() {
    let spool = SpoolDir::new(scratch_dir("written"));
    let mut writer = spool
      .create("report.pdf".into(), "application/pdf".into())
      .await
      .unwrap();
    writer.write(b"%PDF").await.unwrap();
    writer.write(b"-1.7").await.unwrap();
    let spooled = writer.finish().await.unwrap();
    assert_eq!(spooled.size_bytes, 8);
    assert_eq!(std::fs::read(spooled.path()).unwrap(), b"%PDF-1.7");
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt;
      let mode = std::fs::metadata(spooled.path()).unwrap().permissions().mode();
      assert_eq!(mode & 0o777, 0o600, "only the bridge's user reads it");
    }
    let path = spooled.path().to_owned();
    drop(spooled);
    assert!(!path.exists());
  }

  #[tokio::test]
  async fn a_file_given_up_while_written_is_deleted() {
    let spool = SpoolDir::new(scratch_dir("given-up"));
    let mut writer = spool.create("a".into(), "b".into()).await.unwrap();
    writer.write(b"half").await.unwrap();
    let path = writer.spooled.path().to_owned();
    drop(writer);
    assert!(!path.exists());
  }

  #[test]
  fn a_sweep_deletes_leftovers_and_nothing_else() {
    let directory = scratch_dir("sweep");
    std::fs::create_dir_all(&directory).unwrap();
    for name in [
      "upload-12-0.part",
      "upload-12-7.part",
      "bridge.db",
      "upload-x.part",
      "upload-1-2.txt",
    ] {
      std::fs::write(directory.join(name), b"x").unwrap();
    }
    assert_eq!(SpoolDir::new(directory.clone()).sweep().unwrap(), 2);
    let mut left: Vec<String> = std::fs::read_dir(&directory)
      .unwrap()
      .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
      .collect();
    left.sort();
    assert_eq!(left, ["bridge.db", "upload-1-2.txt", "upload-x.part"]);
    assert_eq!(SpoolDir::new(directory.join("missing")).sweep().unwrap(), 0);
  }

  fn scratch_dir(name: &str) -> PathBuf {
    let directory =
      std::env::temp_dir().join(format!("teamcraft-bridge-spool-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    directory
  }
}
