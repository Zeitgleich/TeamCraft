//! "Copy image" on Linux (#719): WebKitGTK's web Clipboard API doesn't write pictures reliably, so
//! the page hands the picture to the shell, which puts it on the GTK clipboard. GTK serves it on
//! X11 and Wayland alike, for as long as the app runs (a clipboard manager such as Klipper keeps a
//! copy after that). The page converts every format to PNG first; a raster original (JPEG, GIF, …)
//! comes along and is offered beside the PNG, so an app that takes it gets the file unchanged.
//! macOS and Windows copy through the web Clipboard API of their webviews (copy_image.rs in the UI).

use std::borrow::Cow;

use tauri::ipc::{InvokeBody, Request};

/// Header: how many bytes of the body are the PNG (the original follows).
const PNG_LENGTH_HEADER: &str = "x-png-bytes";
/// Header: the original's media type.
const ORIGINAL_TYPE_HEADER: &str = "x-original-type";
/// Largest picture (PNG and original together) the page may put on the clipboard.
const MAX_CLIPBOARD_BYTES: usize = 128 * 1024 * 1024;
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// A picture for the clipboard: the PNG, and maybe the original with its media type.
#[derive(Debug, PartialEq, Eq)]
pub struct ClipboardPicture {
  pub png: Vec<u8>,
  pub maybe_original: Option<(String, Vec<u8>)>,
}

/// Why a picture can't go on the clipboard.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ClipboardError {
  #[error("the picture is sent as raw bytes")]
  NotRaw,
  #[error("the picture is larger than the clipboard takes")]
  TooLarge,
  #[error("the PNG length header is missing or wrong")]
  BadLength,
  #[error("the picture is not a PNG")]
  NotPng,
  #[error("the original's type is not a picture the clipboard offers")]
  BadOriginalType,
  #[cfg_attr(target_os = "linux", allow(dead_code))]
  #[error("this platform copies pictures in the page")]
  Unsupported,
  #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
  #[error("the clipboard could not be reached")]
  Unavailable,
}

/// Puts the picture in the raw body on the clipboard (Linux).
#[tauri::command(rename_all = "snake_case")]
pub async fn clipboard_write_image(app: tauri::AppHandle, request: Request<'_>) -> Result<(), String> {
  let InvokeBody::Raw(body) = request.body() else {
    return Err(ClipboardError::NotRaw.to_string());
  };
  let header = |name: &str| request.headers().get(name).and_then(|value| value.to_str().ok());
  let picture = parse(body, header(PNG_LENGTH_HEADER), header(ORIGINAL_TYPE_HEADER))
    .map_err(|error| error.to_string())?;
  place(&app, picture).await.map_err(|error| error.to_string())
}

/// Splits and checks the page's body: the PNG first (`png_length` bytes), then the original of
/// `maybe_original_type`, if any.
pub fn parse(
  body: &[u8],
  maybe_png_length: Option<&str>,
  maybe_original_type: Option<&str>,
) -> Result<ClipboardPicture, ClipboardError> {
  if body.len() > MAX_CLIPBOARD_BYTES {
    return Err(ClipboardError::TooLarge);
  }
  let png_length: usize = maybe_png_length
    .and_then(|length| length.trim().parse().ok())
    .filter(|length| *length <= body.len())
    .ok_or(ClipboardError::BadLength)?;
  let (png, rest) = body.split_at(png_length);
  if !png.starts_with(PNG_SIGNATURE) {
    return Err(ClipboardError::NotPng);
  }
  let maybe_original = match maybe_original_type {
    None if rest.is_empty() => None,
    None => return Err(ClipboardError::BadLength),
    Some(_) if rest.is_empty() => return Err(ClipboardError::BadLength),
    Some(media_type) => Some((offered_type(media_type)?.into_owned(), rest.to_vec())),
  };
  Ok(ClipboardPicture {
    png: png.to_vec(),
    maybe_original,
  })
}

/// The original's type as a clipboard target: a plain `image/…` type other than PNG and SVG.
fn offered_type(media_type: &str) -> Result<Cow<'_, str>, ClipboardError> {
  let media_type = media_type.trim();
  let Some(subtype) = media_type.strip_prefix("image/") else {
    return Err(ClipboardError::BadOriginalType);
  };
  let is_token = !subtype.is_empty()
    && subtype
      .chars()
      .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '+' | '-'));
  if !is_token || subtype == "png" || subtype.starts_with("svg") {
    return Err(ClipboardError::BadOriginalType);
  }
  Ok(Cow::Borrowed(media_type))
}

#[cfg(target_os = "linux")]
async fn place(app: &tauri::AppHandle, picture: ClipboardPicture) -> Result<(), ClipboardError> {
  let (sender, receiver) = tokio::sync::oneshot::channel();
  // GTK lives on the main thread.
  app
    .run_on_main_thread(move || drop(sender.send(set_on_gtk_clipboard(picture))))
    .map_err(|_| ClipboardError::Unavailable)?;
  receiver.await.map_err(|_| ClipboardError::Unavailable)?
}

#[cfg(not(target_os = "linux"))]
async fn place(_app: &tauri::AppHandle, _picture: ClipboardPicture) -> Result<(), ClipboardError> {
  Err(ClipboardError::Unsupported)
}

/// Offers `image/png` and the original's type on the clipboard; GTK asks for the bytes when another
/// app pastes.
#[cfg(target_os = "linux")]
fn set_on_gtk_clipboard(picture: ClipboardPicture) -> Result<(), ClipboardError> {
  use gtk::{TargetEntry, TargetFlags};

  const PNG_INFO: u32 = 0;
  const ORIGINAL_INFO: u32 = 1;
  let mut targets = vec![TargetEntry::new("image/png", TargetFlags::OTHER_APP, PNG_INFO)];
  if let Some((media_type, _)) = &picture.maybe_original {
    targets.push(TargetEntry::new(
      media_type,
      TargetFlags::OTHER_APP,
      ORIGINAL_INFO,
    ));
  }
  let clipboard = gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD);
  let is_set = clipboard.set_with_data(&targets, move |_, selection, info| {
    let (media_type, bytes) = match (&picture.maybe_original, info) {
      (Some((media_type, bytes)), ORIGINAL_INFO) => (media_type.as_str(), bytes.as_slice()),
      _ => ("image/png", picture.png.as_slice()),
    };
    selection.set(&gtk::gdk::Atom::intern(media_type), 8, bytes);
  });
  match is_set {
    true => Ok(()),
    false => Err(ClipboardError::Unavailable),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const PNG: &[u8] = b"\x89PNG\r\n\x1a\nIHDR-and-the-rest";

  #[test]
  fn a_png_alone_is_taken() {
    let length = PNG.len().to_string();
    assert_eq!(
      parse(PNG, Some(&length), None),
      Ok(ClipboardPicture {
        png: PNG.to_vec(),
        maybe_original: None
      })
    );
  }

  #[test]
  fn the_original_follows_the_png() {
    let body = [PNG, b"\xff\xd8\xffjpeg"].concat();
    let length = PNG.len().to_string();
    let picture = parse(&body, Some(&length), Some("image/jpeg")).expect("valid");
    assert_eq!(picture.png, PNG);
    assert_eq!(
      picture.maybe_original,
      Some(("image/jpeg".to_owned(), b"\xff\xd8\xffjpeg".to_vec()))
    );
  }

  #[test]
  fn malformed_bodies_are_refused() {
    let length = PNG.len().to_string();
    assert_eq!(parse(PNG, None, None), Err(ClipboardError::BadLength));
    assert_eq!(parse(PNG, Some("x"), None), Err(ClipboardError::BadLength));
    assert_eq!(parse(PNG, Some("999"), None), Err(ClipboardError::BadLength));
    assert_eq!(
      parse(b"GIF89a....", Some("10"), None),
      Err(ClipboardError::NotPng)
    );
    // Bytes after the PNG without a type, or a type without bytes.
    assert_eq!(
      parse(&[PNG, b"x"].concat(), Some(&length), None),
      Err(ClipboardError::BadLength)
    );
    assert_eq!(
      parse(PNG, Some(&length), Some("image/jpeg")),
      Err(ClipboardError::BadLength)
    );
    let too_large = vec![0_u8; MAX_CLIPBOARD_BYTES + 1];
    assert_eq!(parse(&too_large, Some("8"), None), Err(ClipboardError::TooLarge));
  }

  #[test]
  fn only_raster_originals_are_offered() {
    let body = [PNG, b"data"].concat();
    let length = PNG.len().to_string();
    for refused in [
      "image/svg+xml",
      "image/png",
      "text/html",
      "image/",
      "image/jpeg\r\nx",
      "image/a b",
    ] {
      assert_eq!(
        parse(&body, Some(&length), Some(refused)),
        Err(ClipboardError::BadOriginalType),
        "{refused:?}"
      );
    }
    for offered in [
      "image/gif",
      "image/webp",
      "image/heic",
      "image/vnd.microsoft.icon",
    ] {
      assert!(parse(&body, Some(&length), Some(offered)).is_ok(), "{offered}");
    }
  }
}
