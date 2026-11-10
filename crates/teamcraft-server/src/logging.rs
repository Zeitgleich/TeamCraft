use teamcraft_client_api::crash_reporting::trace;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer as _;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::config::{LogFormat, LoggingConfig};

/// Installs the global `tracing` subscriber. `RUST_LOG` overrides the configured filter.
///
/// The filter is the log's alone: the trace layer (docs/tracing.md) sees the request and SQL spans
/// at every log level, and only while crash reports are on.
///
/// Callers must never log plaintext message content, keys, tokens or passwords (security.md).
pub fn init(config: &LoggingConfig) -> anyhow::Result<()> {
  let filter = EnvFilter::try_from_default_env().or_else(|_| EnvFilter::try_new(&config.filter))?;
  let registry = tracing_subscriber::registry().with(trace::layer());
  match config.format {
    LogFormat::Pretty => registry
      .with(tracing_subscriber::fmt::layer().with_filter(filter))
      .try_init()?,
    LogFormat::Json => registry
      .with(tracing_subscriber::fmt::layer().json().with_filter(filter))
      .try_init()?,
  }
  Ok(())
}
