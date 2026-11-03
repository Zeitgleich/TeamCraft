//! Large files from the composer go to the client core without passing through memory whole
//! (issue #153). The core reads them one part at a time while they go up; how it gets at them
//! depends on where it runs:
//!
//! * In the browser (the core runs in the page), the picked `File` is registered with the core,
//!   which slices it and keeps a copy in IndexedDB so a reload continues the upload.
//! * In the desktop app (the core runs in the shell), the file is copied to the shell's staging
//!   directory in chunks over raw IPC requests, and the message names the copy's path.
//!
//! Small files are read into memory as before: the composer needs them for its thumbnails, and a
//! single request is all they take.

use teamcraft_client_api::actions::FileSource;
use wasm_bindgen_futures::JsFuture;

use crate::auth::tauri;

/// Files up to this size are read whole (as large as one upload part in the core).
pub const READ_WHOLE_MAX_BYTES: u64 = 8 * 1024 * 1024;
/// Chunk of a file staged in the desktop app; below the shell's limit per request.
const STAGE_CHUNK_BYTES: u64 = 8 * 1024 * 1024;

/// Where the core reads `file` from. Fails (with a reason for the log) when the file can't be read
/// or the desktop app can't take it.
pub async fn source_of(file: &web_sys::File) -> Result<FileSource, String> {
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
