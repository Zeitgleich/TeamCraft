//! Links leave the app through the system's browser or mail client, and only web pages and mail
//! addresses do (never `file:`, `tauri:` or other schemes a member could put in a message).

use tauri::{AppHandle, Url};
use tauri_plugin_opener::OpenerExt;

use teamcraft_client_api::links::is_external_link;

/// Opens a link the user clicked. The page routes clicks here because the webview's own
/// new-window hook does not fire for `target="_blank"` on every platform (#548); errors reach the
/// user through the page.
#[tauri::command(rename_all = "snake_case")]
pub fn open_external(app: AppHandle, url: String) -> Result<(), String> {
  open(&app, &url)
}

/// macOS's settings page for Screen & System Audio Recording, where a refused share is fixed (#940).
#[cfg(target_os = "macos")]
const SCREEN_RECORDING_SETTINGS: &str =
  "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture";

/// Opens the system's Screen Recording settings for a share macOS refused (#940). The address is
/// fixed here: the page can't hand a settings link of its own to the opener.
#[tauri::command(rename_all = "snake_case")]
pub fn open_screen_recording_settings(app: AppHandle) -> Result<(), String> {
  #[cfg(target_os = "macos")]
  {
    app
      .opener()
      .open_url(SCREEN_RECORDING_SETTINGS, None::<&str>)
      .map_err(|error| error.to_string())
  }
  #[cfg(not(target_os = "macos"))]
  {
    let _ = app;
    Err("screen recording settings exist only on macOS".to_owned())
  }
}

/// Hands `url` to the system if it is a web or mail link.
pub fn open(app: &AppHandle, url: &str) -> Result<(), String> {
  let url = checked(url)?;
  app
    .opener()
    .open_url(url.as_str(), None::<&str>)
    .map_err(|error| error.to_string())
}

/// The normalized address, or why it must not leave the app.
fn checked(url: &str) -> Result<Url, String> {
  if !is_external_link(url) {
    return Err("only web and mail links can be opened".to_owned());
  }
  let parsed: Url = url
    .parse()
    .map_err(|_| "the link is not a valid address".to_owned())?;
  // Re-checked on the parsed form: the scheme the system sees is the one that counts.
  if !is_external_link(parsed.as_str()) {
    return Err("only web and mail links can be opened".to_owned());
  }
  Ok(parsed)
}

#[cfg(test)]
mod tests {
  use super::checked;

  #[test]
  fn web_and_mail_links_are_opened() {
    for url in [
      "https://example.com/a?b=c#d",
      "http://example.com/",
      "mailto:a@example.com",
    ] {
      assert!(checked(url).is_ok(), "{url}");
    }
  }

  #[test]
  fn everything_else_is_refused() {
    for url in [
      "file:///etc/passwd",
      "tauri://localhost/",
      "about:blank",
      "javascript:alert(1)",
      "search-ms:query=x",
      " https://example.com",
      "https://",
      "",
    ] {
      assert!(checked(url).is_err(), "{url}");
    }
  }
}
