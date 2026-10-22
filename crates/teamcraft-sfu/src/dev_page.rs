//! Browser test page, served over plain HTTP in development mode only.
//!
//! The page connects with `serverCertificateHashes`, so the SFU's self-signed certificate works
//! without installing a CA. Browsers treat loopback (`http://localhost`, `http://127.0.0.1`,
//! `http://[::1]`) as a secure context, which WebTransport requires. Like the SFU, the listener is
//! dual-stack for IPv6 addresses.

use std::net::SocketAddr;

use axum::{Router, response::Html, routing::get};
use tracing::info;

use crate::net::bind_dual_stack;
use wtransport::tls::{Sha256Digest, Sha256DigestFmt};

const PAGE_TEMPLATE: &str = include_str!("../static/dev.html");

/// Serves the test page until the task is dropped.
pub async fn serve_dev_page(
  addr: SocketAddr,
  webtransport_port: u16,
  certificate_hash: &Sha256Digest,
) -> std::io::Result<()> {
  let page = render(webtransport_port, certificate_hash);
  let router = Router::new().route("/", get(move || async move { Html(page) }));
  let listener = bind_dual_stack(addr)?;
  info!("dev test page on http://{}/", listener.local_addr()?);
  axum::serve(listener, router).await
}

fn render(webtransport_port: u16, certificate_hash: &Sha256Digest) -> String {
  PAGE_TEMPLATE
    .replace("__WEBTRANSPORT_PORT__", &webtransport_port.to_string())
    .replace(
      "__CERT_HASH__",
      &certificate_hash.fmt(Sha256DigestFmt::BytesArray),
    )
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn placeholders_are_replaced() {
    let page = render(4433, &Sha256Digest::new([7; 32]));
    assert!(page.contains("const WEBTRANSPORT_PORT = 4433;"));
    assert!(page.contains("new Uint8Array([7, 7,"));
    assert!(!page.contains("__"));
  }
}
