//! Where the files of a webhook wait while they go up (docs/bridge.md, "Files"): one file each in
//! the spool directory, created for the bridge's user alone, and deleted as soon as the request is
//! answered, whatever the answer. The server needs a file's size before its upload begins, and a
//! form doesn't say it, so the file is read to its end first; on disk, not in memory.
//!
//! All requests together hold at most `files.max_spool_bytes`: every write reserves its bytes
//! first, and a file gives its reservation back when it is deleted. A bridge that stops while files
//! are on their way leaves them behind; the next start deletes them ([`SpoolDir::prepare`]).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
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
  usage: Arc<SpoolUsage>,
}

/// A file being written into the spool directory.
pub struct SpoolWriter {
  file: tokio::fs::File,
  spooled: SpooledFile,
}

/// A file in the spool directory, with what its upload needs. Dropping it deletes the file and
/// frees its bytes in the spool.
#[derive(Debug)]
pub struct SpooledFile {
  path: PathBuf,
  pub file_name: String,
  pub media_type: String,
  pub size_bytes: u64,
  /// A picture's thumbnail and size, made by the endpoint ([`crate::file_hooks`]).
  pub maybe_thumbnail: Option<Vec<u8>>,
  pub maybe_dimensions: Option<(u32, u32)>,
  usage: Arc<SpoolUsage>,
}

/// Bytes the spooled files take, all requests together.
#[derive(Debug)]
struct SpoolUsage {
  used: AtomicU64,
  max: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum SpoolError {
  /// The files of all requests together take `files.max_spool_bytes`.
  #[error("the spool directory is full")]
  Full,
  #[error("the spool directory {0} is a link or belongs to another user")]
  NotPrivate(String),
  #[error(transparent)]
  Io(#[from] io::Error),
}

impl SpoolDir {
  pub fn new(path: PathBuf, max_bytes: u64) -> Self {
    Self {
      path,
      usage: Arc::new(SpoolUsage {
        used: AtomicU64::new(0),
        max: max_bytes,
      }),
    }
  }

  /// Gets the directory ready at start: creates it for the bridge's user alone, narrows a wider
  /// mode to `0700`, refuses a link or another user's directory (somebody else could read the
  /// files there), and deletes the files an earlier run left behind. Returns how many.
  pub fn prepare(&self) -> Result<usize, SpoolError> {
    let display = || self.path.display().to_string();
    match std::fs::symlink_metadata(&self.path) {
      Ok(metadata) if !metadata.is_dir() => return Err(SpoolError::NotPrivate(display())),
      Ok(_) => {},
      Err(error) if error.kind() == io::ErrorKind::NotFound => {},
      Err(error) => return Err(error.into()),
    }
    create_private_dir_sync(&self.path)?;
    make_private(&self.path)?;
    Ok(self.sweep()?)
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

  /// Bytes the spooled files take now.
  pub fn used_bytes(&self) -> u64 {
    self.usage.used.load(Ordering::Acquire)
  }

  /// Creates a new, empty file (the directory too, should it be gone).
  pub async fn create(&self, file_name: String, media_type: String) -> Result<SpoolWriter, SpoolError> {
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
              maybe_thumbnail: None,
              maybe_dimensions: None,
              usage: self.usage.clone(),
            },
          });
        },
        // A leftover of an earlier process with the same ID: take the next number.
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
        Err(error) => return Err(error.into()),
      }
    }
  }
}

impl SpoolWriter {
  /// Appends `bytes`, once the spool has room for them.
  pub async fn write(&mut self, bytes: &[u8]) -> Result<(), SpoolError> {
    let length = bytes.len() as u64;
    if !self.spooled.usage.try_reserve(length) {
      return Err(SpoolError::Full);
    }
    // Counted before the write: a failed write keeps its reservation until the file is deleted.
    self.spooled.size_bytes += length;
    self.file.write_all(bytes).await?;
    Ok(())
  }

  /// Writes out what is buffered and closes the file.
  pub async fn finish(mut self) -> Result<SpooledFile, SpoolError> {
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
    self.usage.release(self.size_bytes);
  }
}

impl SpoolUsage {
  fn try_reserve(&self, bytes: u64) -> bool {
    self
      .used
      .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
        used.checked_add(bytes).filter(|total| *total <= self.max)
      })
      .is_ok()
  }

  fn release(&self, bytes: u64) {
    // Never below zero: every release follows its reservation.
    let _ = self.used.try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
      Some(used.saturating_sub(bytes))
    });
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

#[cfg(unix)]
fn create_private_dir_sync(path: &Path) -> io::Result<()> {
  use std::os::unix::fs::DirBuilderExt;
  std::fs::DirBuilder::new()
    .recursive(true)
    .mode(0o700)
    .create(path)
}

#[cfg(not(unix))]
fn create_private_dir_sync(path: &Path) -> io::Result<()> {
  std::fs::create_dir_all(path)
}

/// The directory must be the bridge's own, readable by nobody else. Its owner is compared with the
/// owner of a file the bridge creates in it (the process's user, without a system call of our own).
#[cfg(unix)]
fn make_private(path: &Path) -> Result<(), SpoolError> {
  use std::os::unix::fs::{MetadataExt, PermissionsExt};
  let probe = path.join(format!(".owner-{}", std::process::id()));
  let probe_owner = {
    let file = std::fs::OpenOptions::new()
      .write(true)
      .create_new(true)
      .open(&probe)?;
    let owner = file.metadata().map(|metadata| metadata.uid());
    drop(file);
    std::fs::remove_file(&probe)?;
    owner?
  };
  let metadata = std::fs::metadata(path)?;
  if metadata.uid() != probe_owner {
    return Err(SpoolError::NotPrivate(path.display().to_string()));
  }
  if metadata.permissions().mode() & 0o777 != 0o700 {
    warn!(directory = %path.display(), "the spool directory was open to others; it is 0700 now");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
  }
  Ok(())
}

#[cfg(not(unix))]
fn make_private(_path: &Path) -> Result<(), SpoolError> {
  Ok(())
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
    let spool = SpoolDir::new(scratch_dir("written"), 1024);
    let mut writer = spool
      .create("report.pdf".into(), "application/pdf".into())
      .await
      .unwrap();
    writer.write(b"%PDF").await.unwrap();
    writer.write(b"-1.7").await.unwrap();
    let spooled = writer.finish().await.unwrap();
    assert_eq!(spooled.size_bytes, 8);
    assert_eq!(spool.used_bytes(), 8);
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
    assert_eq!(spool.used_bytes(), 0, "its bytes are free again");
  }

  #[tokio::test]
  async fn a_file_given_up_while_written_is_deleted() {
    let spool = SpoolDir::new(scratch_dir("given-up"), 1024);
    let mut writer = spool.create("a".into(), "b".into()).await.unwrap();
    writer.write(b"half").await.unwrap();
    let path = writer.spooled.path().to_owned();
    drop(writer);
    assert!(!path.exists());
    assert_eq!(spool.used_bytes(), 0);
  }

  #[tokio::test]
  async fn all_files_together_stay_within_the_spool_limit() {
    let spool = SpoolDir::new(scratch_dir("full"), 10);
    let mut first = spool.create("a".into(), "b".into()).await.unwrap();
    let mut second = spool.create("c".into(), "d".into()).await.unwrap();
    first.write(&[1; 6]).await.unwrap();
    assert!(matches!(second.write(&[2; 5]).await, Err(SpoolError::Full)));
    second.write(&[2; 4]).await.unwrap();
    drop(first);
    second.write(&[2; 6]).await.unwrap();
    assert_eq!(spool.used_bytes(), 10);
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
    assert_eq!(SpoolDir::new(directory.clone(), 1).prepare().unwrap(), 2);
    let mut left: Vec<String> = std::fs::read_dir(&directory)
      .unwrap()
      .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
      .collect();
    left.sort();
    assert_eq!(left, ["bridge.db", "upload-1-2.txt", "upload-x.part"]);
    assert_eq!(SpoolDir::new(directory.join("missing"), 1).sweep().unwrap(), 0);
  }

  #[cfg(unix)]
  #[test]
  fn a_spool_open_to_others_is_closed_at_start_and_a_link_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let directory = scratch_dir("mode");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o777)).unwrap();
    SpoolDir::new(directory.clone(), 1).prepare().unwrap();
    let mode = std::fs::metadata(&directory).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);

    let fresh = scratch_dir("fresh").join("nested");
    SpoolDir::new(fresh.clone(), 1).prepare().unwrap();
    assert_eq!(
      std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777,
      0o700
    );

    let link = scratch_dir("link");
    std::os::unix::fs::symlink(&directory, &link).unwrap();
    assert!(matches!(
      SpoolDir::new(link, 1).prepare(),
      Err(SpoolError::NotPrivate(_))
    ));
  }

  fn scratch_dir(name: &str) -> PathBuf {
    let directory =
      std::env::temp_dir().join(format!("teamcraft-bridge-spool-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    let _ = std::fs::remove_file(&directory);
    directory
  }
}
