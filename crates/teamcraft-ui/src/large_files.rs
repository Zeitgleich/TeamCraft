//! Large files from the composer go to the client core without passing through memory whole
//! (issue #153). The core reads them one part at a time while they go up; how it gets at them
//! depends on where it runs:
//!
//! * In the browser (the core runs in the page), the picked `File` is registered with the core,
//!   which slices it and keeps a copy in IndexedDB so a reload continues the upload.
//! * In the desktop app (the core runs in the shell), the file is copied to the shell's staging
//!   directory in chunks over raw IPC requests, and the message names the copy's path. A file the
//!   shell knows by path (dropped on Linux, shared from the file manager) is copied by the shell
//!   itself and never enters the webview.
//!
//! Small files are read into memory as before: the composer needs them for its thumbnails, and a
//! single request is all they take.

use std::collections::HashMap;

use futures_util::FutureExt;
use futures_util::future::{LocalBoxFuture, Shared};
use teamcraft_client_api::actions::FileSource;
use wasm_bindgen_futures::JsFuture;

use crate::auth::tauri;
use crate::shell_drop::ShellFile;

/// Files up to this size are read whole (as large as one upload part in the core).
pub const READ_WHOLE_MAX_BYTES: u64 = 8 * 1024 * 1024;
/// Chunk of a file staged in the desktop app; below the shell's limit per request.
const STAGE_CHUNK_BYTES: u64 = 8 * 1024 * 1024;

/// A file too large to read into memory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LargeFile {
  /// The browser's handle on a file on disk.
  Picked(web_sys::File),
  /// A file the desktop shell knows by path.
  InShell(ShellFile),
}

/// A file's handover to the core, running or done: one per attached file, shared by the draft and
/// the send.
pub type Handover = Shared<LocalBoxFuture<'static, Result<FileSource, String>>>;

/// The handovers of a composer's attached files, by draft file ID (#2364). The draft starts one as
/// a file is attached; a send right after joins it instead of copying the file a second time, so
/// the message waits for one copy, behind its row, never for two.
pub type Handovers = Running<Result<FileSource, String>>;

/// Shared work by key: started once, joined by whoever asks for the same key.
pub struct Running<T: Clone + 'static> {
  by_key: HashMap<String, Shared<LocalBoxFuture<'static, T>>>,
}

impl<T: Clone + 'static> Default for Running<T> {
  fn default() -> Self {
    Self {
      by_key: HashMap::new(),
    }
  }
}

impl<T: Clone + 'static> Running<T> {
  /// The work for `key`: the running (or finished) one, else `start`ed now and kept.
  pub fn join(
    &mut self,
    key: &str,
    start: impl FnOnce() -> LocalBoxFuture<'static, T>,
  ) -> Shared<LocalBoxFuture<'static, T>> {
    self
      .by_key
      .entry(key.to_owned())
      .or_insert_with(|| start().shared())
      .clone()
  }

  /// Like [`Self::join`], and forgets `key`: its user is the last one.
  pub fn take(
    &mut self,
    key: &str,
    start: impl FnOnce() -> LocalBoxFuture<'static, T>,
  ) -> Shared<LocalBoxFuture<'static, T>> {
    self.by_key.remove(key).unwrap_or_else(|| start().shared())
  }

  /// Forgets `key` (its file was removed); a task still waiting for it gets its result.
  pub fn forget(&mut self, key: &str) {
    self.by_key.remove(key);
  }
}

/// Starts the handover of `file` ([`source_of`]) as a future that can be shared.
pub fn hand_over(file: LargeFile) -> LocalBoxFuture<'static, Result<FileSource, String>> {
  async move { source_of(&file).await }.boxed_local()
}

/// What the row of a message shows of a file still handed over ([`hand_over`]): its size. Never
/// sent; the send replaces it with the handover's source.
pub fn stand_in(file: &LargeFile) -> FileSource {
  let size_bytes = match file {
    LargeFile::Picked(file) => file.size() as u64,
    LargeFile::InShell(file) => file.size_bytes,
  };
  FileSource::Picked {
    handle: String::new(),
    size_bytes,
  }
}

/// Where the core reads `file` from. Fails (with a reason for the log) when the file can't be read
/// or the desktop app can't take it.
pub async fn source_of(file: &LargeFile) -> Result<FileSource, String> {
  match file {
    LargeFile::Picked(file) => source_of_picked(file).await,
    LargeFile::InShell(file) => {
      let args = serde_json::json!({ "id": file.id, "stage_id": stage_id() });
      let path = tauri::call::<String>("core_stage_dropped_file", &args).await?;
      Ok(FileSource::Path { path })
    },
  }
}

async fn source_of_picked(file: &web_sys::File) -> Result<FileSource, String> {
  if tauri::is_available() {
    return stage(file).await.map(|path| FileSource::Path { path });
  }
  #[cfg(target_arch = "wasm32")]
  if tauri::has_core() {
    let size_bytes = file.size() as u64;
    let handle = teamcraft_core::picked_files::register(file.clone().into()).await;
    return Ok(FileSource::Picked { handle, size_bytes });
  }
  // The demo backend has no core: it gets the bytes.
  let buffer = JsFuture::from(file.array_buffer())
    .await
    .map_err(|_| "the file could not be read".to_owned())?;
  Ok(FileSource::Bytes {
    bytes: js_sys::Uint8Array::new(&buffer).to_vec(),
  })
}

/// Copies `file` to the desktop app's staging directory chunk by chunk; returns the copy's path.
async fn stage(file: &web_sys::File) -> Result<String, String> {
  let id = stage_id();
  let size_bytes = file.size() as u64;
  let mut offset = 0;
  let mut maybe_path = None;
  while offset < size_bytes || maybe_path.is_none() {
    let end = (offset + STAGE_CHUNK_BYTES).min(size_bytes);
    // Invariant: file sizes are far below 2^53, so the offsets are exact as f64.
    let slice = file
      .slice_with_f64_and_f64(offset as f64, end as f64)
      .map_err(|_| "the file could not be sliced".to_owned())?;
    let buffer = JsFuture::from(slice.array_buffer())
      .await
      .map_err(|_| "the file could not be read".to_owned())?;
    let chunk = js_sys::Uint8Array::new(&buffer);
    let headers = [("x-stage-id", id.clone()), ("x-stage-offset", offset.to_string())];
    let answer = tauri::invoke_raw("core_stage_file", &chunk, &headers).await?;
    maybe_path = answer.as_string();
    offset = end;
  }
  maybe_path.ok_or_else(|| "the desktop app named no staged copy".to_owned())
}

/// A random name for a staged copy: 32 hex digits, as the shell expects.
fn stage_id() -> String {
  let mut bytes = [0u8; 16];
  if getrandom::getrandom(&mut bytes).is_err() {
    // Only the uniqueness of concurrent copies rests on it, never a secret.
    bytes = (js_sys::Date::now() as u128).to_le_bytes();
  }
  bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
  use std::cell::Cell;
  use std::rc::Rc;

  use super::*;

  #[test]
  fn a_send_joins_the_handover_the_draft_started() {
    let started = Rc::new(Cell::new(0));
    let mut running = Running::<u32>::default();
    let draft = running.join("file-1", counting(&started, 7));
    let send = running.take("file-1", counting(&started, 8));
    assert_eq!(started.get(), 1, "the file was handed over once");
    assert_eq!(draft.now_or_never(), Some(7));
    assert_eq!(send.now_or_never(), Some(7));
  }

  #[test]
  fn a_send_without_a_running_handover_starts_one() {
    let started = Rc::new(Cell::new(0));
    let mut running = Running::<u32>::default();
    assert_eq!(
      running.take("file-1", counting(&started, 3)).now_or_never(),
      Some(3)
    );
    // Taken: the next ask is a new file under the same key.
    assert_eq!(
      running.take("file-1", counting(&started, 4)).now_or_never(),
      Some(4)
    );
    assert_eq!(started.get(), 2);
  }

  #[test]
  fn joins_share_one_handover_and_forget_ends_it() {
    let started = Rc::new(Cell::new(0));
    let mut running = Running::<u32>::default();
    drop(running.join("file-1", counting(&started, 1)));
    drop(running.join("file-1", counting(&started, 2)));
    drop(running.join("file-2", counting(&started, 3)));
    assert_eq!(started.get(), 2);
    running.forget("file-1");
    assert_eq!(
      running.join("file-1", counting(&started, 5)).now_or_never(),
      Some(5)
    );
    assert_eq!(started.get(), 3);
  }

  fn counting(started: &Rc<Cell<u32>>, value: u32) -> impl FnOnce() -> LocalBoxFuture<'static, u32> {
    let started = Rc::clone(started);
    move || {
      started.set(started.get() + 1);
      async move { value }.boxed_local()
    }
  }
}
