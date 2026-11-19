//! Keeps the seed users' devices of the demo instance online (docs/operations.md, "Demo instance";
//! `teamcraft_e2e::demo_peers`): they let a reviewer's new device into the conversations' encryption
//! groups, so the reviewer can write, and answer a reviewer's direct message once. Runs as a
//! service beside the server, from the stores `demo-seed` wrote (`DEMO_PEER_STORES`), which the
//! nightly reset puts back with the database (`dev/demo/demo-reset.sh`, stopped meanwhile):
//!
//! ```sh
//! E2E_DEPLOYED_URL=https://demo.example.com DEV_TOKEN_ISSUER=https://id.demo.example.com \
//!   DEV_TOKEN_PASSWORD=... DEV_TOKEN_SCOPES="openid profile email groups" \
//!   DEV_TOKEN_SCRIPT=/opt/teamcraft-demo/dev-token.py DEMO_PEER_STORES=/var/lib/teamcraft-demo/peers \
//!   demo-peers
//! ```
//!
//! Stops cleanly on SIGTERM or Ctrl-C (closes the gateway connections and the stores). Exits 2 when
//! a setting or a seed store is missing.
// An operator tool built on the test harness: it reports on stderr, as the harness does.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

use std::path::PathBuf;

use teamcraft_e2e::demo::{Cast, has_seed_store};
use teamcraft_e2e::demo_peers::start_all;
use tokio::signal::unix::{SignalKind, signal};

const URL_VAR: &str = "E2E_DEPLOYED_URL";
/// The directory with one store per seed user, as `demo-seed` wrote it.
const STORES_VAR: &str = "DEMO_PEER_STORES";
/// As for `demo-seed`: appended to the user names to make the identity provider's logins.
const LOGIN_SUFFIX_VAR: &str = "DEMO_LOGIN_SUFFIX";
const DEFAULT_LOGIN_SUFFIX: &str = "@example.com";
/// The core's own log (`warn` by default), e.g. `DEMO_PEERS_LOG=teamcraft_core=info`.
const LOG_VAR: &str = "DEMO_PEERS_LOG";
/// Other logins than the cast's (comma-separated), e.g. the dev realm's in the e2e check.
const LOGINS_VAR: &str = "DEMO_PEER_LOGINS";

// One thread: four idle devices need no more, and every thread's stack and allocator arena counts.
#[tokio::main(flavor = "current_thread")]
async fn main() {
  let filter = std::env::var(LOG_VAR).unwrap_or_else(|_| "warn".to_owned());
  tracing_subscriber::fmt()
    .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
    .with_writer(std::io::stderr)
    .init();
  let base_url = required(URL_VAR);
  let stores = PathBuf::from(required(STORES_VAR));
  let suffix = std::env::var(LOGIN_SUFFIX_VAR).unwrap_or_else(|_| DEFAULT_LOGIN_SUFFIX.to_owned());
  let cast = Cast::demo(&suffix);
  let maybe_logins = std::env::var(LOGINS_VAR).ok();
  let logins: Vec<&str> = match &maybe_logins {
    Some(logins) => logins
      .split(',')
      .map(str::trim)
      .filter(|login| !login.is_empty())
      .collect(),
    None => cast.peer_logins(),
  };
  if let Some(missing) = logins.iter().find(|login| !has_seed_store(&stores, login)) {
    eprintln!(
      "no seed store of {missing} in {}: run demo-seed with {STORES_VAR}",
      stores.display()
    );
    std::process::exit(2);
  }
  let mut terminate = signal(SignalKind::terminate()).expect("listening for SIGTERM");
  let local = tokio::task::LocalSet::new();
  local
    .run_until(async {
      let peers = start_all(base_url.trim_end_matches('/'), &stores, &logins);
      eprintln!("demo-peers: {} seed devices started", peers.len());
      tokio::select! {
        _ = terminate.recv() => {},
        _ = tokio::signal::ctrl_c() => {},
      }
      for peer in peers {
        peer.stop().await;
      }
      eprintln!("demo-peers: stopped");
    })
    .await;
}

fn required(name: &str) -> String {
  std::env::var(name).unwrap_or_else(|_| {
    eprintln!("{name} is not set");
    std::process::exit(2);
  })
}
