//! Files shared to the app from outside (#208): `teamcraft-desktop --share -- <paths>` from the file
//! manager's "Send to" or "Open with", and macOS "Open With".
//!
//! The arguments arrive at a cold start or, when the app runs, through the single-instance plugin.
//! The paths wait in [`DroppedFilesState`] until the UI asks: at a cold start the webview doesn't
//! exist yet, and the signed-in app may be a sign-in away. The event only says "there is something";
//! neither paths nor file names go out in an event, and nothing is uploaded before the user picked
//! a conversation and pressed send.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager, State};
use tracing::debug;

use crate::dropped_files::{DroppedFilesState, SharedFiles};
use crate::tray;

/// Everything after this argument is a file to share.
const SHARE_ARG: &str = "--share";
/// Tells the page that files wait to be shared (`SHARED_FILES_EVENT` in the UI's `share_into`).
const SHARED_FILES_EVENT: &str = "shared-files";

/// Takes the files the app was started with. Runs in `setup`.
pub fn collect(app: &AppHandle) {
  let args: Vec<OsString> = std::env::args_os().collect();
  let working_directory = std::env::current_dir().unwrap_or_default();
  let paths = share_paths(&args, &working_directory);
  if !paths.is_empty() {
    receive(app, paths);
    // A share is a request to see the window, also with "start minimized" on.
    tray::show_main_window(app);
  }
}

/// A second launch handed its arguments to this instance.
pub fn receive_args(app: &AppHandle, args: &[String], working_directory: &Path) {
  let args: Vec<OsString> = args.iter().map(OsString::from).collect();
  let paths = share_paths(&args, working_directory);
  if !paths.is_empty() {
    receive(app, paths);
  }
}

/// macOS hands opened files to the running app as `file:` URLs.
#[cfg(target_os = "macos")]
pub fn receive_opened(app: &AppHandle, urls: &[url::Url]) {
  let paths = crate::dropped_files::local_paths(urls.iter().map(url::Url::as_str));
  if !paths.is_empty() {
    receive(app, paths);
    tray::show_main_window(app);
  }
}

/// The waiting share; it stays until the UI says it is finished.
#[tauri::command(rename_all = "snake_case")]
pub async fn shared_files_list(store: State<'_, Arc<DroppedFilesState>>) -> Result<SharedFiles, String> {
  let store = store.inner().clone();
  // File metadata can be slow (network shares); not on the main thread.
  tauri::async_runtime::spawn_blocking(move || store.waiting_share())
    .await
    .map_err(|error| error.to_string())
}

/// The user chose where the waiting share goes, or discarded it.
#[tauri::command(rename_all = "snake_case")]
pub fn shared_files_finish(store: State<'_, Arc<DroppedFilesState>>) {
  store.finish_share();
}

/// Resolves the paths off the main thread: all three callers run on it, and a hung network mount
/// must not freeze the window. The page hears of the share once that is done.
fn receive(app: &AppHandle, paths: Vec<PathBuf>) {
  debug!(count = paths.len(), "files shared to the app");
  let store = app.state::<Arc<DroppedFilesState>>().inner().clone();
  let app = app.clone();
  tauri::async_runtime::spawn_blocking(move || {
    store.shared(paths);
    if let Err(error) = app.emit_to(tray::MAIN_WINDOW, SHARED_FILES_EVENT, "") {
      debug!(%error, "could not tell the webview about shared files");
    }
  });
}

/// The paths after `--share`, absolute: a second launch started elsewhere names them relative to its
/// own working directory. Only as the first argument: arguments mean nothing else to this app, and
/// nothing else may turn into a share. A `--` after it is skipped: launchers pass it so that the
/// deep-link plugin, which reads the first arguments as `String`s at every start, never reaches a
/// path that isn't UTF-8 and panics.
fn share_paths(args: &[OsString], working_directory: &Path) -> Vec<PathBuf> {
  let mut rest = args.iter().skip(1).peekable();
  if rest.next().map(OsString::as_os_str) != Some(OsStr::new(SHARE_ARG)) {
    return Vec::new();
  }
  rest.next_if(|arg| arg.as_os_str() == OsStr::new("--"));
  rest.map(|arg| working_directory.join(arg)).collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
  }

  #[test]
  fn only_what_follows_a_leading_share_argument_is_shared() {
    let directory = Path::new("/home/someone");
    assert!(share_paths(&args(&["teamcraft"]), directory).is_empty());
    assert!(share_paths(&args(&["teamcraft", "teamcraft://open/x"]), directory).is_empty());
    assert!(share_paths(&args(&["teamcraft", "--share"]), directory).is_empty());
    assert!(
      share_paths(
        &args(&["teamcraft", "--autostart", "--share", "a.txt"]),
        directory
      )
      .is_empty(),
      "not as a later argument"
    );
    assert_eq!(
      share_paths(
        &args(&["teamcraft", "--share", "a.txt", "/tmp/b c.pdf"]),
        directory
      ),
      [
        PathBuf::from("/home/someone/a.txt"),
        PathBuf::from("/tmp/b c.pdf")
      ]
    );
  }

  #[test]
  fn one_separator_is_skipped_and_a_file_may_be_called_like_one() {
    let directory = Path::new("/d");
    assert_eq!(
      share_paths(&args(&["teamcraft", "--share", "--", "a", "--"]), directory),
      [PathBuf::from("/d/a"), PathBuf::from("/d/--")]
    );
  }

  #[cfg(unix)]
  #[test]
  fn a_path_that_is_not_utf8_is_kept() {
    use std::os::unix::ffi::OsStringExt;
    let odd = OsString::from_vec(vec![b'a', 0xff]);
    let all = [
      OsString::from("teamcraft"),
      OsString::from("--share"),
      OsString::from("--"),
      odd.clone(),
    ];
    assert_eq!(share_paths(&all, Path::new("/d")), [Path::new("/d").join(odd)]);
  }
}
