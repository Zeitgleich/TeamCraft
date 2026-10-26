//! Pasting files and pictures into the composer (#761). What a paste carries depends on the app
//! that copied and on the webview: the files themselves, a picture only as a clipboard item, copied
//! files only as their `file://` addresses, or nothing the page can read. [`PasteKind`] decides
//! from the paste event; what the page can't read, the desktop shell reads from the system
//! clipboard (apps/desktop/src/clipboard_paste.rs).

use wasm_bindgen::JsCast;

use crate::auth::tauri;
use crate::components::file_drop::{DroppedFiles, attach_from_shell};
use crate::i18n::I18n;
use crate::reactive::SetIfAlive;
use crate::state::AppState;

/// The name of a pasted picture that has none of its own.
const PASTED_PICTURE_NAME: &str = "image.png";

/// What a paste brings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasteKind {
  /// Files (or pictures) the page got with the event.
  Files,
  /// Copied files named only by their addresses: the shell reads them (desktop app).
  CopiedFiles,
  /// Probably a picture the event didn't carry: the shell reads it (desktop app).
  Picture,
  /// Text (or nothing the shell could add).
  Text,
}

impl PasteKind {
  /// Decides from the event's data: whether it has files, its `text/uri-list`, `text/plain` and
  /// `text/html`.
  pub fn classify(
    has_files: bool,
    is_desktop_app: bool,
    maybe_uri_list: Option<&str>,
    plain: &str,
    maybe_html: Option<&str>,
  ) -> Self {
    if has_files {
      return Self::Files;
    }
    if !is_desktop_app {
      return Self::Text;
    }
    let names_files = |text: &str| {
      let uris: Vec<&str> = uri_lines(text).collect();
      !uris.is_empty() && uris.iter().all(|uri| is_file_uri(uri))
    };
    // KDE puts the copied files' addresses into `text/plain` too.
    if maybe_uri_list.is_some_and(names_files) || names_files(plain) {
      return Self::CopiedFiles;
    }
    // "Copy image" in a browser may arrive as an `<img>` alone; a screenshot tool's copy as nothing.
    let has_text = !plain.trim().is_empty();
    let is_only_pictures = maybe_html.is_none_or(html_is_only_pictures);
    match !has_text && is_only_pictures {
      true => Self::Picture,
      false => Self::Text,
    }
  }
}

/// Reads what the paste didn't carry from the system clipboard and hands it to `target` (the
/// composer): copied files like dropped ones, else a picture as PNG. Nothing on the clipboard
/// pastes nothing; too large a picture says so.
pub fn paste_from_shell(kind: PasteKind, target: DroppedFiles, state: AppState, i18n: I18n) {
  leptos::task::spawn_local(async move {
    if kind == PasteKind::CopiedFiles {
      match tauri::call::<usize>("clipboard_paste_files", &serde_json::json!({})).await {
        Ok(count) if count > 0 => {
          attach_from_shell(Some(target), true, state, i18n).await;
          return;
        },
        Ok(_) => {},
        Err(error) => tracing::warn!(%error, "could not read the copied files"),
      }
    }
    match read_picture().await {
      Ok(Some(picture)) => target.0.set_if_alive(vec![picture]),
      Ok(None) => {},
      Err(error) => state.show_error(i18n.t("paste-picture-failed"), error),
    }
  });
}

/// The clipboard's picture as a PNG `File`; `None` without one.
async fn read_picture() -> Result<Option<web_sys::File>, String> {
  let answer = tauri::invoke("clipboard_read_image", js_sys::Object::new().into())
    .await
    .map_err(|error| error.to_string())?;
  if !answer.is_instance_of::<js_sys::ArrayBuffer>() && !js_sys::Array::is_array(&answer) {
    return Err("unreadable answer of clipboard_read_image".to_owned());
  }
  let bytes = js_sys::Uint8Array::new(&answer);
  if bytes.length() == 0 {
    return Ok(None);
  }
  let options = web_sys::FilePropertyBag::new();
  options.set_type("image/png");
  web_sys::File::new_with_u8_array_sequence_and_options(
    &js_sys::Array::of1(&bytes),
    PASTED_PICTURE_NAME,
    &options,
  )
  .map(Some)
  .map_err(|_| "could not make a file of the pasted picture".to_owned())
}

/// The URIs of a `text/uri-list` (or of text that is one): lines without comments and blanks.
fn uri_lines(text: &str) -> impl Iterator<Item = &str> {
  text
    .lines()
    .map(str::trim)
    .filter(|line| !line.is_empty() && !line.starts_with('#'))
}

fn is_file_uri(uri: &str) -> bool {
  uri
    .get(..7)
    .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file://"))
}

/// HTML with pictures (or nothing) and no text of its own, as a browser's "Copy image" gives it.
fn html_is_only_pictures(html: &str) -> bool {
  let mut text = String::new();
  let mut is_in_tag = false;
  for character in html.chars() {
    match character {
      '<' => is_in_tag = true,
      '>' => is_in_tag = false,
      _ if !is_in_tag => text.push(character),
      _ => {},
    }
  }
  let text = text.replace("&nbsp;", " ");
  text.trim().is_empty()
}

#[cfg(test)]
mod tests {
  use super::*;

  fn classify(has_files: bool, uri_list: Option<&str>, plain: &str, html: Option<&str>) -> PasteKind {
    PasteKind::classify(has_files, true, uri_list, plain, html)
  }

  #[test]
  fn files_in_the_event_attach_as_they_are() {
    assert_eq!(
      classify(true, Some("file:///tmp/a.png"), "text", None),
      PasteKind::Files
    );
    assert_eq!(PasteKind::classify(true, false, None, "", None), PasteKind::Files);
  }

  #[test]
  fn copied_files_are_read_by_the_shell() {
    // Dolphin: a URI list, and the same addresses as text.
    let list = "file:///home/user/Pictures/beach%20day.jpg\r\nfile:///home/user/notes.txt\r\n";
    assert_eq!(classify(false, Some(list), list, None), PasteKind::CopiedFiles);
    assert_eq!(
      classify(false, None, "file:///tmp/a.png", None),
      PasteKind::CopiedFiles
    );
    assert_eq!(
      classify(false, Some("# copied\nFILE:///tmp/a.png"), "", None),
      PasteKind::CopiedFiles
    );
  }

  #[test]
  fn a_paste_without_data_or_with_only_a_picture_reads_the_clipboard_picture() {
    // Spectacle, Flameshot: nothing the page can read.
    assert_eq!(classify(false, None, "", None), PasteKind::Picture);
    // A browser's "Copy image" without the picture itself.
    assert_eq!(
      classify(
        false,
        None,
        "",
        Some("<meta charset='utf-8'><img src=\"https://example.com/a.png\">")
      ),
      PasteKind::Picture
    );
    assert_eq!(
      classify(false, None, "  ", Some("<p>&nbsp;</p>")),
      PasteKind::Picture
    );
  }

  #[test]
  fn text_stays_text() {
    assert_eq!(classify(false, None, "hello", None), PasteKind::Text);
    assert_eq!(classify(false, None, "", Some("<b>hello</b>")), PasteKind::Text);
    // A link, or text that only mentions a file address among others.
    assert_eq!(
      classify(false, Some("https://example.com/"), "https://example.com/", None),
      PasteKind::Text
    );
    assert_eq!(
      classify(false, None, "see file:///tmp/a.png", None),
      PasteKind::Text
    );
    assert_eq!(
      classify(false, None, "file:///tmp/a.png\nhttps://example.com/", None),
      PasteKind::Text
    );
  }

  #[test]
  fn the_browser_reads_its_own_clipboard() {
    assert_eq!(PasteKind::classify(false, false, None, "", None), PasteKind::Text);
    assert_eq!(
      PasteKind::classify(false, false, None, "file:///tmp/a.png", None),
      PasteKind::Text
    );
  }
}
