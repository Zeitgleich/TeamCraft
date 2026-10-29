//! Entry point: configuration, logging, then the notetaker until a signal.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use teamcraft_notetaker::app::{open_client, run};
use teamcraft_notetaker::config::{CONFIG_PATH_ENV, NotetakerConfig};
use teamcraft_ops::shutdown_signal;
use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
  match serve() {
    Ok(()) => ExitCode::SUCCESS,
    Err(error) => {
      // Logging may not be initialised yet, so report on stderr directly.
      let _ = writeln!(std::io::stderr(), "teamcraft-notetaker: {error:#}");
      ExitCode::FAILURE
    },
  }
}

#[tokio::main]
async fn serve() -> anyhow::Result<()> {
  let config = NotetakerConfig::load(config_path().as_deref())?;
  tracing_subscriber::fmt()
    .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
    .init();
  let client = open_client(&config).await?;
  run(&config, client, shutdown_signal()).await
}

fn config_path() -> Option<PathBuf> {
  let mut args = std::env::args().skip(1);
  while let Some(arg) = args.next() {
    if arg == "--config" {
      return args.next().map(PathBuf::from);
    }
    if let Some(path) = arg.strip_prefix("--config=") {
      return Some(PathBuf::from(path));
    }
  }
  std::env::var_os(CONFIG_PATH_ENV).map(PathBuf::from)
}
