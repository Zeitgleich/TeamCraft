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

/// The OS keystore via `keyring`: Keychain (macOS/iOS), Secret Service (Linux), Credential Manager.
#[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
pub struct KeyringKeySource {
  service: String,
  account: String,
}

/// The account's backup key kept in the OS keychain (desktop, e2ee.md §6.5.6), so this computer
/// can set the account up again without the recovery kit when its local data is lost. Same
/// keychains as [`KeyringKeySource`]; without one nothing is kept and the kit is the way back.
#[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
pub struct KeptBackupKey {
  service: String,
  account: String,
}

#[derive(Debug, thiserror::Error)]
pub enum KeySourceError {
  #[error("OS keystore unavailable: {0}")]
  Keystore(String),
  #[error("stored database key has {0} bytes, expected {SYMMETRIC_KEY_LEN}")]
  InvalidKey(usize),
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

#[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
impl KeyringKeySource {
  /// `account` separates profiles (e.g. one database per server and user).
  pub fn new(service: impl Into<String>, account: impl Into<String>) -> Self {
    Self {
      service: service.into(),
      account: account.into(),
    }
  }
}

#[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
impl DatabaseKeySource for KeyringKeySource {
  fn database_key(&self) -> Result<SymmetricKey, KeySourceError> {
    run_outside_runtime(|| self.load_or_create())
      .map_err(|error| KeySourceError::Keystore(error.to_string()))?
  }
}

#[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
impl KeyringKeySource {
  /// The keystore round trip itself; blocks, and must run outside a runtime (see
  /// [`run_outside_runtime`]).
  fn load_or_create(&self) -> Result<SymmetricKey, KeySourceError> {
    let entry = keyring::Entry::new(&self.service, &self.account)
      .map_err(|error| KeySourceError::Keystore(error.to_string()))?;
    match entry.get_secret() {
      Ok(bytes) => {
        let bytes = zeroize::Zeroizing::new(bytes);
        SymmetricKey::from_slice(&bytes).ok_or(KeySourceError::InvalidKey(bytes.len()))
      },
      Err(keyring::Error::NoEntry) => {
        let key = SymmetricKey::generate(&mut rand_core::OsRng);
        entry
          .set_secret(key.expose_secret())
          .map_err(|error| KeySourceError::Keystore(error.to_string()))?;
        Ok(key)
      },
      Err(error) => Err(KeySourceError::Keystore(error.to_string())),
    }
  }
}

#[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
impl KeptBackupKey {
  /// `account` scopes the entry like the database key's (server and user).
  pub fn new(service: impl Into<String>, account: impl Into<String>) -> Self {
    Self {
      service: service.into(),
      account: account.into(),
    }
  }

  /// The kept key, `None` when the keychain holds none for the account. Blocks on the keychain.
  pub fn load(&self) -> Result<Option<SymmetricKey>, KeySourceError> {
    self.run(|entry| match entry.get_secret() {
      Ok(bytes) => {
        let bytes = zeroize::Zeroizing::new(bytes);
        SymmetricKey::from_slice(&bytes)
          .map(Some)
          .ok_or(KeySourceError::InvalidKey(bytes.len()))
      },
      Err(keyring::Error::NoEntry) => Ok(None),
      Err(error) => Err(KeySourceError::Keystore(error.to_string())),
    })
  }

  /// Keeps `key`; an entry that already holds it is left alone, so a start writes nothing.
  pub fn keep(&self, key: &SymmetricKey) -> Result<(), KeySourceError> {
    let is_kept = match self.load() {
      Ok(Some(kept)) => kept.is_same_key(key),
      // An unreadable entry is replaced.
      Ok(None) | Err(KeySourceError::InvalidKey(_)) => false,
      Err(error) => return Err(error),
    };
    if is_kept {
      return Ok(());
    }
    self.run(|entry| {
      entry
        .set_secret(key.expose_secret())
        .map_err(|error| KeySourceError::Keystore(error.to_string()))
    })
  }

  /// Removes the entry; one that is not there counts as removed.
  pub fn forget(&self) -> Result<(), KeySourceError> {
    self.run(|entry| match entry.delete_credential() {
      Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
      Err(error) => Err(KeySourceError::Keystore(error.to_string())),
    })
  }

  /// Runs one keychain call outside any async runtime (see [`run_outside_runtime`]).
  fn run<T: Send>(
    &self,
    action: impl FnOnce(&keyring::Entry) -> Result<T, KeySourceError> + Send,
  ) -> Result<T, KeySourceError> {
    run_outside_runtime(|| {
      let entry = keyring::Entry::new(&self.service, &self.account)
        .map_err(|error| KeySourceError::Keystore(error.to_string()))?;
      action(&entry)
    })
    .map_err(|error| KeySourceError::Keystore(error.to_string()))?
  }
}

#[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
impl std::fmt::Debug for KeptBackupKey {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("KeptBackupKey")
      .field("service", &self.service)
      .finish_non_exhaustive()
  }
}

#[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
impl std::fmt::Debug for KeyringKeySource {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("KeyringKeySource")
      .field("service", &self.service)
      .finish_non_exhaustive()
  }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
  use super::*;

  /// Throwaway keystore entry of the regression test; deleted again at the end.
  #[cfg(not(any(target_os = "android", target_os = "ios")))]
  const TEST_SERVICE: &str = "de.zeitgleich.teamcraft.test-keystore-runtime";

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

  /// The desktop core's situation: the database key is fetched from inside the core's runtime, with
  /// the desktop's dependency features (zbus with `tokio`, see the dev-dependencies). Before the fix
  /// this panicked even without a Secret Service, because the panic comes before the D-Bus
  /// connection; without a keystore (CI containers) the keystore error is accepted.
  #[cfg(not(any(target_os = "android", target_os = "ios")))]
  #[tokio::test]
  async fn os_keystore_key_from_inside_a_runtime_does_not_panic() {
    let account = format!("runtime-test-{}", std::process::id());
    let source = KeyringKeySource::new(TEST_SERVICE, &account);
    match source.database_key() {
      Ok(first) => {
        let second = source.database_key();
        delete_test_entry(&account);
        assert_eq!(first.expose_secret(), second.unwrap().expose_secret());
      },
      Err(KeySourceError::Keystore(error)) => {
        assert!(!error.contains("panicked"), "{error}");
      },
      Err(error) => panic!("{error}"),
    }
  }

  /// The backup key round trip through the real keychain; without one (CI containers) the keychain
  /// error is accepted, as above.
  #[cfg(not(any(target_os = "android", target_os = "ios")))]
  #[tokio::test]
  async fn a_kept_backup_key_comes_back_and_goes() {
    let account = format!("backup-key-test-{}", std::process::id());
    let kept = KeptBackupKey::new(TEST_SERVICE, &account);
    let key = SymmetricKey::generate(&mut rand_core::OsRng);
    match kept.keep(&key) {
      Ok(()) => {
        let loaded = kept.load();
        // Keeping the same key again writes nothing and fails nothing.
        let again = kept.keep(&key);
        kept.forget().unwrap();
        assert_eq!(loaded.unwrap().unwrap().expose_secret(), key.expose_secret());
        again.unwrap();
        assert!(kept.load().unwrap().is_none());
        kept.forget().unwrap();
      },
      Err(KeySourceError::Keystore(error)) => {
        assert!(!error.contains("panicked"), "{error}");
      },
      Err(error) => panic!("{error}"),
    }
  }

  fn block_on_fresh_runtime<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
      .build()
      .unwrap()
      .block_on(future)
  }

  #[cfg(not(any(target_os = "android", target_os = "ios")))]
  fn delete_test_entry(account: &str) {
    run_outside_runtime(|| {
      let entry = keyring::Entry::new(TEST_SERVICE, account).unwrap();
      match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => {},
        Err(error) => panic!("could not delete the test entry: {error}"),
      }
    })
    .unwrap();
  }
}
