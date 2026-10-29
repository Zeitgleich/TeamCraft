//! The OpenRouter API key (e2ee.md §8.2): from the notetaker's configuration, or fetched from the
//! server, which hands it only to the bot account the signed policy names. Either way it is used
//! only if its SHA-256 is the one the owners signed into the policy, so a compromised server can't
//! swap in a key of an account of its own.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use teamcraft_crypto::service_key::is_api_key_sha256;
use teamcraft_proto::{NotetakerCredentialsResponse, NotetakerPolicy};
use thiserror::Error;
use tracing::{info, warn};

use crate::openrouter::{OpenRouterClient, RetrySettings};

/// How long a fetch of the key may take.
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);

/// Where the key comes from.
pub trait KeySource: Send + Sync {
  fn fetch(&self) -> KeyFuture;
}

/// A key on its way.
pub type KeyFuture = Pin<Box<dyn Future<Output = Result<SecretString, KeyError>> + Send>>;

/// The key of the configuration file.
pub struct ConfiguredKey(pub SecretString);

/// The key the server stores, fetched with the bot's token.
pub struct ServerKey {
  http: reqwest::Client,
  url: String,
  bot_token: SecretString,
}

/// The OpenRouter client for the key the policy signed, made once per signed hash.
pub struct Credentials {
  source: Arc<dyn KeySource>,
  settings: ClientSettings,
  maybe_current: Option<(String, Arc<OpenRouterClient>)>,
  /// A hash whose key didn't match or couldn't be fetched, so it is reported once.
  maybe_failed_hash: Option<String>,
}

/// How the OpenRouter client is built.
#[derive(Debug, Clone)]
pub struct ClientSettings {
  pub base_url: String,
  pub request_timeout: Duration,
  pub retry: RetrySettings,
  pub maybe_app_title: Option<String>,
}

#[derive(Debug, Error)]
pub enum KeyError {
  #[error("the policy has no API key hash")]
  NoSignedHash,
  #[error("the server has no API key stored")]
  NotStored,
  #[error("the API key does not match the hash the owners signed")]
  HashMismatch,
  #[error("fetching the API key failed: {0}")]
  Fetch(String),
  #[error("building the OpenRouter client failed: {0}")]
  Client(String),
}

impl KeySource for ConfiguredKey {
  fn fetch(&self) -> KeyFuture {
    let key = self.0.clone();
    Box::pin(async move { Ok(key) })
  }
}

impl ServerKey {
  /// `server_url`: the TeamCraft server's base URL, as for the core.
  pub fn new(server_url: &str, bot_token: SecretString) -> Result<Self, KeyError> {
    let http = reqwest::Client::builder()
      .timeout(FETCH_TIMEOUT)
      .build()
      .map_err(|error| KeyError::Fetch(error.to_string()))?;
    Ok(Self {
      http,
      url: format!(
        "{}/api/v1/notetaker/credentials",
        server_url.trim_end_matches('/')
      ),
      bot_token,
    })
  }
}

impl KeySource for ServerKey {
  fn fetch(&self) -> KeyFuture {
    let request = self
      .http
      .get(&self.url)
      .bearer_auth(self.bot_token.expose_secret());
    Box::pin(async move {
      let response = request
        .send()
        .await
        .map_err(|error| KeyError::Fetch(error.without_url().to_string()))?;
      let status = response.status();
      if status == reqwest::StatusCode::NOT_FOUND {
        return Err(KeyError::NotStored);
      }
      if !status.is_success() {
        return Err(KeyError::Fetch(format!("the server answered {status}")));
      }
      let body: NotetakerCredentialsResponse = response
        .json()
        .await
        .map_err(|error| KeyError::Fetch(error.without_url().to_string()))?;
      Ok(SecretString::from(body.api_key.expose_secret().to_owned()))
    })
  }
}

impl Credentials {
  pub fn new(source: Arc<dyn KeySource>, settings: ClientSettings) -> Self {
    Self {
      source,
      settings,
      maybe_current: None,
      maybe_failed_hash: None,
    }
  }

  /// The client for the key `policy` signed: the one made before while the hash stays the same,
  /// otherwise a fresh key, checked against the hash.
  pub async fn client(&mut self, policy: &NotetakerPolicy) -> Result<Arc<OpenRouterClient>, KeyError> {
    let hash = policy
      .maybe_api_key_sha256
      .clone()
      .ok_or(KeyError::NoSignedHash)?;
    if let Some((current_hash, client)) = &self.maybe_current
      && *current_hash == hash
    {
      return Ok(Arc::clone(client));
    }
    match self.fetch_checked(&hash).await {
      Ok(client) => {
        info!("OpenRouter key loaded; it matches the signed hash");
        self.maybe_failed_hash = None;
        self.maybe_current = Some((hash, Arc::clone(&client)));
        Ok(client)
      },
      Err(error) => {
        if self.maybe_failed_hash.as_deref() != Some(hash.as_str()) {
          warn!(%error, "no usable OpenRouter key");
          self.maybe_failed_hash = Some(hash);
        }
        Err(error)
      },
    }
  }

  /// Forgets the client, e.g. after OpenRouter refused its key.
  pub fn forget(&mut self) {
    self.maybe_current = None;
  }

  async fn fetch_checked(&self, hash: &str) -> Result<Arc<OpenRouterClient>, KeyError> {
    let key = self.source.fetch().await?;
    if !is_api_key_sha256(key.expose_secret(), hash) {
      return Err(KeyError::HashMismatch);
    }
    let trimmed = SecretString::from(key.expose_secret().trim().to_owned());
    let client = OpenRouterClient::new(
      &self.settings.base_url,
      trimmed,
      self.settings.request_timeout,
      self.settings.retry,
      self.settings.maybe_app_title.clone(),
    )
    .map_err(|error| KeyError::Client(error.to_string()))?;
    Ok(Arc::new(client))
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_crypto::service_key::api_key_sha256_hex;

  use super::*;

  #[tokio::test]
  async fn a_key_is_used_only_with_the_signed_hash() {
    let mut credentials = credentials("sk-or-v1-good");
    let mut policy = NotetakerPolicy {
      maybe_api_key_sha256: Some(api_key_sha256_hex("sk-or-v1-good")),
      ..NotetakerPolicy::default()
    };
    assert!(credentials.client(&policy).await.is_ok());
    policy.maybe_api_key_sha256 = Some(api_key_sha256_hex("sk-or-v1-other"));
    assert!(matches!(
      credentials.client(&policy).await,
      Err(KeyError::HashMismatch)
    ));
    policy.maybe_api_key_sha256 = None;
    assert!(matches!(
      credentials.client(&policy).await,
      Err(KeyError::NoSignedHash)
    ));
  }

  #[tokio::test]
  async fn the_client_is_kept_while_the_hash_stays() {
    let mut credentials = credentials(" sk-or-v1-good\n");
    let policy = NotetakerPolicy {
      maybe_api_key_sha256: Some(api_key_sha256_hex("sk-or-v1-good")),
      ..NotetakerPolicy::default()
    };
    let first = credentials.client(&policy).await.unwrap();
    let second = credentials.client(&policy).await.unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    credentials.forget();
    assert!(!Arc::ptr_eq(&first, &credentials.client(&policy).await.unwrap()));
  }

  fn credentials(key: &str) -> Credentials {
    Credentials::new(
      Arc::new(ConfiguredKey(SecretString::from(key.to_owned()))),
      ClientSettings {
        base_url: "https://openrouter.invalid/api/v1".into(),
        request_timeout: Duration::from_secs(1),
        retry: RetrySettings {
          max_retries: 0,
          base_delay: Duration::from_millis(1),
          max_delay: Duration::from_millis(1),
        },
        maybe_app_title: None,
      },
    )
  }
}
