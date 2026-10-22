//! Entry point: configuration, logging, then the bridge. `teamcraft-bridge healthcheck` probes a
//! running bridge for the container `HEALTHCHECK`.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use teamcraft_bridge::app::{open_client, run};
use teamcraft_bridge::config::{BridgeConfig, CONFIG_PATH_ENV};
use teamcraft_ops::{DEFAULT_TIMEOUT, probe, probe_target, shutdown_signal};
use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
  let is_healthcheck = std::env::args().nth(1).as_deref() == Some("healthcheck");
  let result = if is_healthcheck { healthcheck() } else { serve() };
  match result {
    Ok(()) => ExitCode::SUCCESS,
    Err(error) => {
      // Logging may not be initialised yet, so report on stderr directly.
      let _ = writeln!(std::io::stderr(), "teamcraft-bridge: {error:#}");
      ExitCode::FAILURE
    },
  }
}

/// Probes the first configured listen address (loopback of the same family if it is unspecified).
fn healthcheck() -> anyhow::Result<()> {
  let config = BridgeConfig::load(config_path().as_deref())?;
  let addr = config.listen.addrs.first().context("listen.addrs is empty")?;
  probe(probe_target(*addr), "/healthz", DEFAULT_TIMEOUT)?;
  Ok(())
}

#[tokio::main]
async fn serve() -> anyhow::Result<()> {
  let config = BridgeConfig::load(config_path().as_deref())?;
  tracing_subscriber::fmt()
    .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
    .init();
  let client = open_client(&config).await?;
  run(&config, client, shutdown_signal()).await?;
  Ok(())
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
