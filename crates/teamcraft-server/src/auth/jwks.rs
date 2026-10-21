use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use jsonwebtoken::DecodingKey;
use jsonwebtoken::jwk::JwkSet;
use serde::Deserialize;
use tokio::sync::{Mutex, RwLock};
use tokio::time::Instant;
use tracing::{debug, warn};

use super::error::AuthError;

/// Upper bound for identity provider responses; real documents are a few kilobytes.
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Where signing keys come from. A trait so the cache can be tested without a network.
pub trait JwkSource: Send + Sync {
  fn fetch(&self) -> BoxFuture<'_, Result<JwkSet, AuthError>>;
}

#[derive(Debug, Clone, Copy)]
pub struct JwksCacheOptions {
  pub ttl: Duration,
  pub min_refresh_interval: Duration,
}

/// Caches the provider's signing keys.
///
/// Keys are refreshed when they are older than the TTL or when a token names an unknown key ID
/// (key rotation), but never more often than `min_refresh_interval`, so unknown key IDs from an
/// attacker can't turn the server into a request amplifier.
pub struct JwksCache {
  source: Box<dyn JwkSource>,
  options: JwksCacheOptions,
  state: RwLock<Option<CachedKeys>>,
  // Serializes refreshes so concurrent requests with a new key ID trigger one fetch.
  refresh: Mutex<()>,
}

struct CachedKeys {
  keys: JwkSet,
  fetched_at: Instant,
}

impl JwksCache {
  pub fn new(source: Box<dyn JwkSource>, options: JwksCacheOptions) -> Self {
    Self {
      source,
      options,
      state: RwLock::new(None),
      refresh: Mutex::new(()),
    }
  }

  /// The verification key for `key_id`, refreshing the cached set if needed.
  pub async fn key(&self, key_id: &str) -> Result<DecodingKey, AuthError> {
    if let Some(key) = self.cached_key(key_id, true).await? {
      return Ok(key);
    }
    self.refresh_if_allowed().await?;
    self
      .cached_key(key_id, false)
      .await?
      .ok_or_else(|| AuthError::InvalidToken(format!("unknown signing key {key_id:?}")))
  }

  async fn cached_key(&self, key_id: &str, require_fresh: bool) -> Result<Option<DecodingKey>, AuthError> {
    let state = self.state.read().await;
    let Some(cached) = state.as_ref() else {
      return Ok(None);
    };
    if require_fresh && cached.fetched_at.elapsed() > self.options.ttl {
      return Ok(None);
    }
    match cached.keys.find(key_id) {
      Some(jwk) => DecodingKey::from_jwk(jwk)
        .map(Some)
        .map_err(|e| AuthError::InvalidToken(format!("unusable signing key: {e}"))),
      None => Ok(None),
    }
  }

  async fn refresh_if_allowed(&self) -> Result<(), AuthError> {
    let _guard = self.refresh.lock().await;
    let last_fetch = self.state.read().await.as_ref().map(|cached| cached.fetched_at);
    if let Some(fetched_at) = last_fetch
      && fetched_at.elapsed() < self.options.min_refresh_interval
    {
      debug!("signing keys were refreshed recently, not fetching again");
      return Ok(());
    }

    match self.source.fetch().await {
      Ok(keys) => {
        debug!(key_count = keys.keys.len(), "signing keys refreshed");
        *self.state.write().await = Some(CachedKeys {
          keys,
          fetched_at: Instant::now(),
        });
        Ok(())
      },
      // Keep serving with the stale set if there is one; a provider outage must not log everyone out.
      Err(error) if last_fetch.is_some() => {
        warn!(%error, "refreshing signing keys failed, using the cached set");
        Ok(())
      },
      Err(error) => Err(error),
    }
  }
}

/// Fetches keys over HTTP: OIDC discovery first, unless a `jwks_uri` is configured.
pub struct HttpJwkSource {
  client: reqwest::Client,
  issuer: String,
  maybe_jwks_uri: Option<String>,
  discovered_uri: Mutex<Option<String>>,
}

#[derive(Deserialize)]
struct DiscoveryDocument {
  issuer: String,
  jwks_uri: String,
}

impl HttpJwkSource {
  pub fn new(
    issuer: String,
    maybe_jwks_uri: Option<String>,
    timeout: Duration,
  ) -> Result<Self, reqwest::Error> {
    let client = reqwest::Client::builder().timeout(timeout).build()?;
    Ok(Self {
      client,
      issuer,
      maybe_jwks_uri,
      discovered_uri: Mutex::new(None),
    })
  }

  async fn jwks_uri(&self) -> Result<String, AuthError> {
    if let Some(uri) = &self.maybe_jwks_uri {
      return Ok(uri.clone());
    }
    let mut discovered = self.discovered_uri.lock().await;
    if let Some(uri) = discovered.as_ref() {
      return Ok(uri.clone());
    }
    let url = format!(
      "{}/.well-known/openid-configuration",
      self.issuer.trim_end_matches('/')
    );
    let document: DiscoveryDocument = self.get_json(&url).await?;
    if document.issuer != self.issuer {
      return Err(AuthError::KeysUnavailable(format!(
        "discovery document names issuer {:?}, configured is {:?}",
        document.issuer, self.issuer
      )));
    }
    *discovered = Some(document.jwks_uri.clone());
    Ok(document.jwks_uri)
  }

  async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T, AuthError> {
    let unavailable =
      |what: &str, e: &dyn std::fmt::Display| AuthError::KeysUnavailable(format!("{what} {url}: {e}"));
    let response = self
      .client
      .get(url)
      .send()
      .await
      .map_err(|e| unavailable("GET", &e))?;
    let response = response.error_for_status().map_err(|e| unavailable("GET", &e))?;
    let bytes = response.bytes().await.map_err(|e| unavailable("reading", &e))?;
    if bytes.len() > MAX_RESPONSE_BYTES {
      return Err(unavailable("reading", &"response too large"));
    }
    serde_json::from_slice(&bytes).map_err(|e| unavailable("parsing", &e))
  }
}

impl JwkSource for HttpJwkSource {
  fn fetch(&self) -> BoxFuture<'_, Result<JwkSet, AuthError>> {
    Box::pin(async move {
      let uri = self.jwks_uri().await?;
      self.get_json(&uri).await
    })
  }
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;
  use std::sync::atomic::{AtomicUsize, Ordering};

  use jsonwebtoken::jwk::Jwk;

  use super::*;
  use crate::auth::test_keys::{TEST_KEY_ID, TEST_MODULUS_B64};

  const OPTIONS: JwksCacheOptions = JwksCacheOptions {
    ttl: Duration::from_secs(600),
    min_refresh_interval: Duration::from_secs(30),
  };

  #[tokio::test(start_paused = true)]
  async fn fetches_once_for_repeated_lookups() {
    let source = FakeSource::with_key(TEST_KEY_ID);
    let cache = JwksCache::new(Box::new(source.clone()), OPTIONS);

    cache.key(TEST_KEY_ID).await.unwrap();
    cache.key(TEST_KEY_ID).await.unwrap();

    assert_eq!(source.fetches(), 1);
  }

  #[tokio::test(start_paused = true)]
  async fn unknown_key_id_refreshes_but_is_rate_limited() {
    let source = FakeSource::with_key(TEST_KEY_ID);
    let cache = JwksCache::new(Box::new(source.clone()), OPTIONS);
    cache.key(TEST_KEY_ID).await.unwrap();

    // Within the minimum interval the unknown ID does not trigger another fetch.
    assert!(cache.key("rotated").await.is_err());
    assert_eq!(source.fetches(), 1);

    tokio::time::advance(Duration::from_secs(31)).await;
    assert!(cache.key("rotated").await.is_err());
    assert_eq!(source.fetches(), 2);
  }

  #[tokio::test(start_paused = true)]
  async fn rotated_key_is_found_after_refresh() {
    let source = FakeSource::with_key("old");
    let cache = JwksCache::new(Box::new(source.clone()), OPTIONS);
    cache.key("old").await.unwrap();

    source.replace_key("new");
    tokio::time::advance(Duration::from_secs(31)).await;
    cache.key("new").await.unwrap();
  }

  #[tokio::test(start_paused = true)]
  async fn stale_keys_are_refreshed_after_ttl_and_survive_outages() {
    let source = FakeSource::with_key(TEST_KEY_ID);
    let cache = JwksCache::new(Box::new(source.clone()), OPTIONS);
    cache.key(TEST_KEY_ID).await.unwrap();

    tokio::time::advance(Duration::from_secs(601)).await;
    source.set_failing(true);
    // The refresh fails, the stale set is kept and still verifies tokens.
    cache.key(TEST_KEY_ID).await.unwrap();
    assert_eq!(source.fetches(), 2);
  }

  #[tokio::test(start_paused = true)]
  async fn first_fetch_failure_is_reported() {
    let source = FakeSource::with_key(TEST_KEY_ID);
    source.set_failing(true);
    let cache = JwksCache::new(Box::new(source), OPTIONS);
    assert!(matches!(
      cache.key(TEST_KEY_ID).await,
      Err(AuthError::KeysUnavailable(_))
    ));
  }

  #[derive(Clone)]
  struct FakeSource {
    inner: Arc<FakeInner>,
  }

  struct FakeInner {
    fetches: AtomicUsize,
    key_id: std::sync::Mutex<String>,
    is_failing: std::sync::atomic::AtomicBool,
  }

  impl FakeSource {
    fn with_key(key_id: &str) -> Self {
      Self {
        inner: Arc::new(FakeInner {
          fetches: AtomicUsize::new(0),
          key_id: std::sync::Mutex::new(key_id.to_owned()),
          is_failing: std::sync::atomic::AtomicBool::new(false),
        }),
      }
    }

    fn fetches(&self) -> usize {
      self.inner.fetches.load(Ordering::SeqCst)
    }

    fn replace_key(&self, key_id: &str) {
      *self.inner.key_id.lock().unwrap() = key_id.to_owned();
    }

    fn set_failing(&self, is_failing: bool) {
      self.inner.is_failing.store(is_failing, Ordering::SeqCst);
    }
  }

  impl JwkSource for FakeSource {
    fn fetch(&self) -> BoxFuture<'_, Result<JwkSet, AuthError>> {
      Box::pin(async move {
        self.inner.fetches.fetch_add(1, Ordering::SeqCst);
        if self.inner.is_failing.load(Ordering::SeqCst) {
          return Err(AuthError::KeysUnavailable("fake outage".into()));
        }
        let key_id = self.inner.key_id.lock().unwrap().clone();
        let jwk: Jwk = serde_json::from_value(serde_json::json!({
          "kty": "RSA", "kid": key_id, "use": "sig", "alg": "RS256", "n": TEST_MODULUS_B64, "e": "AQAB"
        }))
        .unwrap();
        Ok(JwkSet { keys: vec![jwk] })
      })
    }
  }
}
