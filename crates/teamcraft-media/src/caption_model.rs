//! The speech model of live captions (#215): downloaded on first use, not shipped with the app,
//! and checked against its pinned SHA-256 when it is written and again whenever it is loaded.
//!
//! The model is whisper.cpp's quantized multilingual "base" model (MIT, from the project's own
//! model repository), about 57 MiB: good enough for German and English at a real-time factor
//! that fits beside a call on an ordinary laptop. The download is pinned to a commit of that
//! repository, so the file behind the URL can't change; the hash would refuse it anyway.
//!
//! A download goes into a `.part` file next to the model and is renamed into place only once it is
//! complete and its hash matches: a broken or stopped download never looks installed.

use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;

/// The model captions use.
pub const CAPTION_MODEL: CaptionModelFile = CaptionModelFile {
  file_name: "ggml-base-q5_1.bin",
  url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-base-q5_1.bin",
  sha256_hex: "422f1ae452ade6f30a004d7e5c6a43195e4433bc370bf23fac9cc591f01a8898",
  size_bytes: 59_707_625,
};

/// The suffix of a download that isn't complete yet.
const PART_SUFFIX: &str = ".part";

/// A speech model file as published: where it comes from and what it must be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptionModelFile {
  pub file_name: &'static str,
  pub url: &'static str,
  pub sha256_hex: &'static str,
  pub size_bytes: u64,
}

/// A download being written: hashed as it comes, bounded by the published size.
pub struct ModelDownload {
  model: CaptionModelFile,
  part_path: PathBuf,
  final_path: PathBuf,
  file: io::BufWriter<File>,
  hasher: Sha256,
  written_bytes: u64,
}

#[derive(Debug, Error)]
pub enum CaptionModelError {
  #[error("storing the speech model failed: {0}")]
  Storage(#[from] io::Error),
  /// More bytes than the published file has.
  #[error("the speech model download is larger than the published file")]
  TooLarge,
  /// Not the published file (wrong, truncated or tampered with).
  #[error("the speech model is not the published file")]
  Rejected,
}

impl CaptionModelFile {
  /// Where it lives in `directory` once installed.
  pub fn path_in(&self, directory: &Path) -> PathBuf {
    directory.join(self.file_name)
  }

  /// Whether a file is installed in `directory` (its content isn't checked: see
  /// [`Self::verified_path`]).
  pub fn is_installed(&self, directory: &Path) -> bool {
    self.path_in(directory).is_file()
  }

  /// The installed file, once its size and hash are checked; a file that fails the check is
  /// deleted, so the next start downloads it again. Reads the whole file: call it off the UI and
  /// the call's threads.
  pub fn verified_path(&self, directory: &Path) -> Result<PathBuf, CaptionModelError> {
    let path = self.path_in(directory);
    let checked = check_file(self, &path);
    if matches!(checked, Err(CaptionModelError::Rejected)) {
      // A file that isn't the model is of no use to anybody.
      let _ = fs::remove_file(&path);
    }
    checked.map(|()| path)
  }

  /// Starts writing a download into `directory` (created if missing); an earlier unfinished one
  /// is overwritten.
  pub fn start_download(&self, directory: &Path) -> Result<ModelDownload, CaptionModelError> {
    fs::create_dir_all(directory)?;
    let final_path = self.path_in(directory);
    let part_path = directory.join(format!("{}{PART_SUFFIX}", self.file_name));
    let file = File::create(&part_path)?;
    Ok(ModelDownload {
      model: *self,
      part_path,
      final_path,
      file: io::BufWriter::new(file),
      hasher: Sha256::new(),
      written_bytes: 0,
    })
  }

  /// Deletes the model and any unfinished download from `directory`.
  pub fn delete(&self, directory: &Path) -> Result<(), CaptionModelError> {
    for path in [
      self.path_in(directory),
      directory.join(format!("{}{PART_SUFFIX}", self.file_name)),
    ] {
      match fs::remove_file(path) {
        Ok(()) => {},
        Err(error) if error.kind() == io::ErrorKind::NotFound => {},
        Err(error) => return Err(error.into()),
      }
    }
    Ok(())
  }
}

impl ModelDownload {
  /// Writes the next piece of the download.
  pub fn write(&mut self, chunk: &[u8]) -> Result<(), CaptionModelError> {
    self.written_bytes += chunk.len() as u64;
    if self.written_bytes > self.model.size_bytes {
      return Err(CaptionModelError::TooLarge);
    }
    self.hasher.update(chunk);
    self.file.write_all(chunk)?;
    Ok(())
  }

  pub fn written_bytes(&self) -> u64 {
    self.written_bytes
  }

  /// Checks the complete download and puts it in place; a download that isn't the published
  /// file is deleted.
  pub fn finish(mut self) -> Result<PathBuf, CaptionModelError> {
    self.file.flush()?;
    self.file.get_ref().sync_all()?;
    let hash = hex(&self.hasher.finalize_reset());
    if self.written_bytes != self.model.size_bytes || hash != self.model.sha256_hex {
      return Err(CaptionModelError::Rejected);
    }
    fs::rename(&self.part_path, &self.final_path)?;
    Ok(self.final_path.clone())
  }
}

impl Drop for ModelDownload {
  /// A download that wasn't finished (failed, refused, stopped) leaves nothing behind.
  fn drop(&mut self) {
    if self.part_path.exists() {
      let _ = fs::remove_file(&self.part_path);
    }
  }
}

fn check_file(model: &CaptionModelFile, path: &Path) -> Result<(), CaptionModelError> {
  let file = File::open(path)?;
  if file.metadata()?.len() != model.size_bytes {
    return Err(CaptionModelError::Rejected);
  }
  let mut reader = BufReader::with_capacity(1 << 20, file);
  let mut hasher = Sha256::new();
  let mut buffer = vec![0u8; 1 << 20];
  loop {
    let read = reader.read(&mut buffer)?;
    if read == 0 {
      break;
    }
    hasher.update(&buffer[..read]);
  }
  if hex(&hasher.finalize()) != model.sha256_hex {
    return Err(CaptionModelError::Rejected);
  }
  Ok(())
}

fn hex(bytes: &[u8]) -> String {
  bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  /// SHA-256 of `b"model bytes"`.
  const SAMPLE_SHA256: &str = "9cb7487000bc86ac36ce83c4acfabe8878552be99572a6770f65ab1d048a5c48";

  #[test]
  fn a_complete_matching_download_is_installed_and_verifies() {
    let directory = TestDir::new("complete");
    let model = sample_model();
    let mut download = model.start_download(directory.path()).unwrap();
    download.write(b"model ").unwrap();
    download.write(b"bytes").unwrap();
    let path = download.finish().unwrap();
    assert_eq!(path, model.path_in(directory.path()));
    assert!(model.is_installed(directory.path()));
    assert_eq!(model.verified_path(directory.path()).unwrap(), path);
    assert!(!part_path(&model, directory.path()).exists());
  }

  #[test]
  fn a_wrong_download_is_refused_and_leaves_nothing() {
    let directory = TestDir::new("wrong");
    let model = sample_model();
    let mut download = model.start_download(directory.path()).unwrap();
    download.write(b"model bytez").unwrap();
    assert!(matches!(download.finish(), Err(CaptionModelError::Rejected)));
    assert!(!model.is_installed(directory.path()));
    assert!(!part_path(&model, directory.path()).exists());
  }

  #[test]
  fn a_truncated_download_is_refused() {
    let directory = TestDir::new("truncated");
    let model = sample_model();
    let mut download = model.start_download(directory.path()).unwrap();
    download.write(b"model").unwrap();
    assert!(matches!(download.finish(), Err(CaptionModelError::Rejected)));
  }

  #[test]
  fn more_bytes_than_published_stop_the_download() {
    let directory = TestDir::new("too_large");
    let model = sample_model();
    let mut download = model.start_download(directory.path()).unwrap();
    assert!(matches!(
      download.write(b"model bytes and more"),
      Err(CaptionModelError::TooLarge)
    ));
    drop(download);
    assert!(!part_path(&model, directory.path()).exists());
  }

  #[test]
  fn a_stopped_download_leaves_nothing() {
    let directory = TestDir::new("stopped");
    let model = sample_model();
    let mut download = model.start_download(directory.path()).unwrap();
    download.write(b"model").unwrap();
    drop(download);
    assert!(!part_path(&model, directory.path()).exists());
    assert!(!model.is_installed(directory.path()));
  }

  #[test]
  fn a_changed_installed_file_fails_the_check_and_is_deleted() {
    let directory = TestDir::new("changed");
    let model = sample_model();
    fs::write(model.path_in(directory.path()), b"model bytez").unwrap();
    assert!(matches!(
      model.verified_path(directory.path()),
      Err(CaptionModelError::Rejected)
    ));
    assert!(!model.is_installed(directory.path()));
  }

  #[test]
  fn delete_removes_the_model_and_is_fine_without_one() {
    let directory = TestDir::new("delete");
    let model = sample_model();
    model.delete(directory.path()).unwrap();
    fs::write(model.path_in(directory.path()), b"model bytes").unwrap();
    model.delete(directory.path()).unwrap();
    assert!(!model.is_installed(directory.path()));
  }

  #[test]
  fn the_pinned_model_is_https_and_names_its_file() {
    assert!(CAPTION_MODEL.url.starts_with("https://"));
    assert!(CAPTION_MODEL.url.ends_with(CAPTION_MODEL.file_name));
    assert_eq!(CAPTION_MODEL.sha256_hex.len(), 64);
  }

  /// A fresh folder under the system's temporary folder, deleted after the test.
  struct TestDir(PathBuf);

  impl TestDir {
    fn new(name: &str) -> Self {
      let path = std::env::temp_dir().join(format!("teamcraft-caption-model-{name}-{}", std::process::id()));
      let _ = fs::remove_dir_all(&path);
      fs::create_dir_all(&path).unwrap();
      Self(path)
    }

    fn path(&self) -> &Path {
      &self.0
    }
  }

  impl Drop for TestDir {
    fn drop(&mut self) {
      let _ = fs::remove_dir_all(&self.0);
    }
  }

  fn sample_model() -> CaptionModelFile {
    CaptionModelFile {
      file_name: "sample.bin",
      url: "https://example.com/sample.bin",
      sha256_hex: SAMPLE_SHA256,
      size_bytes: 11,
    }
  }

  fn part_path(model: &CaptionModelFile, directory: &Path) -> PathBuf {
    directory.join(format!("{}{PART_SUFFIX}", model.file_name))
  }
}
