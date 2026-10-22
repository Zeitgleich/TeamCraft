//! Browser test page, served over plain HTTP in development mode only.
//!
//! The page connects with `serverCertificateHashes`, so the SFU's self-signed certificate works
//! without installing a CA. Browsers treat loopback (`http://localhost`, `http://127.0.0.1`,
//! `http://[::1]`) as a secure context, which WebTransport requires. Like the SFU, the listener is
//! dual-stack for IPv6 addresses.
//!
//! - `/`: JavaScript-only audio test (WebCodecs Opus, no DAVE).
//! - `/engine`: the Rust call engine as WebAssembly (DAVE, WebCodecs, AudioWorklet), built from
//!   `teamcraft-media`'s `web_call` example into the directory given to [`serve_dev_page`]. The
//!   files are read on every request, so a rebuild needs no restart.

use std::net::SocketAddr;
use std::path::PathBuf;

use axum::{
  Router,
  http::{StatusCode, header},
  response::{Html, IntoResponse, Response},
  routing::get,
};
use tracing::info;
use wtransport::tls::{Sha256Digest, Sha256DigestFmt};

use crate::net::bind_dual_stack;

const PAGE_TEMPLATE: &str = include_str!("../static/dev.html");
const ENGINE_PAGE_TEMPLATE: &str = include_str!("../static/engine.html");
/// Output of `wasm-bindgen --target web` for the `web_call` example.
const ENGINE_JS: &str = "web_call.js";
const ENGINE_WASM: &str = "web_call_bg.wasm";

/// Serves the test pages until the task is dropped.
pub async fn serve_dev_page(
  addr: SocketAddr,
  webtransport_port: u16,
  certificate_hash: &Sha256Digest,
  maybe_engine_dir: Option<PathBuf>,
) -> std::io::Result<()> {
  let page = render(PAGE_TEMPLATE, webtransport_port, certificate_hash);
  let engine_page = render(ENGINE_PAGE_TEMPLATE, webtransport_port, certificate_hash);
  let router = Router::new()
    .route("/", get(move || async move { Html(page) }))
    .route("/engine", get(move || async move { Html(engine_page) }))
    .route(
      "/engine/web_call.js",
      get({
        let maybe_dir = maybe_engine_dir.clone();
        move || engine_file(maybe_dir, ENGINE_JS, "text/javascript")
      }),
    )
    .route(
      "/engine/web_call_bg.wasm",
      get(move || engine_file(maybe_engine_dir, ENGINE_WASM, "application/wasm")),
    );
  let listener = bind_dual_stack(addr)?;
  info!(
    "dev test pages on http://{0}/ and http://{0}/engine",
    listener.local_addr()?
  );
  axum::serve(listener, router).await
}

/// One of the two fixed engine files; nothing else from the directory is served.
async fn engine_file(maybe_dir: Option<PathBuf>, name: &'static str, content_type: &'static str) -> Response {
  let Some(dir) = maybe_dir else {
    return (
      StatusCode::NOT_FOUND,
      "no engine build: set TEAMCRAFT_SFU_DEV_ENGINE_DIR (see crates/teamcraft-media/examples/web_call.rs)",
    )
      .into_response();
  };
  match tokio::fs::read(dir.join(name)).await {
    Ok(bytes) => ([(header::CONTENT_TYPE, content_type)], bytes).into_response(),
    Err(error) => (StatusCode::NOT_FOUND, format!("{name}: {error}")).into_response(),
  }
}

fn render(template: &str, webtransport_port: u16, certificate_hash: &Sha256Digest) -> String {
  template
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
    for template in [PAGE_TEMPLATE, ENGINE_PAGE_TEMPLATE] {
      let page = render(template, 4433, &Sha256Digest::new([7; 32]));
      assert!(page.contains("const WEBTRANSPORT_PORT = 4433;"));
      assert!(page.contains("new Uint8Array([7, 7,"));
      assert!(!page.contains("__"));
    }
  }
}
