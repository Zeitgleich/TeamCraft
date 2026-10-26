//! Pasting what the page's `paste` event didn't carry (#761). Depending on the app that copied it,
//! WebKitGTK hands the page a picture or copied files without their contents (or nothing at all),
//! so the composer asks the shell, which reads the system clipboard itself:
//!
//! - copied files (Dolphin, GNOME Files: `text/uri-list`) become a drop the page reads like dropped
//!   files (dropped_files.rs), so the page never names a path;
//! - a picture (a screenshot tool, "Copy image" in a browser, GIMP) comes back as PNG.
//!
//! Linux reads the GTK clipboard (X11 and Wayland); macOS and Windows have no copied-files list
//! here and read pictures through the clipboard plugin.

use std::sync::Arc;
use std::time::Instant;

use tauri::State;
use tauri::ipc::Response;

use crate::dropped_files::DroppedFilesState;

/// Largest picture the shell hands over; a bigger one is refused with a message.
pub const MAX_PASTED_IMAGE_BYTES: usize = 64 * 1024 * 1024;

/// Copied files on the system clipboard become readable like a drop; answers how many.
#[tauri::command(rename_all = "snake_case")]
pub async fn clipboard_paste_files(
  app: tauri::AppHandle,
  store: State<'_, Arc<DroppedFilesState>>,
) -> Result<usize, String> {
  let uris = copied_uris(&app).await?;
  let paths = crate::dropped_files::local_paths(uris.iter().map(String::as_str));
  if paths.is_empty() {
    return Ok(0);
  }
  let count = paths.len();
  store.dropped(paths, Instant::now());
  Ok(count)
}

/// The picture on the system clipboard as PNG; empty when there is none.
#[tauri::command(rename_all = "snake_case")]
pub async fn clipboard_read_image(app: tauri::AppHandle) -> Result<Response, String> {
  let png = clipboard_png(&app).await?;
  if png.len() > MAX_PASTED_IMAGE_BYTES {
    return Err("the picture on the clipboard is too large to paste".to_owned());
  }
  Ok(Response::new(png))
}

#[cfg(target_os = "linux")]
async fn copied_uris(app: &tauri::AppHandle) -> Result<Vec<String>, String> {
  let (sender, receiver) = tokio::sync::oneshot::channel();
  // GTK lives on the main thread; the clipboard answers in a callback.
  app
    .run_on_main_thread(move || {
      gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD).request_uris(move |_, uris| {
        drop(sender.send(uris.iter().map(ToString::to_string).collect()));
      });
    })
    .map_err(|error| error.to_string())?;
  receiver
    .await
    .map_err(|_| "the clipboard did not answer".to_owned())
}

#[cfg(not(target_os = "linux"))]
async fn copied_uris(_app: &tauri::AppHandle) -> Result<Vec<String>, String> {
  Ok(Vec::new())
}

#[cfg(target_os = "linux")]
async fn clipboard_png(app: &tauri::AppHandle) -> Result<Vec<u8>, String> {
  let (sender, receiver) = tokio::sync::oneshot::channel();
  app
    .run_on_main_thread(move || {
      gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD).request_image(move |_, maybe_image| {
        let png = match maybe_image {
          Some(image) => image
            .save_to_bufferv("png", &[])
            .map_err(|error| error.to_string()),
          None => Ok(Vec::new()),
        };
        drop(sender.send(png));
      });
    })
    .map_err(|error| error.to_string())?;
  receiver
    .await
    .map_err(|_| "the clipboard did not answer".to_owned())?
}

#[cfg(not(target_os = "linux"))]
async fn clipboard_png(app: &tauri::AppHandle) -> Result<Vec<u8>, String> {
  use tauri_plugin_clipboard_manager::ClipboardExt;

  // No picture on the clipboard reads as an error of the plugin: nothing to paste.
  let Ok(image) = app.clipboard().read_image() else {
    return Ok(Vec::new());
  };
  encode_png(image.rgba(), image.width(), image.height())
}

/// RGBA pixels as a PNG.
#[cfg(any(not(target_os = "linux"), test))]
fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
  let picture = image::RgbaImage::from_raw(width, height, rgba.to_vec())
    .ok_or_else(|| "the clipboard's picture has the wrong size".to_owned())?;
  let mut png = std::io::Cursor::new(Vec::new());
  picture
    .write_to(&mut png, image::ImageFormat::Png)
    .map_err(|error| error.to_string())?;
  Ok(png.into_inner())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn clipboard_pixels_become_a_png() {
    let rgba = [255_u8, 0, 0, 255].repeat(4);
    let png = encode_png(&rgba, 2, 2).expect("encodes");
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert!(encode_png(&rgba, 3, 3).is_err(), "too few pixels for the size");
  }
}
