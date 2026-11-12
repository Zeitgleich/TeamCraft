//! Seeds the demo instance for store reviews (docs/operations.md, "Demo instance"; issue #1628).
//!
//! Real clients do the work, so keys, MLS groups and attachments are what any app would make: the
//! seeding run signs in every demo user at the instance's identity provider, enrolls one device
//! each, signs the workspace policy, and fills a space, channels, direct messages and files. The
//! operator then takes the snapshot (`dev/demo/demo-reset.sh snapshot`) that the nightly reset
//! restores. Run it once against an empty instance:
//!
//! ```sh
//! E2E_DEPLOYED_URL=https://demo.example.com DEV_TOKEN_ISSUER=https://demo.example.com/dex \
//!   DEV_TOKEN_PASSWORD=... DEV_TOKEN_SCOPES="openid profile email groups" \
//!   DEMO_RECOVERY_PASSPHRASE=... \
//!   cargo run -p teamcraft-e2e --features e2e --bin demo-seed
//! ```
//!
//! Everything here is placeholder content: no real people, companies or hosts. The seed itself is
//! `teamcraft_e2e::demo` (tests/e2e/src/demo.rs), which the `demo_signin` scenario also plays on a
//! test server (#1743). To change it, edit that module, then run this again on an empty instance
//! and take a new snapshot.
// A one-shot operator tool built on the test harness: it reports on stderr and stops at the first
// failure, as the harness does.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_e2e::Stack;
use teamcraft_e2e::demo::{Cast, seed};

/// The passphrase that opens every demo user's key backup: reviewers type it on their first
/// sign-in after a reset (operations.md, "Review instructions").
const PASSPHRASE_VAR: &str = "DEMO_RECOVERY_PASSPHRASE";
/// Appended to the user names (`owner`, `alex`, `sam`, `robin`, `reviewer-1`, `reviewer-2`) to make
/// the logins of the identity provider (Dex signs in with the e-mail address).
const LOGIN_SUFFIX_VAR: &str = "DEMO_LOGIN_SUFFIX";
const DEFAULT_LOGIN_SUFFIX: &str = "@example.com";

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
  let Some(stack) = Stack::deployed() else {
    std::process::exit(2);
  };
  let Ok(passphrase) = std::env::var(PASSPHRASE_VAR) else {
    eprintln!("{PASSPHRASE_VAR} is not set");
    std::process::exit(2);
  };
  let suffix = std::env::var(LOGIN_SUFFIX_VAR).unwrap_or_else(|_| DEFAULT_LOGIN_SUFFIX.to_owned());
  // The apps' own key stretching: the reviewers' phones open the backup with it.
  seed(
    &stack,
    &Cast::demo(&suffix),
    &passphrase,
    Argon2Settings::default(),
  )
  .await;
  eprintln!("demo seed done: take the snapshot now (dev/demo/demo-reset.sh snapshot)");
}
