//! WebTransport SFU that forwards DAVE-encrypted RTP over QUIC datagrams.
//!
//! Configuration via environment (spike; moves to the TOML config later):
//!
//! - `TEAMCRAFT_SFU_BIND`: UDP address, default `[::]:4433` (dual-stack)
//! - `TEAMCRAFT_SFU_DEV_HTTP`: e.g. `[::1]:8080` (dual-stack); enables dev mode (test page, dev tokens)
//! - `TEAMCRAFT_SFU_CERT` / `TEAMCRAFT_SFU_KEY`: PEM files; without them a self-signed
//!   certificate valid for 14 days is generated (browsers accept it via certificate hash)

use std::{net::SocketAddr, sync::Arc};

use anyhow::Context;
use teamcraft_sfu::{
  auth::{DevTokenVerifier, JoinTokenVerifier, RejectAllVerifier},
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
  let verifier: Arc<dyn JoinTokenVerifier> = if config.maybe_dev_http_addr.is_some() {
    warn!("development mode: unsigned dev tokens are accepted");
    Arc::new(DevTokenVerifier)
  } else {
    warn!("no join token verifier configured yet; all joins are rejected");
    Arc::new(RejectAllVerifier)
  };

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
