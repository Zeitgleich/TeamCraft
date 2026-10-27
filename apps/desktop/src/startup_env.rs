//! Linux: environment variables GTK and WebKitGTK read once, when they start. `set_var` is unsafe
//! (forbidden here), so a start that lacks one of them re-executes the binary once with them set,
//! however the app was launched (deb, rpm, AppImage, Flatpak, a terminal). A value the user set
//! is always kept.
//!
//! - `GTK_USE_PORTAL=1`: file pickers through xdg-desktop-portal, so Plasma shows its own dialog
//!   and GNOME its own (#509). WebKitGTK's `<input type=file>` opens a `GtkFileChooserNative`,
//!   which uses the portal when the variable is set. Flatpak's GTK uses the portal anyway.
//! - `WEBKIT_DISABLE_DMABUF_RENDERER=1`, only when the user asks for it with
//!   `TEAMCRAFT_WEBKIT_DMABUF=0`: WebKitGTK's DMABUF renderer is the default again (#805). Without
//!   it WebKitGTK paints into shared memory and the CPU copies every frame, which made animations
//!   and the lightbox stutter on a large window. The flicker that led to turning it off for
//!   everyone (AMD on KDE Plasma, #687, #708) did not show on current Mesa, WebKitGTK and KWin; the
//!   switch stays for a driver or compositor where it does
//!   (apps/desktop/packaging/linux/README.md, "Rendering").

use std::ffi::OsStr;
use std::os::unix::process::CommandExt;
use std::process::Command;

const PORTAL_VARIABLE: &str = "GTK_USE_PORTAL";
const DMABUF_VARIABLE: &str = "WEBKIT_DISABLE_DMABUF_RENDERER";
/// The user's way to switch WebKitGTK's DMABUF renderer off: `0` does, anything else leaves it on.
const DMABUF_CHOICE_VARIABLE: &str = "TEAMCRAFT_WEBKIT_DMABUF";

/// What the start found in its environment.
struct StartEnvironment<'a> {
  maybe_portal: Option<&'a OsStr>,
  maybe_dmabuf: Option<&'a OsStr>,
  maybe_dmabuf_choice: Option<&'a OsStr>,
  is_flatpak: bool,
}

/// Replaces this process with itself and the variables it lacks. Returns when nothing needs to be
/// done or the re-execution failed: the app still works, with the GTK file dialog and WebKit's
/// default renderer.
pub fn ensure_startup_environment() {
  let maybe_portal = std::env::var_os(PORTAL_VARIABLE);
  let maybe_dmabuf = std::env::var_os(DMABUF_VARIABLE);
  let maybe_dmabuf_choice = std::env::var_os(DMABUF_CHOICE_VARIABLE);
  let missing = missing_variables(&StartEnvironment {
    maybe_portal: maybe_portal.as_deref(),
    maybe_dmabuf: maybe_dmabuf.as_deref(),
    maybe_dmabuf_choice: maybe_dmabuf_choice.as_deref(),
    is_flatpak: std::env::var_os("FLATPAK_ID").is_some(),
  });
  if missing.is_empty() {
    return;
  }
  let Ok(exe) = std::env::current_exe() else {
    return;
  };
  // `exec` only returns on failure; logging isn't set up yet, and the fallback is harmless. The
  // new process has every variable set, so it doesn't execute again.
  let _error = Command::new(exe)
    .args(std::env::args_os().skip(1))
    .envs(missing)
    .exec();
}

/// The variables to set before GTK starts: none the user set themselves, the portal outside
/// Flatpak, and the DMABUF switch only when the user chose the compatible renderer.
fn missing_variables(start: &StartEnvironment<'_>) -> Vec<(&'static str, &'static str)> {
  let mut missing = Vec::new();
  if start.maybe_portal.is_none() && !start.is_flatpak {
    missing.push((PORTAL_VARIABLE, "1"));
  }
  let disables_dmabuf = start.maybe_dmabuf_choice.is_some_and(|value| value == "0");
  if start.maybe_dmabuf.is_none() && disables_dmabuf {
    missing.push((DMABUF_VARIABLE, "1"));
  }
  missing
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_plain_start_sets_only_the_portal() {
    assert_eq!(
      missing_variables(&start(None, None, None, false)),
      vec![(PORTAL_VARIABLE, "1")]
    );
  }

  #[test]
  fn the_users_values_are_kept() {
    let zero = Some(OsStr::new("0"));
    assert!(missing_variables(&start(zero, zero, zero, false)).is_empty());
    let one = Some(OsStr::new("1"));
    assert!(missing_variables(&start(one, one, zero, false)).is_empty());
  }

  #[test]
  fn flatpak_needs_no_variable() {
    assert!(missing_variables(&start(None, None, None, true)).is_empty());
  }

  #[test]
  fn the_compatible_renderer_can_be_chosen() {
    assert_eq!(
      missing_variables(&start(None, None, Some(OsStr::new("0")), true)),
      vec![(DMABUF_VARIABLE, "1")]
    );
    // Any other value is not a choice to switch it off.
    assert!(missing_variables(&start(None, None, Some(OsStr::new("1")), true)).is_empty());
  }

  fn start<'a>(
    maybe_portal: Option<&'a OsStr>,
    maybe_dmabuf: Option<&'a OsStr>,
    maybe_dmabuf_choice: Option<&'a OsStr>,
    is_flatpak: bool,
  ) -> StartEnvironment<'a> {
    StartEnvironment {
      maybe_portal,
      maybe_dmabuf,
      maybe_dmabuf_choice,
      is_flatpak,
    }
  }
}
