//! Opening a saved file, or showing it in the file manager, from the downloads list. The path comes
//! from the core's record of the download, never from the page. Files that would run as programs
//! when opened (an `.exe`, a shell script, a `.desktop` launcher) are shown in their folder
//! instead: they come from other members, and opening one must be a choice made in the file manager.

use std::path::Path;

use teamcraft_core::downloads::OpenRequest;

/// Extensions the desktop systems run instead of opening in a viewer (lowercase).
const RUNNABLE_EXTENSIONS: &[&str] = &[
  "app",
  "appimage",
  "application",
  "bat",
  "cmd",
  "com",
  "command",
  "cpl",
  "deb",
  "desktop",
  "dmg",
  "exe",
  "gadget",
  "hta",
  "inf",
  "jar",
  "js",
  "jse",
  "lnk",
  "msc",
  "msi",
  "msix",
  "msp",
  "pif",
  "pkg",
  "ps1",
  "psm1",
  "reg",
  "rpm",
  "run",
  "scf",
  "scr",
  "sh",
  "url",
  "vb",
  "vbe",
  "vbs",
  "workflow",
  "ws",
  "wsf",
  "wsh",
];

/// Opens or shows a saved file; a failure is logged (the file may be gone since).
pub fn open_saved(request: &OpenRequest) {
  let opened = if request.is_reveal || is_runnable(&request.path) {
    tauri_plugin_opener::reveal_item_in_dir(&request.path)
  } else {
    tauri_plugin_opener::open_path(&request.path, None::<&str>)
  };
  // Never the path: it names the file.
  if let Err(error) = opened {
    tracing::warn!(%error, is_reveal = request.is_reveal, "could not open a saved file");
  }
}

fn is_runnable(path: &Path) -> bool {
  path
    .extension()
    .and_then(|extension| extension.to_str())
    .is_some_and(|extension| RUNNABLE_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str()))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn programs_and_launchers_are_shown_not_run() {
    for name in [
      "setup.EXE",
      "install.sh",
      "app.desktop",
      "x.AppImage",
      "run.ps1",
      "a.lnk",
    ] {
      assert!(is_runnable(Path::new(name)), "{name}");
    }
    for name in ["report.pdf", "photo.jpg", "notes", "archive.tar.gz", ".bashrc"] {
      assert!(!is_runnable(Path::new(name)), "{name}");
    }
  }
}
