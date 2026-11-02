//! The sealed file of a desktop's local secrets (e2ee.md §9): tokens, sessions and copies of the
//! database and backup keys, under one key that the OS keychain holds.
//!
//! Layout: `version (1) || nonce (24) || ciphertext || tag (16)`, XChaCha20-Poly1305 with a fresh
//! random nonce per write and the version bound in as associated data.

use rand_core::CryptoRngCore;

use crate::aead::{self, NONCE_LEN, TAG_LEN};
use crate::domain::{self, LOCAL_SECRETS_FILE};
use crate::secret::{SecretBytes, SymmetricKey};

const FILE_VERSION: u8 = 1;

#[derive(Debug, thiserror::Error)]
pub enum SecretFileError {
  #[error("the secrets file is malformed, was tampered with or belongs to another key")]
  BadFile,
  #[error("encryption failed")]
  Encrypt,
}

/// Seals the file's content. A fresh random nonce is drawn per call.
pub fn seal(
  key: &SymmetricKey,
  plaintext: &[u8],
  rng: &mut (impl CryptoRngCore + ?Sized),
) -> Result<Vec<u8>, SecretFileError> {
  let (nonce, ciphertext) = aead::seal(key, &aad(), plaintext, rng).map_err(|_| SecretFileError::Encrypt)?;
  let mut sealed = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
  sealed.push(FILE_VERSION);
  sealed.extend_from_slice(&nonce);
  sealed.extend_from_slice(&ciphertext);
  Ok(sealed)
}

/// Opens a file written by [`seal`].
pub fn open(key: &SymmetricKey, sealed: &[u8]) -> Result<SecretBytes, SecretFileError> {
  let (&version, rest) = sealed.split_first().ok_or(SecretFileError::BadFile)?;
  if version != FILE_VERSION || rest.len() < NONCE_LEN + TAG_LEN {
    return Err(SecretFileError::BadFile);
  }
  let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
  let nonce: &[u8; NONCE_LEN] = nonce.try_into().map_err(|_| SecretFileError::BadFile)?;
  aead::open(key, nonce, &aad(), ciphertext).map_err(|_| SecretFileError::BadFile)
}

fn aad() -> Vec<u8> {
  domain::labeled(LOCAL_SECRETS_FILE, &[FILE_VERSION])
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;

  use super::*;
  use crate::secret::SYMMETRIC_KEY_LEN;

  const KEY: [u8; SYMMETRIC_KEY_LEN] = [7; SYMMETRIC_KEY_LEN];

  #[test]
  fn round_trips() {
    let key = SymmetricKey::from_bytes(KEY);
    let sealed = seal(&key, b"secrets", &mut OsRng).unwrap();
    assert_eq!(open(&key, &sealed).unwrap().expose_secret(), b"secrets");
  }

  #[test]
  fn every_write_draws_a_new_nonce() {
    let key = SymmetricKey::from_bytes(KEY);
    assert_ne!(
      seal(&key, b"secrets", &mut OsRng).unwrap(),
      seal(&key, b"secrets", &mut OsRng).unwrap()
    );
  }

  #[test]
  fn another_key_does_not_open_it() {
    let sealed = seal(&SymmetricKey::from_bytes(KEY), b"secrets", &mut OsRng).unwrap();
    let other = SymmetricKey::from_bytes([8; SYMMETRIC_KEY_LEN]);
    assert!(matches!(open(&other, &sealed), Err(SecretFileError::BadFile)));
  }

  #[test]
  fn a_changed_byte_anywhere_is_refused() {
    let key = SymmetricKey::from_bytes(KEY);
    let sealed = seal(&key, b"secrets", &mut OsRng).unwrap();
    for index in 0..sealed.len() {
      let mut damaged = sealed.clone();
      damaged[index] ^= 1;
      assert!(open(&key, &damaged).is_err(), "byte {index}");
    }
  }

  #[test]
  fn empty_and_truncated_files_are_refused() {
    let key = SymmetricKey::from_bytes(KEY);
    let sealed = seal(&key, b"secrets", &mut OsRng).unwrap();
    assert!(open(&key, &[]).is_err());
    for length in 1..sealed.len() {
      assert!(open(&key, &sealed[..length]).is_err(), "length {length}");
    }
  }
}
