//! WebTransport SFU that forwards DAVE-encrypted RTP over QUIC datagrams.
//!
//! Configuration via environment (spike; moves to the TOML config later):
//!
//! - `TEAMCRAFT_SFU_BIND`: UDP address, default `[::]:4433` (dual-stack)
//! - `TEAMCRAFT_SFU_JOIN_TOKEN_PUBLIC_KEY`: PEM file with the server's Ed25519 public key for join
//!   tokens (`[calls] signing_key_path` on the server), and `TEAMCRAFT_SFU_JOIN_TOKEN_ISSUER`: the
//!   server's `[calls] issuer`
//! - `TEAMCRAFT_SFU_HEALTH_BIND`: TCP address of the `/healthz` endpoint, default `[::]:8081`;
//!   `teamcraft-sfu healthcheck` probes it (for the container `HEALTHCHECK`)
//! - `TEAMCRAFT_SFU_SERVER_URL` and `TEAMCRAFT_SFU_REGISTRATION_TOKEN`: report joins and leaves to
//!   `teamcraft-server` (its `[calls] sfu_registration_token`), which starts calls and rings DMs
//! - `TEAMCRAFT_SFU_DEV_HTTP`: e.g. `[::1]:8080` (dual-stack); enables dev mode (test pages; unsigned
//!   dev tokens when no public key is configured)
//! - `TEAMCRAFT_SFU_DEV_ENGINE_DIR`: in dev mode, the `wasm-bindgen` output of `teamcraft-media`'s
//!   `web_call` example, served for the `/engine` test page
//! - `TEAMCRAFT_SFU_CERT` / `TEAMCRAFT_SFU_KEY`: PEM files; without them a self-signed
//!   certificate valid for 14 days is generated (browsers accept it via certificate hash)

use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context;
use teamcraft_ops::{DEFAULT_TIMEOUT, probe, probe_target, shutdown_signal};
use teamcraft_sfu::{
  auth::{DevTokenVerifier, JoinTokenVerifier, JwtJoinTokenConfig, JwtJoinTokenVerifier, RejectAllVerifier},
  config::SfuConfig,
  dev_page::serve_dev_page,
  health::{DEFAULT_HEALTH_ADDR, serve_health},
  reporting::HttpReporter,
  server::Sfu,
};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use wtransport::Identity;

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
  let local_addr = sfu.local_addr()?;
  let certificate_hash = sfu.certificate_hash();
  info!(
    %local_addr,
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
  tokio::select! {
    () = sfu.run() => {},
    result = dev_page => result.context("dev page")?,
    result = serve_health(health_addr()?) => result.context("health endpoint")?,
    () = shutdown_signal() => {},
  }
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
  if config.maybe_dev_http_addr.is_some() {
    warn!("development mode: unsigned dev tokens are accepted");
    return Ok(Arc::new(DevTokenVerifier));
  }
  warn!("no join token public key configured; all joins are rejected");
  Ok(Arc::new(RejectAllVerifier))
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
