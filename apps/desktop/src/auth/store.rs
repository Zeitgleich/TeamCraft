use secrecy::SecretString;
use std::future::Future;

use super::AuthError;

const SERVICE: &str = "de.zeitgleich.teamcraft";
const ACCOUNT: &str = "oidc-refresh-token";

/// Where the refresh token lives between runs.
pub trait TokenStore: Send + Sync + 'static {
  fn load(&self) -> impl Future<Output = Result<Option<SecretString>, AuthError>> + Send;
  fn save(&self, refresh_token: &str) -> impl Future<Output = Result<(), AuthError>> + Send;
  fn clear(&self) -> impl Future<Output = Result<(), AuthError>> + Send;
}

/// The OS keyring: Secret Service on Linux, Keychain on macOS.
pub struct KeyringStore {
  /// Distinguishes servers, so signing in to another instance does not overwrite this one.
  account: String,
}

impl KeyringStore {
  pub fn new(issuer: &str) -> Self {
    Self {
      account: format!("{ACCOUNT}@{issuer}"),
    }
  }

  async fn run<T: Send + 'static>(
    &self,
    action: impl FnOnce(keyring::Entry) -> keyring::Result<T> + Send + 'static,
  ) -> Result<T, AuthError> {
    let account = self.account.clone();
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
  async fn load(&self) -> Result<Option<SecretString>, AuthError> {
    let found = self
      .run(|entry| match entry.get_password() {
        Ok(token) => Ok(Some(token)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(error),
      })
      .await?;
    Ok(found.map(SecretString::from))
  }

  async fn save(&self, refresh_token: &str) -> Result<(), AuthError> {
    let token = refresh_token.to_owned();
    self.run(move |entry| entry.set_password(&token)).await
  }

  async fn clear(&self) -> Result<(), AuthError> {
    self
      .run(|entry| match entry.delete_credential() {
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
  token: std::sync::Mutex<Option<String>>,
}

#[cfg(test)]
impl TokenStore for MemoryStore {
  async fn load(&self) -> Result<Option<SecretString>, AuthError> {
    Ok(
      self
        .token
        .lock()
        .map(|token| token.clone())
        .unwrap_or_default()
        .map(SecretString::from),
    )
  }

  async fn save(&self, refresh_token: &str) -> Result<(), AuthError> {
    if let Ok(mut token) = self.token.lock() {
      *token = Some(refresh_token.to_owned());
    }
    Ok(())
  }

  async fn clear(&self) -> Result<(), AuthError> {
    if let Ok(mut token) = self.token.lock() {
      *token = None;
    }
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use secrecy::ExposeSecret;

  use super::*;

  #[tokio::test]
  async fn memory_store_round_trips() {
    let store = MemoryStore::default();
    assert!(store.load().await.expect("load").is_none());
    store.save("refresh-1").await.expect("save");
    let loaded = store
      .load()
      .await
      .expect("load")
      .map(|token| token.expose_secret().to_owned());
    assert_eq!(loaded.as_deref(), Some("refresh-1"));
    store.clear().await.expect("clear");
    assert!(store.load().await.expect("load").is_none());
  }
}
