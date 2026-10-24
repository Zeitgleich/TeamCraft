use secrecy::{ExposeSecret, SecretString};
use std::future::Future;
use teamcraft_core::store::run_outside_runtime;

use super::AuthError;

const SERVICE: &str = "de.zeitgleich.teamcraft";
const ACCOUNT: &str = "oidc-refresh-token";
/// The session of a local account (token and expiry as JSON), a separate entry per server.
const LOCAL_SESSION_ACCOUNT: &str = "local-session";

/// Where the refresh tokens live between runs, one per server (`server` is its origin), and the
/// sessions of local accounts next to them.
pub trait TokenStore: Send + Sync + 'static {
  fn load(&self, server: &str) -> impl Future<Output = Result<Option<SecretString>, AuthError>> + Send;
  fn save(&self, server: &str, refresh_token: &str) -> impl Future<Output = Result<(), AuthError>> + Send;
  fn clear(&self, server: &str) -> impl Future<Output = Result<(), AuthError>> + Send;
  fn load_local_session(
    &self,
    server: &str,
  ) -> impl Future<Output = Result<Option<SecretString>, AuthError>> + Send;
  fn save_local_session(
    &self,
    server: &str,
    session: &SecretString,
  ) -> impl Future<Output = Result<(), AuthError>> + Send;
  fn clear_local_session(&self, server: &str) -> impl Future<Output = Result<(), AuthError>> + Send;
}

/// The OS keyring: Secret Service on Linux, Keychain on macOS. One entry per server origin, so
/// signing in to another instance never overwrites or reuses this one's token.
#[derive(Default)]
pub struct KeyringStore;

impl KeyringStore {
  async fn run<T: Send + 'static>(
    &self,
    account: String,
    action: impl FnOnce(keyring::Entry) -> keyring::Result<T> + Send + 'static,
  ) -> Result<T, AuthError> {
    // Keyring calls block (D-Bus on Linux); keep them off the async executor, and out of any
    // runtime context, where zbus' blocking API panics (run_outside_runtime).
    tauri::async_runtime::spawn_blocking(move || {
      run_outside_runtime(move || {
        let entry = keyring::Entry::new(SERVICE, &account)?;
        action(entry)
      })
    })
    .await
    .map_err(|error| AuthError::Unsupported(error.to_string()))?
    .map_err(|error| AuthError::Unsupported(format!("keyring: {error}")))?
    .map_err(|error| AuthError::Unsupported(format!("keyring: {error}")))
  }
}

impl KeyringStore {
  async fn load_entry(&self, account: String) -> Result<Option<SecretString>, AuthError> {
    let found = self
      .run(account, |entry| match entry.get_password() {
        Ok(token) => Ok(Some(token)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(error),
      })
      .await?;
    Ok(found.map(SecretString::from))
  }

  async fn save_entry(&self, account: String, secret: SecretString) -> Result<(), AuthError> {
    self
      .run(account, move |entry| entry.set_password(secret.expose_secret()))
      .await
  }

  async fn clear_entry(&self, account: String) -> Result<(), AuthError> {
    self
      .run(account, |entry| match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(error),
      })
      .await
  }
}

impl TokenStore for KeyringStore {
  async fn load(&self, server: &str) -> Result<Option<SecretString>, AuthError> {
    self.load_entry(account(ACCOUNT, server)).await
  }

  async fn save(&self, server: &str, refresh_token: &str) -> Result<(), AuthError> {
    let token = SecretString::from(refresh_token.to_owned());
    self.save_entry(account(ACCOUNT, server), token).await
  }

  async fn clear(&self, server: &str) -> Result<(), AuthError> {
    self.clear_entry(account(ACCOUNT, server)).await
  }

  async fn load_local_session(&self, server: &str) -> Result<Option<SecretString>, AuthError> {
    self.load_entry(account(LOCAL_SESSION_ACCOUNT, server)).await
  }

  async fn save_local_session(&self, server: &str, session: &SecretString) -> Result<(), AuthError> {
    let session = SecretString::from(session.expose_secret().to_owned());
    self
      .save_entry(account(LOCAL_SESSION_ACCOUNT, server), session)
      .await
  }

  async fn clear_local_session(&self, server: &str) -> Result<(), AuthError> {
    self.clear_entry(account(LOCAL_SESSION_ACCOUNT, server)).await
  }
}

/// The keyring account of one kind of secret of one server.
fn account(kind: &str, server: &str) -> String {
  format!("{kind}@{server}")
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

  async fn load_local_session(&self, server: &str) -> Result<Option<SecretString>, AuthError> {
    self.load(&account(LOCAL_SESSION_ACCOUNT, server)).await
  }

  async fn save_local_session(&self, server: &str, session: &SecretString) -> Result<(), AuthError> {
    self
      .save(&account(LOCAL_SESSION_ACCOUNT, server), session.expose_secret())
      .await
  }

  async fn clear_local_session(&self, server: &str) -> Result<(), AuthError> {
    self.clear(&account(LOCAL_SESSION_ACCOUNT, server)).await
  }
}

#[cfg(test)]
mod tests {
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

  #[tokio::test]
  async fn local_sessions_are_separate_entries() {
    let store = MemoryStore::default();
    store.save(SERVER_A, "refresh-a").await.expect("save");
    store
      .save_local_session(SERVER_A, &SecretString::from("session-a"))
      .await
      .expect("save");
    store.clear(SERVER_A).await.expect("clear");
    let loaded = store
      .load_local_session(SERVER_A)
      .await
      .expect("load")
      .map(|session| session.expose_secret().to_owned());
    assert_eq!(loaded.as_deref(), Some("session-a"));
    assert!(store.load_local_session(SERVER_B).await.expect("load").is_none());
    store.clear_local_session(SERVER_A).await.expect("clear");
    assert!(store.load_local_session(SERVER_A).await.expect("load").is_none());
    assert_eq!(
      account(LOCAL_SESSION_ACCOUNT, SERVER_A),
      "local-session@https://a.example.com"
    );
  }
}
