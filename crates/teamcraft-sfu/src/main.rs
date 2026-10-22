//! WebTransport SFU that forwards DAVE-encrypted RTP over QUIC datagrams.
//!
//! Configuration via environment (spike; moves to the TOML config later):
//!
//! - `TEAMCRAFT_SFU_BIND`: UDP address, default `[::]:4433` (dual-stack)
//! - `TEAMCRAFT_SFU_JOIN_TOKEN_PUBLIC_KEY`: PEM file with the server's Ed25519 public key for join
//!   tokens (`[calls] signing_key_path` on the server), and `TEAMCRAFT_SFU_JOIN_TOKEN_ISSUER`: the
//!   server's `[calls] issuer`
//! - `TEAMCRAFT_SFU_DEV_HTTP`: e.g. `[::1]:8080` (dual-stack); enables dev mode (test page; unsigned
//!   dev tokens when no public key is configured)
//! - `TEAMCRAFT_SFU_CERT` / `TEAMCRAFT_SFU_KEY`: PEM files; without them a self-signed
//!   certificate valid for 14 days is generated (browsers accept it via certificate hash)

use std::{net::SocketAddr, sync::Arc};

use anyhow::Context;
use teamcraft_sfu::{
  auth::{DevTokenVerifier, JoinTokenVerifier, JwtJoinTokenConfig, JwtJoinTokenVerifier, RejectAllVerifier},
  config::SfuConfig,
  dev_page::serve_dev_page,
  server::Sfu,
};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use wtransport::Identity;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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
  let sfu = Sfu::bind(config, identity, verifier).context("binding the SFU")?;
  let local_addr = sfu.local_addr()?;
  let certificate_hash = sfu.certificate_hash();
  info!(%local_addr, "SFU listening");

  match maybe_dev_http_addr {
    Some(http_addr) => {
      tokio::select! {
        () = sfu.run() => {},
        result = serve_dev_page(http_addr, local_addr.port(), &certificate_hash) => result.context("dev page")?,
        result = tokio::signal::ctrl_c() => result?,
      }
    },
    None => {
      tokio::select! {
        () = sfu.run() => {},
        result = tokio::signal::ctrl_c() => result?,
      }
    },
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
