//! Wrapper types for secret material (`.claude/rules/security.md`): zeroized on drop, redacted in
//! `Debug`, no `Display`, no `Clone` unless a caller really needs a copy.

use std::fmt;

use rand_core::CryptoRngCore;
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

/// Length of every symmetric key in this crate (256 bit).
pub const SYMMETRIC_KEY_LEN: usize = 32;

/// A 256-bit symmetric key: content keys, attachment keys, backup keys, key-encryption keys.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SymmetricKey([u8; SYMMETRIC_KEY_LEN]);

/// Secret bytes of variable length: decrypted content, passphrases, serialized secret structures.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SecretBytes(Vec<u8>);

impl SymmetricKey {
  /// Generates a fresh random key.
  pub fn generate(rng: &mut impl CryptoRngCore) -> Self {
    let mut bytes = [0u8; SYMMETRIC_KEY_LEN];
    rng.fill_bytes(&mut bytes);
    Self(bytes)
  }

  /// Takes ownership of raw key bytes, e.g. from the OS keystore or a decrypted key bag.
  pub fn from_bytes(bytes: [u8; SYMMETRIC_KEY_LEN]) -> Self {
    Self(bytes)
  }

  /// Copies a key out of a slice. Returns `None` if the slice has the wrong length.
  pub fn from_slice(bytes: &[u8]) -> Option<Self> {
    let array = <[u8; SYMMETRIC_KEY_LEN]>::try_from(bytes).ok()?;
    Some(Self(array))
  }

  /// Raw key bytes, for handing to an AEAD or KDF. Never log or persist them unwrapped.
  pub fn expose_secret(&self) -> &[u8; SYMMETRIC_KEY_LEN] {
    &self.0
  }

  /// Explicit copy. Not `Clone`, so copies of keys are visible in the code.
  pub fn duplicate(&self) -> Self {
    Self(self.0)
  }

  /// Equality in constant time, for crates without `subtle`.
  pub fn is_same_key(&self, other: &Self) -> bool {
    bool::from(self.ct_eq(other))
  }
}

impl ConstantTimeEq for SymmetricKey {
  fn ct_eq(&self, other: &Self) -> subtle::Choice {
    self.0.ct_eq(&other.0)
  }
}

impl fmt::Debug for SymmetricKey {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("SymmetricKey([redacted])")
  }
}

impl SecretBytes {
  pub fn new(bytes: Vec<u8>) -> Self {
    Self(bytes)
  }

  pub fn expose_secret(&self) -> &[u8] {
    &self.0
  }

  pub fn len(&self) -> usize {
    self.0.len()
  }

  pub fn is_empty(&self) -> bool {
    self.0.is_empty()
  }
}

impl From<Vec<u8>> for SecretBytes {
  fn from(bytes: Vec<u8>) -> Self {
    Self(bytes)
  }
}

impl From<Zeroizing<Vec<u8>>> for SecretBytes {
  fn from(bytes: Zeroizing<Vec<u8>>) -> Self {
    // Moves the buffer out; the emptied `Zeroizing` wrapper has nothing left to clear.
    let mut bytes = bytes;
    Self(std::mem::take(&mut *bytes))
  }
}

impl fmt::Debug for SecretBytes {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "SecretBytes([redacted; {} bytes])", self.0.len())
  }
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;

  use super::*;

  #[test]
  fn debug_output_is_redacted() {
    let key = SymmetricKey::from_bytes([0xAB; SYMMETRIC_KEY_LEN]);
    let secret = SecretBytes::new(b"top secret".to_vec());
    assert_eq!(format!("{key:?}"), "SymmetricKey([redacted])");
    assert_eq!(format!("{secret:?}"), "SecretBytes([redacted; 10 bytes])");
    assert!(!format!("{key:?}").contains("ab"));
  }

  #[test]
  fn generated_keys_differ_and_compare_in_constant_time() {
    let first = SymmetricKey::generate(&mut OsRng);
    let second = SymmetricKey::generate(&mut OsRng);
    assert!(bool::from(first.ct_eq(&first.duplicate())));
    assert!(!bool::from(first.ct_eq(&second)));
  }

  #[test]
  fn from_slice_rejects_wrong_length() {
    assert!(SymmetricKey::from_slice(&[0u8; 31]).is_none());
    assert!(SymmetricKey::from_slice(&[0u8; 33]).is_none());
    assert!(SymmetricKey::from_slice(&[0u8; 32]).is_some());
  }

  #[test]
  fn zeroize_clears_bytes() {
    let mut key = SymmetricKey::from_bytes([0xAB; SYMMETRIC_KEY_LEN]);
    key.zeroize();
    assert_eq!(key.expose_secret(), &[0u8; SYMMETRIC_KEY_LEN]);
  }
}
