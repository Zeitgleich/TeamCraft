//! `teamcraft-push-gateway`: see docs/push-gateway.md.
//!
//! ```text
//! teamcraft-push-gateway [--config <file>] [serve]
//! teamcraft-push-gateway healthcheck
//! teamcraft-push-gateway [--config <file>] servers list
//! teamcraft-push-gateway [--config <file>] servers block <server-id> <reason...>
//! teamcraft-push-gateway [--config <file>] servers unblock <server-id>
//! teamcraft-push-gateway [--config <file>] servers limits <server-id> <per-second|default> <burst|default> <per-day|default>
//! teamcraft-push-gateway [--config <file>] servers allow <public-key-base64url>
//! ```
//!
//! The `servers` commands work on the database file directly, also while the gateway runs; a block
//! takes effect within `database.server_cache_secs`.

use std::io::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use teamcraft_crypto::push_gateway::GatewayPublicKey;
use teamcraft_ops::{DEFAULT_TIMEOUT, bind_dual_stack, probe, probe_target, shutdown_signal};
use teamcraft_push_gateway::app::{AppState, api_router, internal_router, now_millis};
use teamcraft_push_gateway::config::GatewayConfig;
use teamcraft_push_gateway::limits::DAY_MS;
use teamcraft_push_gateway::maintenance;
use teamcraft_push_gateway::providers::Providers;
use teamcraft_push_gateway::store::{BlockState, LimitOverrides, Store};
use tracing::info;
use tracing_subscriber::EnvFilter;

const CONFIG_ENV: &str = "TEAMCRAFT_PUSH_GATEWAY_CONFIG";

fn main() -> ExitCode {
  match run() {
    Ok(()) => ExitCode::SUCCESS,
    Err(error) => {
      // Logging may not be initialised yet, so report on stderr directly.
      let _ = writeln!(std::io::stderr(), "teamcraft-push-gateway: {error:#}");
      ExitCode::FAILURE
    },
  }
}

#[tokio::main]
async fn run() -> anyhow::Result<()> {
  let mut args: Vec<String> = std::env::args().skip(1).collect();
  let maybe_config_path = take_config_flag(&mut args)?;
  let config = GatewayConfig::load(maybe_config_path.as_deref()).context("reading the configuration")?;
  match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
    [] | ["serve"] => serve(config).await,
    ["healthcheck"] => healthcheck(&config),
    ["servers", rest @ ..] => servers(&config, rest).await,
    _ => bail!("unknown command; see the usage in the crate documentation (src/main.rs)"),
  }
}

/// `--config <file>` anywhere in the arguments, or the environment variable.
fn take_config_flag(args: &mut Vec<String>) -> anyhow::Result<Option<PathBuf>> {
  if let Some(position) = args.iter().position(|arg| arg == "--config") {
    anyhow::ensure!(position + 1 < args.len(), "--config needs a file");
    let path = args.remove(position + 1);
    args.remove(position);
    return Ok(Some(path.into()));
  }
  Ok(std::env::var_os(CONFIG_ENV).map(PathBuf::from))
}

async fn serve(config: GatewayConfig) -> anyhow::Result<()> {
  tracing_subscriber::fmt()
    .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
    .init();
  let providers = Providers::from_config(&config)?;
  if providers.configured().is_empty() {
    tracing::warn!("no push service has credentials: every push is refused");
  }
  let store = Store::open(&config.database.path)
    .with_context(|| format!("opening the database {}", config.database.path.display()))?;
  let (bind, health_bind) = (config.server.bind, config.server.health_bind);
  let state = AppState::new(config, store, providers);

  let api = bind_dual_stack(bind).with_context(|| format!("binding {bind}"))?;
  let internal = bind_dual_stack(health_bind).with_context(|| format!("binding {health_bind}"))?;
  info!(%bind, %health_bind, "push gateway listening");

  tokio::spawn(maintenance::run(state.clone()));
  let api_service = api_router(state.clone()).into_make_service_with_connect_info::<SocketAddr>();
  tokio::select! {
    result = axum::serve(api, api_service) => result.context("API listener")?,
    result = axum::serve(internal, internal_router(state.clone())) => result.context("health listener")?,
    () = shutdown_signal() => {},
  }
  maintenance::flush_usage(&state).await;
  Ok(())
}

/// Probes the running gateway's health endpoint (the image has no curl).
fn healthcheck(config: &GatewayConfig) -> anyhow::Result<()> {
  probe(
    probe_target(config.server.health_bind),
    "/healthz",
    DEFAULT_TIMEOUT,
  )
  .context("unhealthy")
}

async fn servers(config: &GatewayConfig, args: &[&str]) -> anyhow::Result<()> {
  let store = Store::open(&config.database.path)
    .with_context(|| format!("opening the database {}", config.database.path.display()))?;
  let now_secs = i64::try_from(now_millis() / 1000).unwrap_or(i64::MAX);
  let mut out = std::io::stdout().lock();
  match args {
    ["list"] => {
      let day = i64::try_from(now_millis() / DAY_MS).unwrap_or(i64::MAX);
      writeln!(
        out,
        "server_id                         state    today(ok/gone/failed)  last_seen_unix"
      )?;
      for summary in store.list_servers(day).await? {
        let (record, today) = (summary.record, summary.today);
        let state = match &record.block {
          BlockState::Active => "active".to_owned(),
          BlockState::Blocked { reason } => format!("blocked ({reason})"),
        };
        writeln!(
          out,
          "{}  {state:<8} {}/{}/{}  {}",
          record.server_id, today.accepted, today.gone, today.failed, record.last_seen_at
        )?;
      }
    },
    ["block", server_id, reason @ ..] if !reason.is_empty() => {
      let found = store
        .set_blocked(server_id, Some(&reason.join(" ")), now_secs)
        .await?;
      anyhow::ensure!(found, "unknown server {server_id}");
      writeln!(out, "blocked {server_id}")?;
    },
    ["unblock", server_id] => {
      anyhow::ensure!(
        store.set_blocked(server_id, None, now_secs).await?,
        "unknown server {server_id}"
      );
      writeln!(out, "unblocked {server_id}")?;
    },
    ["limits", server_id, per_second, burst, per_day] => {
      let limits = LimitOverrides {
        maybe_pushes_per_second: parse_override(per_second)?,
        maybe_burst: parse_override(burst)?,
        maybe_pushes_per_day: parse_override(per_day)?,
      };
      anyhow::ensure!(
        store.set_limits(server_id, limits).await?,
        "unknown server {server_id}"
      );
      writeln!(out, "limits of {server_id} updated")?;
    },
    ["allow", public_key] => {
      let bytes = URL_SAFE_NO_PAD
        .decode(public_key)
        .context("the key is not base64url")?;
      let bytes = <[u8; 32]>::try_from(bytes.as_slice()).context("the key must be 32 bytes")?;
      let server_id = GatewayPublicKey::from_bytes(bytes).server_id();
      store.insert_server(&server_id, bytes, now_secs).await?;
      writeln!(out, "allowed {server_id}")?;
    },
    _ => bail!(
      "usage: servers list | block <id> <reason> | unblock <id> | limits <id> <n|default> <n|default> <n|default> | allow <key>"
    ),
  }
  Ok(())
}

fn parse_override<T: std::str::FromStr>(value: &str) -> anyhow::Result<Option<T>> {
  if value == "default" {
    return Ok(None);
  }
  value
    .parse()
    .map(Some)
    .map_err(|_| anyhow::anyhow!("{value} is not a number or \"default\""))
}
