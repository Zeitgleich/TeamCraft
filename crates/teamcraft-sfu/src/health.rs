//! Health endpoint for container orchestration: `GET /healthz` answers 200 while the process runs.
//!
//! The SFU itself speaks QUIC, which a container health check can't probe without a client, so a
//! tiny HTTP listener (default `[::]:8081`, `TEAMCRAFT_SFU_HEALTH_BIND`) stands in. It carries no
//! data and no secrets. The same internal listener carries the host control route
//! ([`crate::control`]) when the SFU has a registration token.

use std::net::SocketAddr;

use axum::{Router, routing::get};
use tracing::info;

use crate::net::bind_dual_stack;

/// Default address of the health listener, also what `teamcraft-sfu healthcheck` probes.
pub const DEFAULT_HEALTH_ADDR: &str = "[::]:8081";

/// Serves `/healthz`, plus the routes in `extra`, until the task is dropped.
pub async fn serve_health(addr: SocketAddr, extra: Router) -> std::io::Result<()> {
  let listener = bind_dual_stack(addr)?;
  info!("health endpoint on http://{}/healthz", listener.local_addr()?);
  axum::serve(listener, router().merge(extra)).await
}

fn router() -> Router {
  Router::new().route("/healthz", get(|| async { "ok" }))
}

#[cfg(test)]
mod tests {
  use std::net::{Ipv4Addr, Ipv6Addr};
  use std::time::Duration;

  use teamcraft_ops::probe;

  use super::*;

  #[tokio::test]
  async fn healthz_answers_on_ipv4_and_ipv6() {
    // Hosts without IPv6 cannot bind `[::]`; nothing to test there.
    let Ok(listener) = bind_dual_stack((Ipv6Addr::UNSPECIFIED, 0).into()) else {
      return;
    };
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, router()).await });

    for addr in [
      SocketAddr::from((Ipv6Addr::LOCALHOST, port)),
      SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
    ] {
      let result = tokio::task::spawn_blocking(move || probe(addr, "/healthz", Duration::from_secs(2)))
        .await
        .unwrap();
      assert!(result.is_ok(), "{addr}: {result:?}");
    }
  }
}
