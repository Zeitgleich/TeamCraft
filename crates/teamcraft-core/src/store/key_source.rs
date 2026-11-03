//! Where the database key comes from (e2ee.md §9: "key in the OS keystore").

use teamcraft_crypto::secret::{SYMMETRIC_KEY_LEN, SymmetricKey};

/// Name of the short-lived thread every OS keystore call runs on.
#[cfg(not(target_arch = "wasm32"))]
const KEYSTORE_THREAD_NAME: &str = "teamcraft-keystore";

/// Provides the 256-bit key of the local database, creating it on first use.
pub trait DatabaseKeySource {
  fn database_key(&self) -> Result<SymmetricKey, KeySourceError>;
}

/// A fixed key, for tests and for platforms that hand the key in from outside (mobile shells).
pub struct StaticKeySource {
  key: SymmetricKey,
}

#[derive(Debug, thiserror::Error)]
pub enum KeySourceError {
  #[error("OS keystore unavailable: {0}")]
  Keystore(String),
  #[error("stored database key has {0} bytes, expected {SYMMETRIC_KEY_LEN}")]
  InvalidKey(usize),
  /// The sealed file of local secrets could not be read or written (store/secret_vault.rs).
  #[error("the secrets file could not be used: {0}")]
  SecretsFile(String),
}

/// Why [`run_outside_runtime`] could not deliver the result of its call.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, thiserror::Error)]
pub enum KeystoreThreadError {
  #[error("could not start the keystore thread: {0}")]
  Spawn(#[source] std::io::Error),
  #[error("the keystore call panicked")]
  Panicked,
}

/// Runs a blocking OS keystore call on its own plain thread, outside any async runtime, and waits
/// for it.
///
/// `keyring`'s Secret Service backend (Linux: GNOME Keyring, KWallet/ksecretd) talks D-Bus through
/// zbus' blocking API. With zbus' `tokio` feature, which the desktop build enables, that blocks on
/// a shared tokio runtime, and tokio panics when the calling thread already drives a runtime
/// ("Cannot start a runtime from within a runtime"): the client core calls the keystore from its
/// own runtime. A fresh thread carries no runtime context, so the call works from any caller, and
/// a panic in the backend comes back as an error instead of taking the caller's thread down.
#[cfg(not(target_arch = "wasm32"))]
pub fn run_outside_runtime<T: Send>(action: impl FnOnce() -> T + Send) -> Result<T, KeystoreThreadError> {
  std::thread::scope(|scope| {
    let worker = std::thread::Builder::new()
      .name(KEYSTORE_THREAD_NAME.to_owned())
      .spawn_scoped(scope, action)
      .map_err(KeystoreThreadError::Spawn)?;
    // Joined here, so a panic surfaces as this error and the scope itself does not re-panic.
    worker.join().map_err(|_| KeystoreThreadError::Panicked)
  })
}

impl StaticKeySource {
  pub fn new(key: SymmetricKey) -> Self {
    Self { key }
  }
}

impl DatabaseKeySource for StaticKeySource {
  fn database_key(&self) -> Result<SymmetricKey, KeySourceError> {
    Ok(self.key.duplicate())
  }
}

impl std::fmt::Debug for StaticKeySource {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("StaticKeySource([redacted])")
  }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
  use super::*;

  mod outside_runtime {
    use super::*;

    #[tokio::test]
    async fn a_blocking_runtime_call_works_from_inside_a_runtime() {
      // What zbus' blocking API does under the hood; called directly here, it panics.
      let answer = run_outside_runtime(|| block_on_fresh_runtime(async { 42 })).unwrap();
      assert_eq!(answer, 42);
    }

    #[tokio::test]
    async fn the_call_sees_no_runtime_context() {
      let has_runtime = run_outside_runtime(|| tokio::runtime::Handle::try_current().is_ok()).unwrap();
      assert!(!has_runtime);
    }

    #[test]
    fn a_panicking_call_is_an_error() {
      let outcome = run_outside_runtime(|| panic!("backend bug"));
      assert!(
        matches!(outcome, Err(KeystoreThreadError::Panicked)),
        "{outcome:?}"
      );
    }

    #[test]
    fn borrows_from_the_caller() {
      let account = String::from("someone");
      let length = run_outside_runtime(|| account.len()).unwrap();
      assert_eq!(length, account.len());
    }
  }

  fn block_on_fresh_runtime<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
      .build()
      .unwrap()
      .block_on(future)
  }
}
