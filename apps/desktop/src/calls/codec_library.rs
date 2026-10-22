//! Cisco's OpenH264 for video, downloaded on first use (teamcraft-media's `openh264_library`
//! explains why it isn't shipped) and cached in the app's data directory.

use std::path::PathBuf;
use std::time::Duration;

use teamcraft_media::openh264_library::{self, OpenH264Binary};
use tracing::info;

/// Cisco's compressed libraries are below 1 MB; anything far larger isn't one.
const MAX_DOWNLOAD_BYTES: usize = 16 * 1024 * 1024;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);

pub(super) struct CodecLibrary {
  directory: PathBuf,
  /// One download at a time; a second caller waits and then finds the library installed.
  installing: tokio::sync::Mutex<()>,
}

impl CodecLibrary {
  /// `directory`: where the library is cached (the app's data directory).
  pub(super) fn new(directory: PathBuf) -> Self {
    Self {
      directory,
      installing: tokio::sync::Mutex::new(()),
    }
  }

  /// Makes sure H.264 can be encoded and decoded: uses the cached library or downloads it. The
  /// error is for the user (shown with a "try again"); calling again retries.
  pub(super) async fn ensure(&self) -> Result<(), String> {
    if openh264_library::is_available() {
      return Ok(());
    }
    let _installing = self.installing.lock().await;
    if openh264_library::is_available() {
      return Ok(());
    }
    let binary = OpenH264Binary::for_this_platform().map_err(|error| error.to_string())?;
    let directory = self.directory.clone();
    let cached = {
      let binary = binary.clone();
      let directory = directory.clone();
      tokio::task::spawn_blocking(move || binary.use_installed(&directory))
        .await
        .map_err(|error| error.to_string())?
    };
    if cached {
      return Ok(());
    }
    info!(url = %binary.download_url(), "downloading Cisco's OpenH264 for video");
    let compressed = download(&binary.download_url())
      .await
      .map_err(|error| format!("video needs Cisco's OpenH264, which could not be downloaded ({error})"))?;
    tokio::task::spawn_blocking(move || binary.install(&directory, &compressed))
      .await
      .map_err(|error| error.to_string())?
      .map(drop)
      .map_err(|error| error.to_string())
  }
}

async fn download(url: &str) -> Result<Vec<u8>, String> {
  let client = reqwest::Client::builder()
    .timeout(DOWNLOAD_TIMEOUT)
    .build()
    .map_err(|error| error.to_string())?;
  let response = client
    .get(url)
    .send()
    .await
    .and_then(reqwest::Response::error_for_status)
    .map_err(|error| error.to_string())?;
  if response
    .content_length()
    .is_some_and(|length| length > MAX_DOWNLOAD_BYTES as u64)
  {
    return Err("the download is too large".into());
  }
  let bytes = response.bytes().await.map_err(|error| error.to_string())?;
  if bytes.len() > MAX_DOWNLOAD_BYTES {
    return Err("the download is too large".into());
  }
  Ok(bytes.to_vec())
}
