//! Access tokens of the dev realm users, through the real authorization-code + PKCE flow of
//! `dev/dev-token.py` (the realm disables the password grant).

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use teamcraft_core::api::{AccessTokenSource, ApiError};

/// Dev tokens live five minutes; fetch a new one well before.
const REFRESH_AFTER: Duration = Duration::from_secs(150);

/// Tokens of one dev user, refreshed on demand. Cheap to clone; clones share the cache.
#[derive(Clone)]
pub struct UserTokens {
  user: String,
  cache: Arc<Mutex<Option<(String, Instant)>>>,
  /// How often a client asked for the token: once per API request and gateway handshake, so the
  /// perf tests count a client's requests with it.
  uses: Arc<AtomicU64>,
}

impl UserTokens {
  pub fn new(user: &str) -> Self {
    Self {
      user: user.to_owned(),
      cache: Arc::default(),
      uses: Arc::default(),
    }
  }

  /// Requests made with these tokens so far (API calls and gateway handshakes).
  pub fn uses(&self) -> u64 {
    self.uses.load(Ordering::Relaxed)
  }

  pub fn user(&self) -> &str {
    &self.user
  }

  /// A valid access token (blocks for a moment when a new one is fetched).
  pub fn token(&self) -> Result<String, String> {
    let mut cache = self.cache.lock().map_err(|_| "token cache poisoned".to_owned())?;
    if let Some((token, fetched_at)) = cache.as_ref()
      && fetched_at.elapsed() < REFRESH_AFTER
    {
      return Ok(token.clone());
    }
    let token = fetch(&self.user)?;
    *cache = Some((token.clone(), Instant::now()));
    Ok(token)
  }

  /// Like [`Self::token`] from async code without stalling the runtime's other tasks.
  pub async fn token_async(&self) -> String {
    let tokens = self.clone();
    tokio::task::spawn_blocking(move || tokens.token())
      .await
      .expect("token task")
      .unwrap_or_else(|error| panic!("token of {}: {error}", self.user))
  }
}

impl AccessTokenSource for UserTokens {
  fn access_token(&self) -> Result<String, ApiError> {
    self.uses.fetch_add(1, Ordering::Relaxed);
    // `block_in_place` needs the multi-thread runtime, which every e2e test uses.
    tokio::task::block_in_place(|| self.token()).map_err(ApiError::Token)
  }
}

fn fetch(user: &str) -> Result<String, String> {
  let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../dev/dev-token.py");
  let output = Command::new("python3")
    .arg(script)
    .arg(user)
    .output()
    .map_err(|error| format!("running python3: {error}"))?;
  if !output.status.success() {
    return Err(format!(
      "dev-token.py failed: {}",
      String::from_utf8_lossy(&output.stderr)
    ));
  }
  Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
