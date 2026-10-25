//! Large files the webview hands to the core (issue #153): it can't give the core a path (a
//! webview's file picker and drag and drop only yield `File` objects), and passing the whole file
//! as bytes would hold it in memory several times over. So the webview copies it here in chunks,
//! each one a raw IPC request, and the message names the staged copy's path; the core reads it one
//! part at a time while it goes up and deletes it afterwards (`maybe_upload_staging_dir`).

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Largest chunk the webview may send at once.
pub const MAX_CHUNK_BYTES: usize = 16 * 1024 * 1024;
/// Staged copies are named by the webview's random ID: 32 hex digits.
const STAGE_ID_LEN: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum StagingError {
  #[error("the staged file's ID is not 32 hex digits")]
  InvalidId,
  #[error("a chunk may have at most {MAX_CHUNK_BYTES} bytes")]
  ChunkTooLarge,
  #[error("the chunk at {offset} doesn't continue the staged file, which has {staged} bytes")]
  OutOfOrder { offset: u64, staged: u64 },
  #[error("the staged file could not be written: {0}")]
  Io(#[from] std::io::Error),
}

/// Writes `chunk` at `offset` of the staged copy `id` in `directory` and returns its path. A chunk
/// sent again (its answer was lost) is accepted without writing it twice; the first chunk starts
/// the copy over.
pub fn append_chunk(directory: &Path, id: &str, offset: u64, chunk: &[u8]) -> Result<PathBuf, StagingError> {
  if id.len() != STAGE_ID_LEN || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
    return Err(StagingError::InvalidId);
  }
  if chunk.len() > MAX_CHUNK_BYTES {
    return Err(StagingError::ChunkTooLarge);
  }
  std::fs::create_dir_all(directory)?;
  let path = directory.join(id);
  if offset == 0 {
    std::fs::write(&path, chunk)?;
    return Ok(path);
  }
  let mut file = OpenOptions::new().append(true).open(&path)?;
  let staged = file.metadata()?.len();
  if staged == offset + chunk.len() as u64 {
    return Ok(path);
  }
  if staged != offset {
    return Err(StagingError::OutOfOrder { offset, staged });
  }
  file.write_all(chunk)?;
  Ok(path)
}

#[cfg(test)]
mod tests {
  use super::*;

  const ID: &str = "0123456789abcdef0123456789abcdef";

  #[test]
  fn chunks_append_in_order_and_a_repeated_one_is_written_once() {
    let directory = scratch("in-order");
    let path = append_chunk(&directory, ID, 0, b"hello ").unwrap();
    append_chunk(&directory, ID, 6, b"world").unwrap();
    append_chunk(&directory, ID, 6, b"world").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"hello world");
    assert!(matches!(
      append_chunk(&directory, ID, 3, b"x"),
      Err(StagingError::OutOfOrder {
        offset: 3,
        staged: 11
      })
    ));
    std::fs::remove_dir_all(directory).unwrap();
  }

  #[test]
  fn ids_cannot_leave_the_staging_directory() {
    let directory = scratch("ids");
    for id in [
      "../../etc/passwd",
      "",
      "0123456789abcdef0123456789abcde/",
      "g123456789abcdef0123456789abcdef",
    ] {
      assert!(matches!(
        append_chunk(&directory, id, 0, b"x"),
        Err(StagingError::InvalidId)
      ));
    }
    assert!(matches!(
      append_chunk(&directory, ID, 0, &vec![0; MAX_CHUNK_BYTES + 1]),
      Err(StagingError::ChunkTooLarge)
    ));
    let _ = std::fs::remove_dir_all(directory);
  }

  fn scratch(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("teamcraft-staging-{name}-{}", std::process::id()))
  }
}
