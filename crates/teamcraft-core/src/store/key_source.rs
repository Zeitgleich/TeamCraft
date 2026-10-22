//! Where the database key comes from (e2ee.md §9: "key in the OS keystore").

use teamcraft_crypto::secret::{SYMMETRIC_KEY_LEN, SymmetricKey};

/// Provides the 256-bit key of the local database, creating it on first use.
pub trait DatabaseKeySource {
  fn database_key(&self) -> Result<SymmetricKey, KeySourceError>;
}

/// A fixed key, for tests and for platforms that hand the key in from outside (mobile shells).
pub struct StaticKeySource {
  key: SymmetricKey,
}

/// The OS keystore via `keyring`: Keychain (macOS/iOS), Secret Service (Linux), Credential Manager.
#[cfg(not(target_arch = "wasm32"))]
pub struct KeyringKeySource {
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

#[cfg(not(target_arch = "wasm32"))]
impl KeyringKeySource {
  /// `account` separates profiles (e.g. one database per server and user).
  pub fn new(service: impl Into<String>, account: impl Into<String>) -> Self {
    Self {
      service: service.into(),
      account: account.into(),
    }
  }
}

#[cfg(not(target_arch = "wasm32"))]
impl DatabaseKeySource for KeyringKeySource {
  fn database_key(&self) -> Result<SymmetricKey, KeySourceError> {
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

#[cfg(not(target_arch = "wasm32"))]
impl std::fmt::Debug for KeyringKeySource {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("KeyringKeySource")
      .field("service", &self.service)
      .finish_non_exhaustive()
  }
}
