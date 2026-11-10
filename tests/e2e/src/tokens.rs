//! Access tokens of the dev realm users, through the real authorization-code + PKCE flow of
//! `dev/dev-token.py` (the realm disables the password grant).

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    if let Some(token) = fresh(cache.as_ref()) {
      return Ok(token);
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
    let maybe_cached = self.cache.lock().ok().and_then(|cache| fresh(cache.as_ref()));
    if let Some(token) = maybe_cached {
      return Ok(token);
    }
    // A fetch blocks for a moment, on a thread of its own: `block_in_place` refuses the app's
    // runner, which runs on a `LocalSet` (crate::app).
    std::thread::scope(|scope| scope.spawn(|| self.token()).join())
      .unwrap_or_else(|_| Err("the token fetch panicked".to_owned()))
      .map_err(ApiError::Token)
  }
}

/// The cached token while it is fresh enough.
fn fresh(cached: Option<&(String, Instant)>) -> Option<String> {
  cached
    .filter(|(_, fetched_at)| fetched_at.elapsed() < REFRESH_AFTER)
    .map(|(token, _)| token.clone())
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

/// The access token as an app holds it (`WebTokens` in the web client, the mobile engine's
/// `ForeignTokens`): one token, used until it runs out, then a fresh one from the identity provider.
/// While the provider can't be reached the source answers as the apps do, "waiting for a fresh
/// access token" (a network error, not a sign-out): the session stays, and requests wait.
#[derive(Clone)]
pub struct AppTokens {
  inner: UserTokens,
  held: Arc<Mutex<Option<String>>>,
  is_provider_reachable: Arc<AtomicBool>,
}

impl AppTokens {
  pub fn new(inner: UserTokens) -> Self {
    Self {
      inner,
      held: Arc::default(),
      is_provider_reachable: Arc::new(AtomicBool::new(true)),
    }
  }

  /// The identity provider can't be reached (or is back): refreshing fails (or works again).
  pub fn set_provider_reachable(&self, is_reachable: bool) {
    self.is_provider_reachable.store(is_reachable, Ordering::SeqCst);
  }

  /// The held token ran out: the next request needs a fresh one from the provider.
  pub fn expire(&self) {
    if let Ok(mut held) = self.held.lock() {
      *held = None;
    }
  }
}

impl AccessTokenSource for AppTokens {
  fn access_token(&self) -> Result<String, ApiError> {
    let mut held = self
      .held
      .lock()
      .map_err(|_| ApiError::Token("token slot poisoned".to_owned()))?;
    if let Some(token) = held.as_ref() {
      return Ok(token.clone());
    }
    if !self.is_provider_reachable.load(Ordering::SeqCst) {
      return Err(ApiError::Network("waiting for a fresh access token".to_owned()));
    }
    let token = self.inner.access_token()?;
    *held = Some(token.clone());
    Ok(token)
  }
}
