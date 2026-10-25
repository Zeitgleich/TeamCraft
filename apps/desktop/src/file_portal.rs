//! Linux: file pickers through xdg-desktop-portal, so Plasma shows its own dialog and GNOME its own
//! (#509). WebKitGTK's `<input type=file>` opens a `GtkFileChooserNative`, which uses the portal
//! when `GTK_USE_PORTAL=1`. The variable has to be set before GTK starts, and `set_var` is unsafe
//! (forbidden here), so a start without it re-executes the binary once with it set, however the app
//! was launched (deb, rpm, AppImage, a terminal).

use std::ffi::OsStr;
use std::os::unix::process::CommandExt;
use std::process::Command;

const PORTAL_VARIABLE: &str = "GTK_USE_PORTAL";

/// Replaces this process with itself and `GTK_USE_PORTAL=1` unless the portal is already wanted, the
/// user chose otherwise, or the sandbox (Flatpak) uses the portal anyway. Returns when nothing needs
/// to be done or the re-execution failed: the GTK dialog still works.
pub fn ensure_portal_choosers() {
  let maybe_value = std::env::var_os(PORTAL_VARIABLE);
  if !needs_portal(maybe_value.as_deref(), std::env::var_os("FLATPAK_ID").is_some()) {
    return;
  }
  let Ok(exe) = std::env::current_exe() else {
    return;
  };
  // `exec` only returns on failure; logging isn't set up yet, and the fallback is harmless.
  let _error = Command::new(exe)
    .args(std::env::args_os().skip(1))
    .env(PORTAL_VARIABLE, "1")
    .exec();
}

/// Whether to restart with the portal: the variable is unset (an explicit `0` is the user's
/// choice) and the app is not in Flatpak, whose GTK always uses the portal.
fn needs_portal(maybe_value: Option<&OsStr>, is_flatpak: bool) -> bool {
  maybe_value.is_none() && !is_flatpak
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn restarts_only_when_the_variable_is_unset_outside_flatpak() {
    assert!(needs_portal(None, false));
    assert!(!needs_portal(Some(OsStr::new("1")), false));
    assert!(!needs_portal(Some(OsStr::new("0")), false));
    assert!(!needs_portal(None, true));
  }
}
