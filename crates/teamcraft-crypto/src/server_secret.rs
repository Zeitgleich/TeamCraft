//! Encryption at rest of the secrets an admin enters in the console (SMTP and CalDAV passwords,
//! admin.md §6.2). This is not end-to-end encryption: the server needs the plaintext to log in to
//! those services. It keeps the secrets out of database dumps and backups; the key lives in the
//! server's configuration or a key file, never in the database.
//!
//! From the configured 256-bit key, HKDF-SHA256 derives the sealing key. Each secret is sealed with
//! XChaCha20-Poly1305 and the setting's name as associated data, so a sealed value cannot be moved
//! to another setting unnoticed.
//!
//! Layout: `version (1) || nonce (24) || ciphertext || tag (16)`.

use hkdf::Hkdf;
use rand_core::CryptoRngCore;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::aead::{self, NONCE_LEN, TAG_LEN};
use crate::domain::{self, SERVER_SETTING_AAD, SERVER_SETTING_KEY};
use crate::secret::{SYMMETRIC_KEY_LEN, SecretBytes, SymmetricKey};

const SEALED_VERSION: u8 = 1;

/// The key that seals stored settings secrets.
pub struct SettingsSealingKey {
  key: SymmetricKey,
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsSealError {
  #[error("a stored secret is malformed, was tampered with or belongs to another key or setting")]
  BadSecret,
  #[error("encryption failed")]
  Encrypt,
}

impl SettingsSealingKey {
  /// Derives the sealing key from the server's configured settings key.
  pub fn derive(master: &SymmetricKey) -> Self {
    let hkdf = Hkdf::<Sha256>::new(None, master.expose_secret());
    let mut output = Zeroizing::new([0u8; SYMMETRIC_KEY_LEN]);
    // Invariant: 32 bytes is far below HKDF-SHA256's 8160-byte output limit.
    if hkdf
      .expand(SERVER_SETTING_KEY.as_bytes(), output.as_mut())
      .is_err()
    {
      unreachable!("HKDF output length is valid");
    }
    Self {
      key: SymmetricKey::from_bytes(*output),
    }
  }

  /// Seals the secret of the setting `name`. A fresh random nonce is drawn per call.
  pub fn seal(
    &self,
    name: &str,
    plaintext: &[u8],
    rng: &mut (impl CryptoRngCore + ?Sized),
  ) -> Result<Vec<u8>, SettingsSealError> {
    let (nonce, ciphertext) =
      aead::seal(&self.key, &aad(name), plaintext, rng).map_err(|_| SettingsSealError::Encrypt)?;
    let mut sealed = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
    sealed.push(SEALED_VERSION);
    sealed.extend_from_slice(&nonce);
    sealed.extend_from_slice(&ciphertext);
    Ok(sealed)
  }

  /// Opens the secret stored for the setting `name`.
  pub fn open(&self, name: &str, sealed: &[u8]) -> Result<SecretBytes, SettingsSealError> {
    let (&version, rest) = sealed.split_first().ok_or(SettingsSealError::BadSecret)?;
    if version != SEALED_VERSION || rest.len() < NONCE_LEN + TAG_LEN {
      return Err(SettingsSealError::BadSecret);
    }
    let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
    let nonce: &[u8; NONCE_LEN] = nonce.try_into().map_err(|_| SettingsSealError::BadSecret)?;
    aead::open(&self.key, nonce, &aad(name), ciphertext).map_err(|_| SettingsSealError::BadSecret)
  }
}

impl std::fmt::Debug for SettingsSealingKey {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("SettingsSealingKey([redacted])")
  }
}

fn aad(name: &str) -> Vec<u8> {
  domain::labeled(SERVER_SETTING_AAD, name.as_bytes())
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;

  use super::*;

  #[test]
  fn secrets_round_trip_and_do_not_appear_in_the_sealed_bytes() {
    let key = key(1);
    let sealed = key.seal("smtp", b"app-password", &mut OsRng).unwrap();
    assert_eq!(
      key.open("smtp", &sealed).unwrap().expose_secret(),
      b"app-password"
    );
    assert!(!sealed.windows(12).any(|window| window == b"app-password"));
    // A fresh nonce each time.
    assert_ne!(sealed, key.seal("smtp", b"app-password", &mut OsRng).unwrap());
  }

  #[test]
  fn another_setting_another_key_or_tampering_is_refused() {
    let sealed = key(1).seal("smtp", b"secret", &mut OsRng).unwrap();
    assert!(key(1).open("caldav", &sealed).is_err());
    assert!(key(2).open("smtp", &sealed).is_err());
    let mut tampered = sealed.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    assert!(key(1).open("smtp", &tampered).is_err());
    let mut wrong_version = sealed;
    wrong_version[0] = 9;
    assert!(key(1).open("smtp", &wrong_version).is_err());
  }

  #[test]
  fn truncated_and_empty_input_is_refused() {
    let key = key(1);
    assert!(key.open("smtp", &[]).is_err());
    assert!(key.open("smtp", &[SEALED_VERSION]).is_err());
    assert!(
      key
        .open("smtp", &[SEALED_VERSION; 1 + NONCE_LEN + TAG_LEN - 1])
        .is_err()
    );
  }

  #[test]
  fn the_sealing_key_is_not_the_configured_key() {
    let master = SymmetricKey::from_bytes([7; SYMMETRIC_KEY_LEN]);
    let derived = SettingsSealingKey::derive(&master);
    assert_ne!(derived.key.expose_secret(), master.expose_secret());
    assert_eq!(format!("{derived:?}"), "SettingsSealingKey([redacted])");
  }

  fn key(seed: u8) -> SettingsSealingKey {
    SettingsSealingKey::derive(&SymmetricKey::from_bytes([seed; SYMMETRIC_KEY_LEN]))
  }
}
