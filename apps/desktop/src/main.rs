//! Desktop shell. It only hosts the webview; the UI lives in `teamcraft-ui` and the client logic
//! will move into `teamcraft-core`, which this crate links once the UI talks to a real backend.

// Hides the console window on Windows release builds; harmless on macOS and Linux.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anyhow::Context;
use tracing_subscriber::EnvFilter;

fn main() -> anyhow::Result<()> {
  tracing_subscriber::fmt()
    .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
    .init();

  tauri::Builder::default()
    .run(tauri::generate_context!())
    .context("failed to run the TeamCraft desktop shell")
}
