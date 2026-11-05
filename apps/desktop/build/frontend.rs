//! The web build the app embeds, without what only browsers run (#1295).
//!
//! Tauri embeds `frontendDist` (`crates/teamcraft-ui/dist`, the same build the web release ships)
//! as it is. The app's calls run in the shell (`RemoteCalls`), so the page never loads the
//! browser's call module, its AudioWorklet, the background effects' segmentation worker and its
//! model (the app bundles its own ONNX model as a resource). They were a few MB of the app. This
//! copies the build without them into `OUT_DIR` and points the embedding there: Tauri's
//! `generate_context!` reads the configuration from `tauri.conf.json` merged with the
//! `TAURI_CONFIG` variable (which the Tauri CLI sets for its `--config` files), and the variable
//! this build script sets for the compiler adds `build.frontendDist`. The Tauri CLI, `cargo build`
//! (Flatpak) and the placeholder directory of the CI's checks all work as before: they only need
//! `crates/teamcraft-ui/dist`. Without it nothing is changed (Tauri reports a missing build itself).

use std::path::{Path, PathBuf};

/// `build.frontendDist` of `tauri.conf.json`, relative to this crate.
const WEB_BUILD: &str = "../../crates/teamcraft-ui/dist";
/// Files of the web build's top directory that only browsers load, by the start of their name
/// (the build adds a hash and an extension: `teamcraft-web-calls-<hash>_bg.wasm`).
const BROWSER_ONLY_FILES: &[&str] = &[
  // The call module (`teamcraft-web-calls`) and its AudioWorklet.
  "teamcraft-web-calls",
  "teamcraft-call-worklet",
  // The background effects' segmentation worker: its script and module.
  "teamcraft-segmentation-worker",
  "teamcraft-web-segmentation",
];
/// Directories of the web build's top directory that only browsers load: the segmentation model.
const BROWSER_ONLY_DIRECTORIES: &[&str] = &["models"];

/// Copies the web build without the browser-only files and has the compiler's Tauri configuration
/// embed the copy.
pub fn embed_without_web_only_files() -> Result<(), String> {
  cargo("rerun-if-env-changed=TAURI_CONFIG");
  let manifest_dir = PathBuf::from(variable("CARGO_MANIFEST_DIR")?);
  let web_build = manifest_dir.join(WEB_BUILD);
  if !web_build.is_dir() {
    return Ok(());
  }
  let copy = PathBuf::from(variable("OUT_DIR")?).join("frontend");
  if copy.exists() {
    std::fs::remove_dir_all(&copy).map_err(|error| format!("clearing {}: {error}", copy.display()))?;
  }
  copy_directory(&web_build, &copy, true)?;
  // Absolute, so it doesn't depend on where the target directory is. Canonical: on Windows that
  // is `\\?\C:\...`, which Tauri can't mistake for a URL as it would `C:\...` (scheme `c`).
  let copy = std::fs::canonicalize(&copy).map_err(|error| format!("{}: {error}", copy.display()))?;
  let mut config = match std::env::var("TAURI_CONFIG") {
    Ok(json) => serde_json::from_str(&json).map_err(|error| format!("TAURI_CONFIG: {error}"))?,
    Err(_) => serde_json::Value::Object(serde_json::Map::new()),
  };
  let build = config
    .as_object_mut()
    .ok_or("TAURI_CONFIG is no object")?
    .entry("build")
    .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
    .as_object_mut()
    .ok_or("TAURI_CONFIG's build is no object")?;
  build.insert(
    "frontendDist".to_owned(),
    serde_json::Value::String(copy.display().to_string()),
  );
  cargo(&format!("rustc-env=TAURI_CONFIG={config}"));
  Ok(())
}

/// Copies `from` into `to`; at the top (`is_top`) without the browser-only files.
fn copy_directory(from: &Path, to: &Path, is_top: bool) -> Result<(), String> {
  std::fs::create_dir_all(to).map_err(|error| format!("{}: {error}", to.display()))?;
  let entries = std::fs::read_dir(from).map_err(|error| format!("{}: {error}", from.display()))?;
  for entry in entries {
    let entry = entry.map_err(|error| format!("{}: {error}", from.display()))?;
    let name = entry.file_name();
    let name_text = name.to_string_lossy();
    let path = entry.path();
    let is_directory = path.is_dir();
    if is_top && is_browser_only(&name_text, is_directory) {
      continue;
    }
    let target = to.join(&name);
    if is_directory {
      copy_directory(&path, &target, false)?;
    } else {
      std::fs::copy(&path, &target).map_err(|error| format!("{}: {error}", path.display()))?;
    }
  }
  Ok(())
}

fn is_browser_only(name: &str, is_directory: bool) -> bool {
  if is_directory {
    return BROWSER_ONLY_DIRECTORIES.contains(&name);
  }
  BROWSER_ONLY_FILES.iter().any(|prefix| {
    name
      .strip_prefix(prefix)
      .is_some_and(|rest| rest.starts_with(['-', '_', '.']))
  })
}

fn variable(name: &str) -> Result<String, String> {
  std::env::var(name).map_err(|_| format!("{name} is not set"))
}

/// A build script talks to cargo on its standard output.
#[allow(clippy::print_stdout)]
fn cargo(directive: &str) {
  println!("cargo:{directive}");
}
