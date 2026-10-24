use secrecy::SecretString;
use std::future::Future;

use super::AuthError;

const SERVICE: &str = "de.zeitgleich.teamcraft";
const ACCOUNT: &str = "oidc-refresh-token";

/// Where the refresh tokens live between runs, one per server (`server` is its origin).
pub trait TokenStore: Send + Sync + 'static {
  fn load(&self, server: &str) -> impl Future<Output = Result<Option<SecretString>, AuthError>> + Send;
  fn save(&self, server: &str, refresh_token: &str) -> impl Future<Output = Result<(), AuthError>> + Send;
  fn clear(&self, server: &str) -> impl Future<Output = Result<(), AuthError>> + Send;
}

/// The OS keyring: Secret Service on Linux, Keychain on macOS. One entry per server origin, so
/// signing in to another instance never overwrites or reuses this one's token.
#[derive(Default)]
pub struct KeyringStore;

impl KeyringStore {
  async fn run<T: Send + 'static>(
    &self,
    server: &str,
    action: impl FnOnce(keyring::Entry) -> keyring::Result<T> + Send + 'static,
  ) -> Result<T, AuthError> {
    let account = format!("{ACCOUNT}@{server}");
    // Keyring calls block (D-Bus on Linux); keep them off the async executor.
    tauri::async_runtime::spawn_blocking(move || {
      let entry = keyring::Entry::new(SERVICE, &account)?;
      action(entry)
    })
    .await
    .map_err(|error| AuthError::Unsupported(error.to_string()))?
    .map_err(|error| AuthError::Unsupported(format!("keyring: {error}")))
  }
}

impl TokenStore for KeyringStore {
  async fn load(&self, server: &str) -> Result<Option<SecretString>, AuthError> {
    let found = self
      .run(server, |entry| match entry.get_password() {
        Ok(token) => Ok(Some(token)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(error),
      })
      .await?;
    Ok(found.map(SecretString::from))
  }

  async fn save(&self, server: &str, refresh_token: &str) -> Result<(), AuthError> {
    let token = refresh_token.to_owned();
    self.run(server, move |entry| entry.set_password(&token)).await
  }

  async fn clear(&self, server: &str) -> Result<(), AuthError> {
    self
      .run(server, |entry| match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(error),
      })
      .await
  }
}

/// In-memory store for tests.
#[cfg(test)]
#[derive(Default)]
pub struct MemoryStore {
  tokens: std::sync::Mutex<std::collections::HashMap<String, String>>,
}

#[cfg(test)]
impl TokenStore for MemoryStore {
  async fn load(&self, server: &str) -> Result<Option<SecretString>, AuthError> {
    Ok(
      self
        .tokens
        .lock()
        .ok()
        .and_then(|tokens| tokens.get(server).cloned())
        .map(SecretString::from),
    )
  }

  async fn save(&self, server: &str, refresh_token: &str) -> Result<(), AuthError> {
    if let Ok(mut tokens) = self.tokens.lock() {
      tokens.insert(server.to_owned(), refresh_token.to_owned());
    }
    Ok(())
  }

  async fn clear(&self, server: &str) -> Result<(), AuthError> {
    if let Ok(mut tokens) = self.tokens.lock() {
      tokens.remove(server);
    }
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use secrecy::ExposeSecret;

  use super::*;

  const SERVER_A: &str = "https://a.example.com";
  const SERVER_B: &str = "https://b.example.com";

  #[tokio::test]
  async fn memory_store_round_trips_per_server() {
    let store = MemoryStore::default();
    assert!(store.load(SERVER_A).await.expect("load").is_none());
    store.save(SERVER_A, "refresh-a").await.expect("save");
    store.save(SERVER_B, "refresh-b").await.expect("save");
    let loaded = store
      .load(SERVER_A)
      .await
      .expect("load")
      .map(|token| token.expose_secret().to_owned());
    assert_eq!(loaded.as_deref(), Some("refresh-a"));
    store.clear(SERVER_A).await.expect("clear");
    assert!(store.load(SERVER_A).await.expect("load").is_none());
    assert!(store.load(SERVER_B).await.expect("load").is_some());
  }
}
