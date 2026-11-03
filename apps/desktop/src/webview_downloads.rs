//! Downloads the webview starts by itself (#719). The app saves files through its own "Save…"
//! (a save dialog, then the core decrypts to the picked path), but a download the page still
//! triggers (a `download` link, a menu entry of the webview) must not vanish: it goes into the
//! downloads folder under a free name, and the page is told whether it worked, which it shows.
//! The handler can't open a dialog: it runs on the main thread and must answer at once.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::webview::DownloadEvent;
use tauri::{Emitter, Manager, Runtime, Webview};

/// Tells the page how a download of its own ended: [`DownloadOutcome`] as JSON text.
pub const DOWNLOAD_FINISHED_EVENT: &str = "webview-download-finished";
/// The name of a download that names none.
const FALLBACK_NAME: &str = "download";

/// How a webview download ended, for the page's toast.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct DownloadOutcome {
  pub is_saved: bool,
  /// The saved file's name (never its folder).
  pub maybe_name: Option<String>,
}

/// Handles one download event of a webview; `true` lets it go ahead.
pub fn on_download<R: Runtime>(webview: Webview<R>, event: DownloadEvent<'_>) -> bool {
  match event {
    DownloadEvent::Requested { destination, .. } => {
      let Ok(directory) = webview.path().download_dir() else {
        tracing::warn!("no downloads folder: a webview download was refused");
        notify(
          &webview,
          &DownloadOutcome {
            is_saved: false,
            maybe_name: None,
          },
        );
        return false;
      };
      let name = destination
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
      *destination = free_path(&directory, &name);
      true
    },
    DownloadEvent::Finished { path, success, .. } => {
      let maybe_name = path
        .as_deref()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned());
      if !success {
        tracing::warn!("a webview download failed");
      }
      notify(
        &webview,
        &DownloadOutcome {
          is_saved: success,
          maybe_name,
        },
      );
      true
    },
    _ => true,
  }
}

fn notify<R: Runtime>(webview: &Webview<R>, outcome: &DownloadOutcome) {
  let Ok(text) = serde_json::to_string(outcome) else {
    return;
  };
  if let Err(error) = webview.emit(DOWNLOAD_FINISHED_EVENT, text) {
    tracing::debug!(%error, "could not tell the page about a download");
  }
}

/// `name` in `directory`, or `name (1)`, `name (2)`, … before its extension when taken. Only the
/// file name of `name` counts, so a download can't leave the folder.
pub fn free_path(directory: &Path, name: &str) -> PathBuf {
  let base = Path::new(name)
    .file_name()
    .map(|name| name.to_string_lossy().into_owned())
    .filter(|name| !name.trim().is_empty() && name != "." && name != "..")
    .unwrap_or_else(|| FALLBACK_NAME.to_owned());
  let candidate = directory.join(&base);
  if !candidate.exists() {
    return candidate;
  }
  let (stem, maybe_extension) = match base.rsplit_once('.') {
    Some((stem, extension)) if !stem.is_empty() => (stem.to_owned(), Some(extension.to_owned())),
    _ => (base.clone(), None),
  };
  (1..)
    .map(|number| match &maybe_extension {
      Some(extension) => directory.join(format!("{stem} ({number}).{extension}")),
      None => directory.join(format!("{stem} ({number})")),
    })
    .find(|path| !path.exists())
    // Invariant: the range is endless and a folder holds finitely many files.
    .unwrap_or(candidate)
}

#[cfg(test)]
mod tests {
  #![allow(clippy::unwrap_used)]

  use super::*;

  #[test]
  fn a_taken_name_gets_a_number_before_its_extension() {
    let directory = std::env::temp_dir().join(format!("teamcraft-webview-dl-{}", std::process::id()));
    drop(std::fs::remove_dir_all(&directory));
    std::fs::create_dir_all(&directory).unwrap();

    assert_eq!(free_path(&directory, "beach.jpg"), directory.join("beach.jpg"));
    std::fs::write(directory.join("beach.jpg"), b"").unwrap();
    assert_eq!(
      free_path(&directory, "beach.jpg"),
      directory.join("beach (1).jpg")
    );
    std::fs::write(directory.join("beach (1).jpg"), b"").unwrap();
    assert_eq!(
      free_path(&directory, "beach.jpg"),
      directory.join("beach (2).jpg")
    );
    std::fs::write(directory.join("notes"), b"").unwrap();
    assert_eq!(free_path(&directory, "notes"), directory.join("notes (1)"));
    std::fs::write(directory.join(".hidden"), b"").unwrap();
    assert_eq!(free_path(&directory, ".hidden"), directory.join(".hidden (1)"));
    std::fs::remove_dir_all(&directory).unwrap();
  }

  #[test]
  fn a_download_stays_in_the_folder() {
    let directory = PathBuf::from("/nonexistent-teamcraft-folder");
    assert_eq!(
      free_path(&directory, "../../etc/passwd"),
      directory.join("passwd")
    );
    assert_eq!(free_path(&directory, ""), directory.join(FALLBACK_NAME));
    assert_eq!(free_path(&directory, ".."), directory.join(FALLBACK_NAME));
  }
}
