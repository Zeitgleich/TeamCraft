use secrecy::{ExposeSecret, SecretString};
use std::future::Future;
use std::sync::Arc;
use teamcraft_core::store::{KeySourceError, SecretVault, TokenKind};

use super::AuthError;

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
  /// The account confirmed last on this server, as the UI handed it over; see
  /// [`AuthService::cached_account`](super::AuthService::cached_account).
  fn load_account(&self, server: &str) -> impl Future<Output = Result<Option<String>, AuthError>> + Send;
  fn save_account(&self, server: &str, account: &str) -> impl Future<Output = Result<(), AuthError>> + Send;
  fn clear_account(&self, server: &str) -> impl Future<Output = Result<(), AuthError>> + Send;
  /// The user signed out or left: deletes what a normal start doesn't look at, such as secrets an
  /// older version of the app wrote or a damaged file set aside. `token_key` and `server` name the
  /// server's refresh token and local session.
  fn purge_after_sign_out(
    &self,
    _token_key: &str,
    _server: &str,
  ) -> impl Future<Output = Result<(), AuthError>> + Send {
    async { Ok(()) }
  }
}

/// The sealed file of local secrets (`SecretVault`, e2ee.md §9), one token of each kind per server
/// origin, so signing in to another instance never overwrites or reuses this one's token. Saving a
/// token writes the file and doesn't touch the OS keychain.
#[derive(Clone)]
pub struct VaultStore {
  vault: Arc<SecretVault>,
}

impl VaultStore {
  pub fn new(vault: Arc<SecretVault>) -> Self {
    Self { vault }
  }

  async fn run<T: Send + 'static>(
    &self,
    action: impl FnOnce(&SecretVault) -> Result<T, KeySourceError> + Send + 'static,
  ) -> Result<T, AuthError> {
    // File access and, the first time, the keychain block; keep them off the async executor.
    let vault = Arc::clone(&self.vault);
    tauri::async_runtime::spawn_blocking(move || action(&vault))
      .await
      .map_err(|error| AuthError::Unsupported(error.to_string()))?
      .map_err(|error| AuthError::Unsupported(format!("keyring: {error}")))
  }

  async fn load_token(&self, kind: TokenKind, server: &str) -> Result<Option<SecretString>, AuthError> {
    let server = server.to_owned();
    let found = self.run(move |vault| vault.load_token(kind, &server)).await?;
    Ok(found.map(|token| SecretString::from(token.as_str().to_owned())))
  }

  async fn save_token(&self, kind: TokenKind, server: &str, token: SecretString) -> Result<(), AuthError> {
    let server = server.to_owned();
    self
      .run(move |vault| vault.save_token(kind, &server, token.expose_secret()))
      .await
  }

  async fn clear_token(&self, kind: TokenKind, server: &str) -> Result<(), AuthError> {
    let server = server.to_owned();
    self.run(move |vault| vault.clear_token(kind, &server)).await
  }
}

impl TokenStore for VaultStore {
  async fn load(&self, server: &str) -> Result<Option<SecretString>, AuthError> {
    self.load_token(TokenKind::RefreshToken, server).await
  }

  async fn save(&self, server: &str, refresh_token: &str) -> Result<(), AuthError> {
    let token = SecretString::from(refresh_token.to_owned());
    self.save_token(TokenKind::RefreshToken, server, token).await
  }

  async fn clear(&self, server: &str) -> Result<(), AuthError> {
    self.clear_token(TokenKind::RefreshToken, server).await
  }

  async fn load_local_session(&self, server: &str) -> Result<Option<SecretString>, AuthError> {
    self.load_token(TokenKind::LocalSession, server).await
  }

  async fn save_local_session(&self, server: &str, session: &SecretString) -> Result<(), AuthError> {
    let session = SecretString::from(session.expose_secret().to_owned());
    self.save_token(TokenKind::LocalSession, server, session).await
  }

  async fn clear_local_session(&self, server: &str) -> Result<(), AuthError> {
    self.clear_token(TokenKind::LocalSession, server).await
  }

  async fn load_account(&self, server: &str) -> Result<Option<String>, AuthError> {
    let server = server.to_owned();
    let found = self.run(move |vault| vault.load_cached_account(&server)).await?;
    Ok(found.map(|account| account.as_str().to_owned()))
  }

  async fn save_account(&self, server: &str, account: &str) -> Result<(), AuthError> {
    let (server, account) = (server.to_owned(), account.to_owned());
    self
      .run(move |vault| vault.save_cached_account(&server, &account))
      .await
  }

  async fn clear_account(&self, server: &str) -> Result<(), AuthError> {
    let server = server.to_owned();
    self.run(move |vault| vault.clear_cached_account(&server)).await
  }

  async fn purge_after_sign_out(&self, token_key: &str, server: &str) -> Result<(), AuthError> {
    let (token_key, server) = (token_key.to_owned(), server.to_owned());
    self
      .run(move |vault| {
        vault.purge_after_sign_out(&[
          (TokenKind::RefreshToken, &token_key),
          (TokenKind::LocalSession, &server),
        ])
      })
      .await
  }
}

/// The in-memory store's name of a local session, apart from the server's refresh token. The
/// session itself is its token and expiry as JSON.
#[cfg(test)]
fn local_session_name(server: &str) -> String {
  format!("local-session@{server}")
}

/// The in-memory store's name of a cached account.
#[cfg(test)]
fn cached_account_name(server: &str) -> String {
  format!("cached-account@{server}")
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
    self.load(&local_session_name(server)).await
  }

  async fn save_local_session(&self, server: &str, session: &SecretString) -> Result<(), AuthError> {
    self
      .save(&local_session_name(server), session.expose_secret())
      .await
  }

  async fn clear_local_session(&self, server: &str) -> Result<(), AuthError> {
    self.clear(&local_session_name(server)).await
  }

  async fn load_account(&self, server: &str) -> Result<Option<String>, AuthError> {
    let found = self.load(&cached_account_name(server)).await?;
    Ok(found.map(|account| account.expose_secret().to_owned()))
  }

  async fn save_account(&self, server: &str, account: &str) -> Result<(), AuthError> {
    self.save(&cached_account_name(server), account).await
  }

  async fn clear_account(&self, server: &str) -> Result<(), AuthError> {
    self.clear(&cached_account_name(server)).await
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
  }
}
