//! WebTransport SFU that forwards DAVE-encrypted RTP over QUIC datagrams.
//!
//! Configuration via environment (spike; moves to the TOML config later):
//!
//! - `TEAMCRAFT_SFU_BIND`: UDP address, default `[::]:3478` (dual-stack)
//! - `TEAMCRAFT_SFU_JOIN_TOKEN_PUBLIC_KEY`: PEM file with the server's Ed25519 public key for join
//!   tokens (`[calls] signing_key_path` on the server), and `TEAMCRAFT_SFU_JOIN_TOKEN_ISSUER`: the
//!   server's `[calls] issuer`
//! - `TEAMCRAFT_SFU_HEALTH_BIND`: TCP address of the `/healthz` endpoint, default `[::]:8081`;
//!   `teamcraft-sfu healthcheck` probes it (for the container `HEALTHCHECK`)
//! - `TEAMCRAFT_SFU_SERVER_URL` and `TEAMCRAFT_SFU_REGISTRATION_TOKEN`: report joins and leaves to
//!   `teamcraft-server` (its `[calls] sfu_registration_token`), which starts calls and rings DMs;
//!   the token also authenticates the server's meeting host controls
//!   (`POST /internal/v1/calls/control` on the health listener)
//! - `TEAMCRAFT_SFU_DEV_HTTP`: e.g. `[::1]:8080` (dual-stack); enables dev mode (test pages)
//! - `TEAMCRAFT_SFU_INSECURE_DEV_TOKENS=1`: accept unsigned dev tokens when no public key is
//!   configured; only honoured if `TEAMCRAFT_SFU_BIND` and the dev HTTP address are loopback
//! - `TEAMCRAFT_SFU_DEV_ENGINE_DIR`: in dev mode, the `wasm-bindgen` output of `teamcraft-media`'s
//!   `web_call` example, served for the `/engine` test page
//! - `TEAMCRAFT_SFU_CERT` / `TEAMCRAFT_SFU_KEY`: PEM files; without them a self-signed
//!   certificate valid for 14 days is generated (browsers accept it via certificate hash)
//! - `TEAMCRAFT_SFU_RECONNECT_GRACE_SECS`: how long a participant whose connection was lost keeps
//!   its slot to rejoin, default 30
//! - `TEAMCRAFT_SFU_IDLE_TIMEOUT_SECS`: a connection silent this long is lost, default 10
//! - `TEAMCRAFT_SFU_SHUTDOWN_DRAIN_SECS`: on SIGTERM every call connection is closed as "going
//!   away" (clients rejoin, ARCHITECTURE.md §6.5); how long those closes get to reach the clients
//!   before the process exits, default 2
//! - `TEAMCRAFT_SFU_UDP_RECEIVE_BUFFER_BYTES`: the UDP socket's receive buffer, default 4 MiB (the
//!   kernel caps it at `net.core.rmem_max`)
//! - `TEAMCRAFT_SFU_MAX_DEVICES_PER_USER_PER_CALL`: devices of one account in the same call at once,
//!   default 3 (keep it equal to `calls.max_devices_per_user_per_call` of the server)
//! - `TEAMCRAFT_SFU_ROSTER_INTERVAL_SECS`: how often the whole call roster is reported to the
//!   server (also at start), which drops participants the SFU no longer has, default 30
//! - `TEAMCRAFT_SFU_FALLBACK_ENABLED`: `0` turns off the WebSocket fallback for networks that block
//!   UDP, default `1` (ARCHITECTURE.md §6.6); `TEAMCRAFT_SFU_FALLBACK_BIND`: its TCP address, plain
//!   HTTP behind the reverse proxy, default `127.0.0.1:8082` (a proxy on this machine; the container
//!   image sets `[::]:8082`)
//! - `TEAMCRAFT_SFU_FALLBACK_TRUSTED_PROXIES`: addresses or networks, comma-separated, whose
//!   `X-Forwarded-For` counts for the per-address limits; default the machine itself
//! - `TEAMCRAFT_SFU_FALLBACK_MAX_CONNECTIONS` (joined, default 1024),
//!   `TEAMCRAFT_SFU_FALLBACK_MAX_UNJOINED_CONNECTIONS` (default 128),
//!   `TEAMCRAFT_SFU_FALLBACK_HANDSHAKE_TIMEOUT_MILLIS` (default 3000),
//!   `TEAMCRAFT_SFU_FALLBACK_FIRST_FRAME_TIMEOUT_MILLIS` (the `join` of a connection that showed no
//!   token in the upgrade, default 3000), `TEAMCRAFT_SFU_FALLBACK_REQUIRE_UPGRADE_TOKEN=1` (refuse
//!   such connections at the upgrade, once every client sends one, which those of before #1297 do not; default `0`),
//!   `TEAMCRAFT_SFU_FALLBACK_QUEUE_BYTES`
//!   (media waiting for one connection, default 1 MiB), `TEAMCRAFT_SFU_FALLBACK_QUEUE_MAX_AGE_MILLIS`
//!   (older media is dropped, default 300) and `TEAMCRAFT_SFU_FALLBACK_SEND_BUFFER_BYTES` (a
//!   connection's socket buffer, default 128 KiB)
//! - `TEAMCRAFT_SFU_METRICS_ENABLED=1`: serve `GET /metrics` on a listener of its own (off by
//!   default, docs/operations.md, "Metrics"), and `TEAMCRAFT_SFU_METRICS_ADDRS`: its addresses,
//!   comma-separated, default `127.0.0.1:9465,[::1]:9465` (`[::]:9465` is dual stack)

use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use teamcraft_ops::metrics::{bind_metrics, spawn_metrics_endpoint};
use teamcraft_ops::{DEFAULT_TIMEOUT, probe, probe_target, shutdown_signal};
use teamcraft_sfu::{
  auth::{DevTokenVerifier, JoinTokenVerifier, JwtJoinTokenConfig, JwtJoinTokenVerifier, RejectAllVerifier},
  config::{FallbackConfig, SfuConfig, parse_trusted_proxies},
  control::control_router,
  dev_page::serve_dev_page,
  health::{DEFAULT_HEALTH_ADDR, serve_health},
  metrics::{SfuMetrics, metrics_addrs},
  reporting::HttpReporter,
  server::Sfu,
};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use wtransport::Identity;

/// Where the fallback listens unless `TEAMCRAFT_SFU_FALLBACK_BIND` says otherwise.
const DEFAULT_FALLBACK_ADDR: &str = "127.0.0.1:8082";

fn main() -> ExitCode {
  if std::env::args().nth(1).as_deref() == Some("healthcheck") {
    return healthcheck();
  }
  match run() {
    Ok(()) => ExitCode::SUCCESS,
    Err(error) => {
      // Logging may not be initialised yet, so report on stderr directly.
      let _ = writeln!(std::io::stderr(), "teamcraft-sfu failed: {error:#}");
      ExitCode::FAILURE
    },
  }
}

/// `teamcraft-sfu healthcheck`: probes the running SFU's health endpoint. The image has no curl.
fn healthcheck() -> ExitCode {
  let result = health_addr()
    .and_then(|addr| probe(probe_target(addr), "/healthz", DEFAULT_TIMEOUT).map_err(anyhow::Error::from));
  match result {
    Ok(()) => ExitCode::SUCCESS,
    Err(error) => {
      let _ = writeln!(std::io::stderr(), "unhealthy: {error:#}");
      ExitCode::FAILURE
    },
  }
}

#[tokio::main]
async fn run() -> anyhow::Result<()> {
  tracing_subscriber::fmt()
    .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
    .init();

  let mut config = SfuConfig::default();
  if let Some(bind) = env_addr("TEAMCRAFT_SFU_BIND")? {
    config.bind_addr = bind;
  }
  config.maybe_dev_http_addr = env_addr("TEAMCRAFT_SFU_DEV_HTTP")?;
  if let Some(grace) = env_duration_secs("TEAMCRAFT_SFU_RECONNECT_GRACE_SECS")? {
    config.reconnect_grace = grace;
  }
  if let Some(timeout) = env_duration_secs("TEAMCRAFT_SFU_IDLE_TIMEOUT_SECS")? {
    config.idle_timeout = timeout;
  }
  if let Some(drain) = env_duration_secs("TEAMCRAFT_SFU_SHUTDOWN_DRAIN_SECS")? {
    config.shutdown_drain = drain;
  }
  if let Some(interval) = env_duration_secs("TEAMCRAFT_SFU_ROSTER_INTERVAL_SECS")? {
    config.roster_interval = interval;
  }
  if let Some(bytes) = env_count("TEAMCRAFT_SFU_UDP_RECEIVE_BUFFER_BYTES")? {
    config.udp_receive_buffer_bytes = bytes;
  }
  if let Some(devices) = env_count("TEAMCRAFT_SFU_MAX_DEVICES_PER_USER_PER_CALL")? {
    config.max_devices_per_user_per_call = devices;
  }

  apply_fallback_env(&mut config.fallback)?;

  let identity = load_identity().await?;
  let verifier = load_verifier(&config)?;

  let maybe_dev_http_addr = config.maybe_dev_http_addr;
  let mut sfu = Sfu::bind(config, identity, verifier).context("binding the SFU")?;
  match (
    std::env::var("TEAMCRAFT_SFU_SERVER_URL"),
    std::env::var("TEAMCRAFT_SFU_REGISTRATION_TOKEN"),
  ) {
    (Ok(server_url), Ok(token)) => {
      info!(%server_url, "reporting call participants to the server");
      sfu = sfu.with_reporter(Arc::new(HttpReporter::new(server_url, token)));
    },
    _ => warn!("no server URL and registration token: calls are not reported to the server"),
  }
  // Host controls come from the server with the same shared token.
  let internal_routes = match std::env::var("TEAMCRAFT_SFU_REGISTRATION_TOKEN") {
    Ok(token) => control_router(sfu.control(), token),
    Err(_) => {
      warn!("no registration token: meeting host controls are disabled");
      axum::Router::new()
    },
  };
  serve_metrics(&sfu).context("metrics endpoint")?;
  let local_addr = sfu.local_addr()?;
  let certificate_hash = sfu.certificate_hash();
  info!(
    %local_addr,
    maybe_fallback_addr = ?sfu.fallback_addr()?,
    certificate_hash = %certificate_hash.fmt(wtransport::tls::Sha256DigestFmt::DottedHex),
    "SFU listening"
  );

  let dev_page = async {
    match maybe_dev_http_addr {
      Some(http_addr) => {
        let maybe_engine_dir = std::env::var_os("TEAMCRAFT_SFU_DEV_ENGINE_DIR").map(PathBuf::from);
        serve_dev_page(http_addr, local_addr.port(), &certificate_hash, maybe_engine_dir).await
      },
      None => std::future::pending().await,
    }
  };
  // SIGTERM (a restart, a rolling deploy): calls are told the SFU is going away and rejoin.
  tokio::select! {
    () = sfu.run_until(shutdown_signal()) => {},
    result = dev_page => result.context("dev page")?,
    result = serve_health(health_addr()?, internal_routes) => result.context("health endpoint")?,
  }
  Ok(())
}

/// The fallback is on unless turned off here: an instance that offers no URL for it never sends a
/// client to it, and an idle listener costs nothing.
fn apply_fallback_env(fallback: &mut FallbackConfig) -> anyhow::Result<()> {
  fallback.maybe_bind_addr = match std::env::var("TEAMCRAFT_SFU_FALLBACK_ENABLED").as_deref() {
    Err(_) | Ok("1") => {
      Some(env_addr("TEAMCRAFT_SFU_FALLBACK_BIND")?.unwrap_or(DEFAULT_FALLBACK_ADDR.parse()?))
    },
    Ok("0") => None,
    Ok(other) => anyhow::bail!("TEAMCRAFT_SFU_FALLBACK_ENABLED must be 0 or 1, not {other:?}"),
  };
  if let Ok(text) = std::env::var("TEAMCRAFT_SFU_FALLBACK_TRUSTED_PROXIES") {
    fallback.trusted_proxies = parse_trusted_proxies(&text)
      .with_context(|| format!("TEAMCRAFT_SFU_FALLBACK_TRUSTED_PROXIES: {text:?}"))?;
  }
  if let Some(connections) = env_count("TEAMCRAFT_SFU_FALLBACK_MAX_UNJOINED_CONNECTIONS")? {
    fallback.max_unjoined_connections = connections;
  }
  if let Some(millis) = env_count("TEAMCRAFT_SFU_FALLBACK_HANDSHAKE_TIMEOUT_MILLIS")? {
    fallback.handshake_timeout = Duration::from_millis(u64::try_from(millis)?);
  }
  if let Some(millis) = env_count("TEAMCRAFT_SFU_FALLBACK_FIRST_FRAME_TIMEOUT_MILLIS")? {
    fallback.first_frame_timeout = Duration::from_millis(u64::try_from(millis)?);
  }
  fallback.requires_upgrade_token = match std::env::var("TEAMCRAFT_SFU_FALLBACK_REQUIRE_UPGRADE_TOKEN")
    .as_deref()
  {
    Err(_) | Ok("0") => false,
    Ok("1") => true,
    Ok(other) => anyhow::bail!("TEAMCRAFT_SFU_FALLBACK_REQUIRE_UPGRADE_TOKEN must be 0 or 1, not {other:?}"),
  };
  if let Some(connections) = env_count("TEAMCRAFT_SFU_FALLBACK_MAX_CONNECTIONS")? {
    fallback.max_connections = connections;
  }
  if let Some(bytes) = env_count("TEAMCRAFT_SFU_FALLBACK_QUEUE_BYTES")? {
    fallback.queue.max_bytes = bytes;
  }
  if let Some(millis) = env_count("TEAMCRAFT_SFU_FALLBACK_QUEUE_MAX_AGE_MILLIS")? {
    fallback.queue.max_age = Duration::from_millis(u64::try_from(millis)?);
  }
  if let Some(bytes) = env_count("TEAMCRAFT_SFU_FALLBACK_SEND_BUFFER_BYTES")? {
    fallback.socket_send_buffer_bytes = bytes;
  }
  Ok(())
}

/// Starts the metrics listener when `TEAMCRAFT_SFU_METRICS_ENABLED` asks for it.
fn serve_metrics(sfu: &Sfu) -> anyhow::Result<()> {
  let maybe_addrs = metrics_addrs(
    std::env::var("TEAMCRAFT_SFU_METRICS_ENABLED").ok().as_deref(),
    std::env::var("TEAMCRAFT_SFU_METRICS_ADDRS").ok().as_deref(),
  )?;
  let Some(addrs) = maybe_addrs else {
    return Ok(());
  };
  let listeners = bind_metrics(&addrs)?;
  let metrics = SfuMetrics::new()?;
  let control = sfu.control();
  spawn_metrics_endpoint(listeners, move || {
    metrics.render(control.room_stats(), control.fallback_connections())
  });
  Ok(())
}

fn load_verifier(config: &SfuConfig) -> anyhow::Result<Arc<dyn JoinTokenVerifier>> {
  if let Ok(path) = std::env::var("TEAMCRAFT_SFU_JOIN_TOKEN_PUBLIC_KEY") {
    let issuer = std::env::var("TEAMCRAFT_SFU_JOIN_TOKEN_ISSUER")
      .context("TEAMCRAFT_SFU_JOIN_TOKEN_ISSUER must be set together with the public key")?;
    let pem = std::fs::read(&path).with_context(|| format!("reading {path}"))?;
    let jwt_config = JwtJoinTokenConfig {
      issuer,
      leeway: config.join_token_leeway,
      max_lifetime: config.max_join_token_lifetime,
    };
    let verifier =
      JwtJoinTokenVerifier::new(&pem, &jwt_config).context("loading the join token public key")?;
    info!("verifying join tokens signed by the server");
    return Ok(Arc::new(verifier));
  }
  let wants_dev_tokens = std::env::var("TEAMCRAFT_SFU_INSECURE_DEV_TOKENS").as_deref() == Ok("1");
  match dev_tokens_allowed(config.bind_addr, config.maybe_dev_http_addr, wants_dev_tokens) {
    Ok(true) => {
      warn!("development mode: unsigned dev tokens are accepted");
      return Ok(Arc::new(DevTokenVerifier));
    },
    Ok(false) => {},
    Err(reason) => warn!(%reason, "unsigned dev tokens refused"),
  }
  warn!("no join token public key configured; all joins are rejected");
  Ok(Arc::new(RejectAllVerifier))
}

/// Unsigned dev tokens let anyone join any call, so they need an explicit opt-in and a listener
/// that only the local machine can reach (C10). `Ok(false)`: not requested.
fn dev_tokens_allowed(
  bind: SocketAddr,
  maybe_dev_http: Option<SocketAddr>,
  is_requested: bool,
) -> Result<bool, String> {
  if !is_requested {
    return Ok(false);
  }
  if !bind.ip().is_loopback() {
    return Err(format!("TEAMCRAFT_SFU_BIND {bind} is not a loopback address"));
  }
  match maybe_dev_http {
    Some(addr) if addr.ip().is_loopback() => Ok(true),
    Some(addr) => Err(format!("TEAMCRAFT_SFU_DEV_HTTP {addr} is not a loopback address")),
    None => Err("TEAMCRAFT_SFU_DEV_HTTP is not set".into()),
  }
}

async fn load_identity() -> anyhow::Result<Identity> {
  match (
    std::env::var("TEAMCRAFT_SFU_CERT"),
    std::env::var("TEAMCRAFT_SFU_KEY"),
  ) {
    (Ok(cert), Ok(key)) => Identity::load_pemfiles(&cert, &key)
      .await
      .context("loading certificate"),
    _ => {
      warn!("using a self-signed certificate");
      Identity::self_signed(["localhost", "127.0.0.1", "::1"]).context("generating certificate")
    },
  }
}

fn health_addr() -> anyhow::Result<SocketAddr> {
  Ok(env_addr("TEAMCRAFT_SFU_HEALTH_BIND")?.unwrap_or(DEFAULT_HEALTH_ADDR.parse()?))
}

fn env_addr(name: &str) -> anyhow::Result<Option<SocketAddr>> {
  match std::env::var(name) {
    Ok(value) => Ok(Some(
      value
        .parse()
        .with_context(|| format!("{name} is not a socket address"))?,
    )),
    Err(_) => Ok(None),
  }
}

/// A count of at least one; zero would keep every device out of every call.
fn env_count(name: &str) -> anyhow::Result<Option<usize>> {
  match std::env::var(name) {
    Ok(value) => {
      let count: usize = value.parse().with_context(|| format!("{name} is not a number"))?;
      anyhow::ensure!(count >= 1, "{name} must be at least 1");
      Ok(Some(count))
    },
    Err(_) => Ok(None),
  }
}

fn env_duration_secs(name: &str) -> anyhow::Result<Option<Duration>> {
  match std::env::var(name) {
    Ok(value) => Ok(Some(Duration::from_secs(
      value
        .parse()
        .with_context(|| format!("{name} is not a number of seconds"))?,
    ))),
    Err(_) => Ok(None),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn addr(text: &str) -> SocketAddr {
    text.parse().unwrap()
  }

  #[test]
  fn dev_tokens_need_the_flag_and_loopback_listeners() {
    let dev = Some(addr("[::1]:8080"));
    assert_eq!(dev_tokens_allowed(addr("[::1]:4433"), dev, false), Ok(false));
    assert_eq!(dev_tokens_allowed(addr("127.0.0.1:4433"), dev, true), Ok(true));
    assert!(dev_tokens_allowed(addr("[::]:4433"), dev, true).is_err());
    assert!(dev_tokens_allowed(addr("127.0.0.1:4433"), Some(addr("[::]:8080")), true).is_err());
    assert!(dev_tokens_allowed(addr("127.0.0.1:4433"), None, true).is_err());
  }
}
