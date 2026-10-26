//! Files the desktop shell saw dropped on the window (#651). WebKitGTK hands the page a drop from a
//! file manager without its files; the shell keeps the dropped paths and the page reads the files
//! from it chunk by chunk, so they attach like picked ones. The page names no path: it can only read
//! what was just dropped (apps/desktop/src/dropped_files.rs).

use serde::Deserialize;
use wasm_bindgen::JsCast;

use crate::auth::tauri;
use crate::media_formats::media_type_for;

/// The shell's event when files were dropped but the page got no `drop` for them.
pub const FILES_DROPPED_EVENT: &str = "files-dropped";
/// Largest file the shell lets the page read (`READ_MAX_BYTES` in the shell).
pub const READ_MAX_BYTES: u64 = 512 * 1024 * 1024;

/// A drop the shell read for the page.
#[derive(Default)]
pub struct ShellDrop {
  pub files: Vec<web_sys::File>,
  /// Names of files above the limit, left out.
  pub too_large: Vec<String>,
  /// Whether the shell knew of a drop at all.
  pub was_dropped: bool,
}

#[derive(Deserialize)]
struct DroppedFile {
  id: u32,
  name: String,
  size_bytes: u64,
}

/// Reads the files of the drop that just happened; files above `max_size_bytes` are only named.
pub async fn take(max_size_bytes: u64) -> Result<ShellDrop, String> {
  let listed: Vec<DroppedFile> =
    tauri::call("dropped_files_take", &serde_json::json!({ "is_handled": false })).await?;
  let mut taken = ShellDrop {
    was_dropped: !listed.is_empty(),
    ..ShellDrop::default()
  };
  for file in listed {
    if file.size_bytes > max_size_bytes {
      taken.too_large.push(file.name);
      continue;
    }
    taken.files.push(read(&file).await?);
  }
  Ok(taken)
}

/// The page attached the dropped files itself: the shell forgets them and sends no fallback event.
pub fn forget() {
  leptos::task::spawn_local(async {
    let args = serde_json::json!({ "is_handled": true });
    if let Err(error) = tauri::call::<Vec<serde_json::Value>>("dropped_files_take", &args).await {
      tracing::debug!(%error, "could not tell the shell the drop was handled");
    }
  });
}

/// One dropped file, read chunk by chunk into a `File`.
async fn read(file: &DroppedFile) -> Result<web_sys::File, String> {
  let parts = js_sys::Array::new();
  let mut offset = 0_u64;
  while offset < file.size_bytes {
    let chunk = read_chunk(file.id, offset).await?;
    if chunk.length() == 0 {
      // The file got shorter while it was read.
      break;
    }
    offset += u64::from(chunk.length());
    parts.push(&chunk);
  }
  let options = web_sys::FilePropertyBag::new();
  options.set_type(&media_type_for(&file.name, ""));
  web_sys::File::new_with_u8_array_sequence_and_options(&parts, &file.name, &options)
    .map_err(|_| "could not make a file of the dropped bytes".to_owned())
}

/// The shell answers with the raw bytes as an `ArrayBuffer` (an array of bytes where the bridge
/// falls back to JSON).
async fn read_chunk(id: u32, offset: u64) -> Result<js_sys::Uint8Array, String> {
  let args = serde_json::json!({ "id": id, "offset": offset });
  let js_args =
    js_sys::JSON::parse(&args.to_string()).map_err(|_| "could not encode the arguments".to_owned())?;
  let answer = tauri::invoke("dropped_file_read", js_args)
    .await
    .map_err(|error| error.to_string())?;
  if !answer.is_instance_of::<js_sys::ArrayBuffer>() && !js_sys::Array::is_array(&answer) {
    return Err("unreadable answer of dropped_file_read".to_owned());
  }
  Ok(js_sys::Uint8Array::new(&answer))
}
