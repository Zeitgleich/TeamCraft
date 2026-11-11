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
//! - `MALLOC_ARENA_MAX=2` and `MALLOC_TRIM_THRESHOLD_=131072`: glibc's allocator keeps what was
//!   freed in up to eight arenas per core and raises its trim threshold as it goes, so the shell
//!   held several times the memory it used: 314 MiB in arenas after four hours of 1.0.30, where a
//!   heap profile finds 35 MiB in use (docs/performance.md, "Desktop on Linux: memory pressure and
//!   the allocator"; #1708).
//!   Two arenas and a fixed threshold give freed memory back. Not set when the user set them or
//!   `GLIBC_TUNABLES`.

use std::ffi::OsStr;
use std::os::unix::process::CommandExt;
use std::process::Command;

const PORTAL_VARIABLE: &str = "GTK_USE_PORTAL";
const DMABUF_VARIABLE: &str = "WEBKIT_DISABLE_DMABUF_RENDERER";
/// The user's way to switch WebKitGTK's DMABUF renderer off: `0` does, anything else leaves it on.
const DMABUF_CHOICE_VARIABLE: &str = "TEAMCRAFT_WEBKIT_DMABUF";
/// glibc's own switches for its allocator; any of them set means the user tunes it.
const TUNABLES_VARIABLE: &str = "GLIBC_TUNABLES";
const ARENA_MAX_VARIABLE: &str = "MALLOC_ARENA_MAX";
const TRIM_THRESHOLD_VARIABLE: &str = "MALLOC_TRIM_THRESHOLD_";
/// Two arenas: the core's threads and GTK's main thread share them instead of keeping one each.
const ARENA_MAX: &str = "2";
/// glibc's default of 128 KiB, but fixed: set, glibc no longer raises it (up to 64 MiB) after a
/// large block is freed, and allocations from 128 KiB up keep their own mapping, returned on free.
const TRIM_THRESHOLD_BYTES: &str = "131072";

/// What the start found in its environment.
struct StartEnvironment<'a> {
  maybe_portal: Option<&'a OsStr>,
  maybe_dmabuf: Option<&'a OsStr>,
  maybe_dmabuf_choice: Option<&'a OsStr>,
  /// The user tunes glibc's allocator: `GLIBC_TUNABLES` or one of our two variables is set.
  is_allocator_tuned: bool,
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
    is_allocator_tuned: [TUNABLES_VARIABLE, ARENA_MAX_VARIABLE, TRIM_THRESHOLD_VARIABLE]
      .iter()
      .any(|name| std::env::var_os(name).is_some()),
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
/// Flatpak, the DMABUF switch only when the user chose the compatible renderer, and the
/// allocator's unless the user tunes it.
fn missing_variables(start: &StartEnvironment<'_>) -> Vec<(&'static str, &'static str)> {
  let mut missing = Vec::new();
  if start.maybe_portal.is_none() && !start.is_flatpak {
    missing.push((PORTAL_VARIABLE, "1"));
  }
  let disables_dmabuf = start.maybe_dmabuf_choice.is_some_and(|value| value == "0");
  if start.maybe_dmabuf.is_none() && disables_dmabuf {
    missing.push((DMABUF_VARIABLE, "1"));
  }
  if !start.is_allocator_tuned {
    missing.push((ARENA_MAX_VARIABLE, ARENA_MAX));
    missing.push((TRIM_THRESHOLD_VARIABLE, TRIM_THRESHOLD_BYTES));
  }
  missing
}

#[cfg(test)]
mod tests {
  use super::*;

  const ALLOCATOR: [(&str, &str); 2] = [
    (ARENA_MAX_VARIABLE, ARENA_MAX),
    (TRIM_THRESHOLD_VARIABLE, TRIM_THRESHOLD_BYTES),
  ];

  #[test]
  fn a_plain_start_sets_the_portal_and_the_allocator() {
    let mut expected = vec![(PORTAL_VARIABLE, "1")];
    expected.extend(ALLOCATOR);
    assert_eq!(
      missing_variables(&start(None, None, None, false, false)),
      expected
    );
  }

  #[test]
  fn the_users_values_are_kept() {
    let zero = Some(OsStr::new("0"));
    assert!(missing_variables(&start(zero, zero, zero, true, false)).is_empty());
    let one = Some(OsStr::new("1"));
    assert!(missing_variables(&start(one, one, zero, true, false)).is_empty());
  }

  #[test]
  fn flatpak_needs_no_portal() {
    assert_eq!(
      missing_variables(&start(None, None, None, false, true)),
      ALLOCATOR.to_vec()
    );
    assert!(missing_variables(&start(None, None, None, true, true)).is_empty());
  }

  #[test]
  fn the_compatible_renderer_can_be_chosen() {
    assert_eq!(
      missing_variables(&start(None, None, Some(OsStr::new("0")), true, true)),
      vec![(DMABUF_VARIABLE, "1")]
    );
    // Any other value is not a choice to switch it off.
    assert!(missing_variables(&start(None, None, Some(OsStr::new("1")), true, true)).is_empty());
  }

  fn start<'a>(
    maybe_portal: Option<&'a OsStr>,
    maybe_dmabuf: Option<&'a OsStr>,
    maybe_dmabuf_choice: Option<&'a OsStr>,
    is_allocator_tuned: bool,
    is_flatpak: bool,
  ) -> StartEnvironment<'a> {
    StartEnvironment {
      maybe_portal,
      maybe_dmabuf,
      maybe_dmabuf_choice,
      is_allocator_tuned,
      is_flatpak,
    }
  }
}
