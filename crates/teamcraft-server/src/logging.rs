use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::config::{LogFormat, LoggingConfig};

/// Installs the global `tracing` subscriber. `RUST_LOG` overrides the configured filter.
///
/// Callers must never log plaintext message content, keys, tokens or passwords (security.md).
pub fn init(config: &LoggingConfig) -> anyhow::Result<()> {
  let filter = EnvFilter::try_from_default_env().or_else(|_| EnvFilter::try_new(&config.filter))?;
  let registry = tracing_subscriber::registry().with(filter);
  match config.format {
    LogFormat::Pretty => registry.with(tracing_subscriber::fmt::layer()).try_init()?,
    LogFormat::Json => registry
      .with(tracing_subscriber::fmt::layer().json())
      .try_init()?,
  }
  Ok(())
}
