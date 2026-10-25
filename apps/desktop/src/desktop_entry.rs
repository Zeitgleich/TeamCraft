//! The `.desktop` entry the packages installed for this app. Notifications name it (`desktop-entry`
//! hint) so the server shows the app's name and icon, and the launcher badge addresses it
//! (`application://<entry>.desktop`) so the task manager finds the app's icon.

use std::path::PathBuf;
use std::sync::OnceLock;

use tauri::AppHandle;

/// The installed entry's name without `.desktop`, looked up once: the Flatpak's ID, the identifier,
/// the product name (Tauri's deb, rpm and AppImage name it `TeamCraft.desktop`) or the executable's
/// name. `None` when none is installed (a development build).
pub fn installed(app: &AppHandle) -> Option<&'static str> {
  static ENTRY: OnceLock<Option<String>> = OnceLock::new();
  ENTRY
    .get_or_init(|| {
      let config = app.config();
      let mut candidates: Vec<String> = std::env::var("FLATPAK_ID").into_iter().collect();
      candidates.push(config.identifier.clone());
      candidates.extend(config.product_name.clone());
      candidates.extend(
        std::env::current_exe()
          .ok()
          .and_then(|path| path.file_stem().map(|stem| stem.to_string_lossy().into_owned())),
      );
      let entry = first_installed(&candidates, &application_dirs());
      if entry.is_none() {
        tracing::debug!("no installed .desktop entry");
      }
      entry
    })
    .as_deref()
}

/// The first candidate with a `<name>.desktop` in one of `dirs`.
fn first_installed(candidates: &[String], dirs: &[PathBuf]) -> Option<String> {
  candidates
    .iter()
    .find(|name| {
      dirs
        .iter()
        .any(|dir| dir.join(format!("{name}.desktop")).is_file())
    })
    .cloned()
}

/// `applications` below the XDG data directories, user first, as desktop environments search.
fn application_dirs() -> Vec<PathBuf> {
  let home = std::env::var_os("HOME").map(PathBuf::from);
  let data_home = std::env::var_os("XDG_DATA_HOME")
    .map(PathBuf::from)
    .or_else(|| home.as_ref().map(|home| home.join(".local/share")));
  let data_dirs = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".to_owned());
  data_home
    .into_iter()
    .chain(
      data_dirs
        .split(':')
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from),
    )
    .map(|dir| dir.join("applications"))
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_desktop_entry_is_the_one_actually_installed() {
    let dir = std::env::temp_dir().join(format!("teamcraft-desktop-entry-{}", std::process::id()));
    let applications = dir.join("applications");
    std::fs::create_dir_all(&applications).unwrap();
    let candidates = ["de.zeitgleich.teamcraft".to_owned(), "TeamCraft".to_owned()];
    assert_eq!(
      first_installed(&candidates, std::slice::from_ref(&applications)),
      None
    );
    std::fs::write(applications.join("TeamCraft.desktop"), "[Desktop Entry]\n").unwrap();
    assert_eq!(
      first_installed(&candidates, std::slice::from_ref(&applications)).as_deref(),
      Some("TeamCraft")
    );
    std::fs::write(
      applications.join("de.zeitgleich.teamcraft.desktop"),
      "[Desktop Entry]\n",
    )
    .unwrap();
    assert_eq!(
      first_installed(&candidates, &[applications]).as_deref(),
      Some("de.zeitgleich.teamcraft")
    );
    std::fs::remove_dir_all(dir).unwrap();
  }
}
