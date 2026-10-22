//! Record encryption for local stores without built-in encryption (the browser's IndexedDB,
//! e2ee.md §9).
//!
//! From the 256-bit database key, HKDF-SHA256 derives two keys:
//!
//! * a **lookup key**: each record's storage key is `HKDF-Expand(lookup key, labeled(namespace,
//!   name))`, a pseudorandom 32-byte ID, so table names, conversation IDs and message IDs don't
//!   appear in the database;
//! * a **record key**: the record (its plaintext name and value) is sealed with XChaCha20-Poly1305,
//!   with the lookup ID as associated data, so a record can't be moved to another ID unnoticed.
//!
//! Layout of a sealed record: `version (1) || nonce (24) || ciphertext || tag (16)`.

use hkdf::Hkdf;
use rand_core::CryptoRngCore;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::aead::{self, NONCE_LEN, TAG_LEN};
use crate::domain::{self, LOCAL_STORE_LOOKUP, LOCAL_STORE_LOOKUP_KEY, LOCAL_STORE_RECORD};
use crate::secret::{SYMMETRIC_KEY_LEN, SecretBytes, SymmetricKey};

const RECORD_VERSION: u8 = 1;
/// Length of a lookup ID.
pub const LOOKUP_ID_LEN: usize = 32;

/// The keys derived from a local database key.
pub struct LocalStoreKeys {
  lookup: SymmetricKey,
  record: SymmetricKey,
}

/// The storage key of one record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LookupId(pub [u8; LOOKUP_ID_LEN]);

#[derive(Debug, thiserror::Error)]
pub enum LocalStoreError {
  #[error("a stored record is malformed, was tampered with or belongs to another key")]
  BadRecord,
  #[error("encryption failed")]
  Encrypt,
}

impl LocalStoreKeys {
  pub fn derive(database_key: &SymmetricKey) -> Self {
    let hkdf = Hkdf::<Sha256>::new(None, database_key.expose_secret());
    Self {
      lookup: expand(&hkdf, LOCAL_STORE_LOOKUP_KEY.as_bytes()),
      record: expand(&hkdf, LOCAL_STORE_RECORD.as_bytes()),
    }
  }

  /// The pseudorandom ID under which `name` in `namespace` is stored.
  pub fn lookup_id(&self, namespace: &str, name: &[u8]) -> LookupId {
    let mut body = Vec::with_capacity(4 + namespace.len() + name.len());
    // Length-prefixed, so ("ab", "c") and ("a", "bc") differ.
    body.extend_from_slice(&u32::try_from(namespace.len()).unwrap_or(u32::MAX).to_be_bytes());
    body.extend_from_slice(namespace.as_bytes());
    body.extend_from_slice(name);
    // Invariant: the lookup key has the hash length, which is a valid PRK.
    let Ok(hkdf) = Hkdf::<Sha256>::from_prk(self.lookup.expose_secret()) else {
      unreachable!("a 32-byte PRK is valid for HKDF-SHA256");
    };
    let mut id = [0u8; LOOKUP_ID_LEN];
    if hkdf
      .expand(&domain::labeled(LOCAL_STORE_LOOKUP, &body), &mut id)
      .is_err()
    {
      unreachable!("HKDF output length is valid");
    }
    LookupId(id)
  }

  /// Seals a record stored under `id`. A fresh random nonce is drawn per call.
  pub fn seal(
    &self,
    id: &LookupId,
    plaintext: &[u8],
    rng: &mut (impl CryptoRngCore + ?Sized),
  ) -> Result<Vec<u8>, LocalStoreError> {
    let (nonce, ciphertext) =
      aead::seal(&self.record, &aad(id), plaintext, rng).map_err(|_| LocalStoreError::Encrypt)?;
    let mut sealed = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
    sealed.push(RECORD_VERSION);
    sealed.extend_from_slice(&nonce);
    sealed.extend_from_slice(&ciphertext);
    Ok(sealed)
  }

  /// Opens a record read from `id`.
  pub fn open(&self, id: &LookupId, sealed: &[u8]) -> Result<SecretBytes, LocalStoreError> {
    let (&version, rest) = sealed.split_first().ok_or(LocalStoreError::BadRecord)?;
    if version != RECORD_VERSION || rest.len() < NONCE_LEN + TAG_LEN {
      return Err(LocalStoreError::BadRecord);
    }
    let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
    let nonce: &[u8; NONCE_LEN] = nonce.try_into().map_err(|_| LocalStoreError::BadRecord)?;
    aead::open(&self.record, nonce, &aad(id), ciphertext).map_err(|_| LocalStoreError::BadRecord)
  }
}

impl std::fmt::Debug for LocalStoreKeys {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("LocalStoreKeys([redacted])")
  }
}

fn aad(id: &LookupId) -> Vec<u8> {
  domain::labeled(LOCAL_STORE_RECORD, &id.0)
}

fn expand(hkdf: &Hkdf<Sha256>, info: &[u8]) -> SymmetricKey {
  let mut output = Zeroizing::new([0u8; SYMMETRIC_KEY_LEN]);
  // Invariant: 32 bytes is far below HKDF-SHA256's 8160-byte output limit.
  if hkdf.expand(info, output.as_mut()).is_err() {
    unreachable!("HKDF output length is valid");
  }
  SymmetricKey::from_bytes(*output)
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;

  use super::*;

  #[test]
  fn records_round_trip() {
    let keys = keys(1);
    let id = keys.lookup_id("messages", b"conversation 7");
    let sealed = keys.seal(&id, b"hello", &mut OsRng).unwrap();
    assert_eq!(keys.open(&id, &sealed).unwrap().expose_secret(), b"hello");
    assert!(!sealed.windows(5).any(|window| window == b"hello"));
  }

  #[test]
  fn lookup_ids_are_deterministic_per_key_and_separate_names() {
    let keys_a = keys(1);
    assert_eq!(
      keys_a.lookup_id("table", b"key"),
      keys_a.lookup_id("table", b"key")
    );
    assert_ne!(keys_a.lookup_id("ab", b"c"), keys_a.lookup_id("a", b"bc"));
    assert_ne!(
      keys_a.lookup_id("table", b"key"),
      keys(2).lookup_id("table", b"key")
    );
  }

  #[test]
  fn tampered_moved_or_foreign_records_are_rejected() {
    let keys_a = keys(1);
    let id = keys_a.lookup_id("t", b"one");
    let sealed = keys_a.seal(&id, b"value", &mut OsRng).unwrap();

    let mut tampered = sealed.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    assert!(keys_a.open(&id, &tampered).is_err());
    // Moved to another record's ID.
    assert!(keys_a.open(&keys_a.lookup_id("t", b"two"), &sealed).is_err());
    // Another database key.
    assert!(keys(2).open(&id, &sealed).is_err());
    // Truncated, empty and unknown versions.
    assert!(keys_a.open(&id, &sealed[..10]).is_err());
    assert!(keys_a.open(&id, &[]).is_err());
    let mut future = sealed;
    future[0] = 2;
    assert!(keys_a.open(&id, &future).is_err());
  }

  fn keys(seed: u8) -> LocalStoreKeys {
    LocalStoreKeys::derive(&SymmetricKey::from_bytes([seed; SYMMETRIC_KEY_LEN]))
  }
}
