//! Linux: environment variables GTK and WebKitGTK read once, when they start. `set_var` is unsafe
//! (forbidden here), so a start that lacks one of them re-executes the binary once with them set,
//! however the app was launched (deb, rpm, AppImage, Flatpak, a terminal). A value the user set
//! is always kept.
//!
//! - `GTK_USE_PORTAL=1`: file pickers through xdg-desktop-portal, so Plasma shows its own dialog
//!   and GNOME its own (#509). WebKitGTK's `<input type=file>` opens a `GtkFileChooserNative`,
//!   which uses the portal when the variable is set. Flatpak's GTK uses the portal anyway.
//! - WebKitGTK's renderer (`webview_renderer.rs`): GPU buffers by default (#805), shared memory
//!   for an Intel GPU on Wayland (`WEBKIT_DMABUF_RENDERER_FORCE_SHM=1`, #2416), the
//!   compatible renderer when the user asks for it (`WEBKIT_DISABLE_DMABUF_RENDERER=1`).
//! - `MALLOC_ARENA_MAX=2` and `MALLOC_TRIM_THRESHOLD_=131072`: glibc's allocator keeps what was
//!   freed in up to eight arenas per core and raises its trim threshold as it goes, so the shell
//!   held several times the memory it used: 314 MiB in arenas after four hours of 1.0.30, where a
//!   heap profile finds 35 MiB in use (docs/performance.md, "Desktop on Linux: memory pressure and
//!   the allocator"; #1708).
//!   Two arenas and a fixed threshold give freed memory back. Not set when the user set them or
//!   `GLIBC_TUNABLES`.
//! - `JSC_numberOfWasmCompilerThreads=4`, on machines with more than five hardware threads:
//!   JavaScriptCore compiles WebAssembly on one thread per hardware thread but one, and each keeps
//!   some of what it compiled with: on 24 threads 12 to 16 MiB more for the app's modules in
//!   `jsc`, 6 MiB of the web process in `desktop_perf.rs`. Four compile the app's main module in
//!   40 instead of 23 ms (docs/performance.md, "Desktop on Linux: where the memory is"; #1708). Not
//!   set when the user set it.

use std::ffi::OsStr;
use std::os::unix::process::CommandExt;
use std::process::Command;

use crate::webview_renderer::{self, RendererInputs};

const PORTAL_VARIABLE: &str = "GTK_USE_PORTAL";
/// glibc's own switches for its allocator; any of them set means the user tunes it.
const TUNABLES_VARIABLE: &str = "GLIBC_TUNABLES";
const ARENA_MAX_VARIABLE: &str = "MALLOC_ARENA_MAX";
const TRIM_THRESHOLD_VARIABLE: &str = "MALLOC_TRIM_THRESHOLD_";
/// Two arenas: the core's threads and GTK's main thread share them instead of keeping one each.
const ARENA_MAX: &str = "2";
/// glibc's default of 128 KiB, but fixed: set, glibc no longer raises it (up to 64 MiB) after a
/// large block is freed, and allocations from 128 KiB up keep their own mapping, returned on free.
const TRIM_THRESHOLD_BYTES: &str = "131072";
/// JavaScriptCore's option for its WebAssembly compiler threads (JSC reads `JSC_<option>`).
const WASM_COMPILER_THREADS_VARIABLE: &str = "JSC_numberOfWasmCompilerThreads";
/// Compiler threads the app's WebAssembly gets: JavaScriptCore's default is one per hardware
/// thread but one, so the variable is set only where that is more.
const WASM_COMPILER_THREADS: usize = 4;
const WASM_COMPILER_THREADS_VALUE: &str = "4";

/// What the start found in its environment.
struct StartEnvironment<'a> {
  maybe_portal: Option<&'a OsStr>,
  renderer: RendererInputs<'a>,
  /// The user tunes glibc's allocator: `GLIBC_TUNABLES` or one of our two variables is set.
  is_allocator_tuned: bool,
  /// The user chose JavaScriptCore's WebAssembly compiler threads.
  is_wasm_compiler_tuned: bool,
  /// Hardware threads the process may run on.
  available_threads: usize,
  is_flatpak: bool,
}

/// Replaces this process with itself and the variables it lacks. Returns when nothing needs to be
/// done or the re-execution failed: the app still works, with the GTK file dialog and WebKit's
/// default renderer.
pub fn ensure_startup_environment() {
  let maybe_portal = std::env::var_os(PORTAL_VARIABLE);
  let renderer_choice = std::env::var_os(webview_renderer::RENDERER_CHOICE_VARIABLE);
  let dmabuf_choice = std::env::var_os(webview_renderer::DMABUF_CHOICE_VARIABLE);
  let dmabuf = std::env::var_os(webview_renderer::DMABUF_VARIABLE);
  let shared_memory = std::env::var_os(webview_renderer::SHARED_MEMORY_VARIABLE);
  let missing = missing_variables(&StartEnvironment {
    maybe_portal: maybe_portal.as_deref(),
    renderer: webview_renderer::system_inputs(
      renderer_choice.as_deref(),
      dmabuf_choice.as_deref(),
      dmabuf.as_deref(),
      shared_memory.as_deref(),
    ),
    is_allocator_tuned: [TUNABLES_VARIABLE, ARENA_MAX_VARIABLE, TRIM_THRESHOLD_VARIABLE]
      .iter()
      .any(|name| std::env::var_os(name).is_some()),
    is_wasm_compiler_tuned: std::env::var_os(WASM_COMPILER_THREADS_VARIABLE).is_some(),
    available_threads: std::thread::available_parallelism().map_or(1, usize::from),
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
/// Flatpak, the renderer's switch where it isn't WebKit's default, and the
/// allocator's unless the user tunes it, and fewer WebAssembly compiler threads on a machine where
/// JavaScriptCore would start more.
fn missing_variables(start: &StartEnvironment<'_>) -> Vec<(&'static str, &'static str)> {
  let mut missing = Vec::new();
  if start.maybe_portal.is_none() && !start.is_flatpak {
    missing.push((PORTAL_VARIABLE, "1"));
  }
  missing.extend(webview_renderer::missing_variables(&start.renderer));
  if !start.is_allocator_tuned {
    missing.push((ARENA_MAX_VARIABLE, ARENA_MAX));
    missing.push((TRIM_THRESHOLD_VARIABLE, TRIM_THRESHOLD_BYTES));
  }
  let default_wasm_compiler_threads = start.available_threads.saturating_sub(1);
  if !start.is_wasm_compiler_tuned && default_wasm_compiler_threads > WASM_COMPILER_THREADS {
    missing.push((WASM_COMPILER_THREADS_VARIABLE, WASM_COMPILER_THREADS_VALUE));
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
    assert_eq!(missing_variables(&start(None, false, false)), expected);
  }

  #[test]
  fn the_users_values_are_kept() {
    let zero = Some(OsStr::new("0"));
    let kept = StartEnvironment {
      renderer: RendererInputs {
        maybe_dmabuf_choice: zero,
        maybe_dmabuf: zero,
        ..RendererInputs::default()
      },
      ..start(zero, true, false)
    };
    assert!(missing_variables(&kept).is_empty());
  }

  #[test]
  fn flatpak_needs_no_portal() {
    assert_eq!(missing_variables(&start(None, false, true)), ALLOCATOR.to_vec());
    assert!(missing_variables(&start(None, true, true)).is_empty());
  }

  #[test]
  fn the_renderers_variables_are_set() {
    let compatible = StartEnvironment {
      renderer: RendererInputs {
        maybe_dmabuf_choice: Some(OsStr::new("0")),
        ..RendererInputs::default()
      },
      ..start(None, true, true)
    };
    assert_eq!(
      missing_variables(&compatible),
      vec![(webview_renderer::DMABUF_VARIABLE, "1")]
    );
  }

  #[test]
  fn many_hardware_threads_get_fewer_wasm_compiler_threads() {
    let wasm = (WASM_COMPILER_THREADS_VARIABLE, WASM_COMPILER_THREADS_VALUE);
    let on = |available_threads| StartEnvironment {
      available_threads,
      ..start(None, true, true)
    };
    assert_eq!(missing_variables(&on(24)), vec![wasm]);
    assert_eq!(missing_variables(&on(6)), vec![wasm]);
    // JavaScriptCore's own default is four threads or fewer here.
    assert!(missing_variables(&on(5)).is_empty());
    assert!(missing_variables(&on(1)).is_empty());
    let tuned = StartEnvironment {
      is_wasm_compiler_tuned: true,
      ..on(24)
    };
    assert!(missing_variables(&tuned).is_empty());
  }

  fn start(maybe_portal: Option<&OsStr>, is_allocator_tuned: bool, is_flatpak: bool) -> StartEnvironment<'_> {
    StartEnvironment {
      maybe_portal,
      renderer: RendererInputs::default(),
      is_allocator_tuned,
      is_wasm_compiler_tuned: false,
      available_threads: 4,
      is_flatpak,
    }
  }
}
