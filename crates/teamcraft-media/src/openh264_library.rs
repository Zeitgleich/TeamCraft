//! Cisco's prebuilt OpenH264 library, loaded at runtime.
//!
//! H.264 is patent-encumbered. Cisco pays the MPEG LA royalties for its own OpenH264 binaries,
//! but only for binaries downloaded from Cisco by the end user's device: a build from source,
//! or Cisco's binary bundled into our installer, is not covered (openh264.org, "FAQ"). So release
//! builds contain no OpenH264 code. On first video use the app downloads the binary for its
//! platform from `ciscobinary.openh264.org`, it is checked against Cisco's published SHA-256
//! hashes (the list the `openh264` crate ships), cached in the app's data directory and loaded
//! from there, with the hash checked again on every load.
//!
//! Development builds and tests can build OpenH264 from source instead (feature
//! `openh264-source`); a loaded library always wins over the built-in one.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use openh264::OpenH264API;
use thiserror::Error;

/// Cisco's download host. Plain HTTP is what Cisco offers; integrity comes from the hash check.
const DOWNLOAD_BASE_URL: &str = "http://ciscobinary.openh264.org";
/// The release whose hashes the `openh264` crate knows.
const VERSION: &str = "2.6.0";
/// Upper bound of a decompressed library (they are about 1 MB).
const MAX_LIBRARY_BYTES: u64 = 16 * 1024 * 1024;

/// The library this process encodes and decodes with, once installed.
static LIBRARY: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Cisco's binary for one platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenH264Binary {
  /// File name of the library, e.g. `libopenh264-2.6.0-linux64.8.so`.
  pub file_name: String,
}

#[derive(Debug, Error)]
pub enum OpenH264LibraryError {
  #[error("Cisco publishes no OpenH264 binary for this platform")]
  UnsupportedPlatform,
  #[error("the OpenH264 download is not a valid bzip2 archive: {0}")]
  Archive(std::io::Error),
  #[error("the OpenH264 download is larger than expected")]
  TooLarge,
  #[error("storing the OpenH264 library failed: {0}")]
  Storage(std::io::Error),
  /// Not one of Cisco's published binaries (wrong file, tampered download), or not loadable.
  #[error("the OpenH264 library was rejected: {0}")]
  Rejected(String),
  #[error("OpenH264 is not installed; video needs Cisco's OpenH264 library")]
  NotInstalled,
}

impl OpenH264Binary {
  /// Cisco's binary for the platform this was built for.
  pub fn for_this_platform() -> Result<Self, OpenH264LibraryError> {
    let platform = platform_suffix().ok_or(OpenH264LibraryError::UnsupportedPlatform)?;
    Ok(Self {
      file_name: format!("{}{VERSION}-{platform}", library_prefix()),
    })
  }

  /// Where Cisco publishes it (bzip2-compressed).
  pub fn download_url(&self) -> String {
    format!("{DOWNLOAD_BASE_URL}/{}.bz2", self.file_name)
  }

  /// Where it lives in `directory` once installed.
  pub fn path_in(&self, directory: &Path) -> PathBuf {
    directory.join(&self.file_name)
  }

  /// Uses an earlier download from `directory` if there is a valid one; `false` if not.
  pub fn use_installed(&self, directory: &Path) -> bool {
    let path = self.path_in(directory);
    path.exists() && use_library(&path).is_ok()
  }

  /// Unpacks a download (`compressed`, from [`Self::download_url`]) into `directory`, checks it
  /// against Cisco's hashes and uses it from now on.
  pub fn install(&self, directory: &Path, compressed: &[u8]) -> Result<PathBuf, OpenH264LibraryError> {
    let library = decompress(compressed)?;
    std::fs::create_dir_all(directory).map_err(OpenH264LibraryError::Storage)?;
    let path = self.path_in(directory);
    let partial = directory.join(format!("{}.partial", self.file_name));
    std::fs::write(&partial, library).map_err(OpenH264LibraryError::Storage)?;
    if let Err(error) = check(&partial) {
      // The rejected file is useless; a leftover would only be checked and rejected again.
      let _ = std::fs::remove_file(&partial);
      return Err(error);
    }
    std::fs::rename(&partial, &path).map_err(OpenH264LibraryError::Storage)?;
    use_library(&path)?;
    Ok(path)
  }
}

/// Uses the library at `path` (checked against Cisco's hashes) for every encoder and decoder
/// created from now on.
pub fn use_library(path: &Path) -> Result<(), OpenH264LibraryError> {
  check(path)?;
  *LIBRARY.write().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(path.to_path_buf());
  Ok(())
}

/// Whether encoders and decoders can be created (a library is installed, or built in).
pub fn is_available() -> bool {
  cfg!(feature = "openh264-source") || installed_path().is_some()
}

/// The API for a new encoder or decoder.
pub(crate) fn api() -> Result<OpenH264API, OpenH264LibraryError> {
  if let Some(path) = installed_path() {
    return OpenH264API::from_blob_path(&path)
      .map_err(|error| OpenH264LibraryError::Rejected(error.to_string()));
  }
  built_in_api()
}

#[cfg(feature = "openh264-source")]
fn built_in_api() -> Result<OpenH264API, OpenH264LibraryError> {
  Ok(OpenH264API::from_source())
}

#[cfg(not(feature = "openh264-source"))]
fn built_in_api() -> Result<OpenH264API, OpenH264LibraryError> {
  Err(OpenH264LibraryError::NotInstalled)
}

fn installed_path() -> Option<PathBuf> {
  LIBRARY
    .read()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .clone()
}

/// Loads the library once: `from_blob_path` only loads files whose SHA-256 is one of Cisco's.
fn check(path: &Path) -> Result<(), OpenH264LibraryError> {
  OpenH264API::from_blob_path(path)
    .map(drop)
    .map_err(|error| OpenH264LibraryError::Rejected(error.to_string()))
}

fn decompress(compressed: &[u8]) -> Result<Vec<u8>, OpenH264LibraryError> {
  let mut library = Vec::new();
  bzip2::read::BzDecoder::new(compressed)
    .take(MAX_LIBRARY_BYTES + 1)
    .read_to_end(&mut library)
    .map_err(OpenH264LibraryError::Archive)?;
  if library.len() as u64 > MAX_LIBRARY_BYTES {
    return Err(OpenH264LibraryError::TooLarge);
  }
  Ok(library)
}

fn library_prefix() -> &'static str {
  if cfg!(target_os = "windows") {
    "openh264-"
  } else {
    "libopenh264-"
  }
}

/// Cisco's name for this platform, with the library's extension.
fn platform_suffix() -> Option<&'static str> {
  let suffix = if cfg!(all(target_os = "android", target_arch = "aarch64")) {
    "android-arm64.8.so"
  } else if cfg!(all(target_os = "android", target_arch = "x86_64")) {
    "android-x64.8.so"
  } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    "linux64.8.so"
  } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
    "linux-arm64.8.so"
  } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
    "mac-arm64.dylib"
  } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
    "mac-x64.dylib"
  } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
    "win64.dll"
  } else {
    return None;
  };
  Some(suffix)
}

#[cfg(test)]
mod tests {
  use std::io::Write;

  use super::*;

  #[test]
  fn names_match_ciscos_downloads() {
    let binary = OpenH264Binary {
      file_name: "libopenh264-2.6.0-linux64.8.so".into(),
    };
    assert_eq!(
      binary.download_url(),
      "http://ciscobinary.openh264.org/libopenh264-2.6.0-linux64.8.so.bz2"
    );
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
      assert_eq!(OpenH264Binary::for_this_platform().unwrap(), binary);
    }
  }

  #[test]
  fn unknown_libraries_are_rejected_and_not_kept() {
    let directory = std::env::temp_dir().join(format!("teamcraft-openh264-test-{}", std::process::id()));
    let binary = OpenH264Binary {
      file_name: "libopenh264-test.so".into(),
    };
    let result = binary.install(&directory, &bzip2_of(b"not a library"));
    assert!(matches!(result, Err(OpenH264LibraryError::Rejected(_))));
    assert!(!binary.path_in(&directory).exists());
    assert!(!binary.use_installed(&directory));
    let _ = std::fs::remove_dir_all(&directory);
  }

  #[test]
  fn broken_downloads_are_rejected() {
    let binary = OpenH264Binary {
      file_name: "libopenh264-test.so".into(),
    };
    let result = binary.install(&std::env::temp_dir(), b"not bzip2");
    assert!(matches!(result, Err(OpenH264LibraryError::Archive(_))));
  }

  fn bzip2_of(data: &[u8]) -> Vec<u8> {
    let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
  }
}
