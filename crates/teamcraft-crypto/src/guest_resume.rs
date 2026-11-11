//! A meeting guest's session sealed in the browser, so a reload brings the guest back (meetings.md,
//! "Guests in the browser").
//!
//! The page seals what it needs to carry on (the server's session token, the meeting, the guest's
//! device) under a fresh random key and keeps only the ciphertext in the browser's storage. The key
//! lives in the page's address fragment, which never reaches the server: the stored copy is useless
//! without the address, and the address is useless in another browser.
//!
//! Layout: `version (1) || nonce (24) || ciphertext || tag (16)`, XChaCha20-Poly1305 with a fresh
//! random nonce per seal and the version bound in as associated data.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand_core::CryptoRngCore;

use crate::aead::{self, NONCE_LEN, TAG_LEN};
use crate::domain::{self, GUEST_RESUME};
use crate::secret::{SYMMETRIC_KEY_LEN, SecretBytes, SymmetricKey};

const SEALED_VERSION: u8 = 1;

/// The key of one guest session's sealed copy; only in the page's address fragment.
pub struct GuestResumeKey(SymmetricKey);

#[derive(Debug, thiserror::Error)]
pub enum GuestResumeError {
  #[error("the guest session key in the address is malformed")]
  BadKey,
  #[error("the stored guest session is malformed, was tampered with or belongs to another key")]
  BadSealed,
  #[error("encryption failed")]
  Encrypt,
}

impl GuestResumeKey {
  pub fn generate(rng: &mut impl CryptoRngCore) -> Self {
    Self(SymmetricKey::generate(rng))
  }

  /// Reads the key as [`Self::to_fragment`] wrote it.
  pub fn from_fragment(text: &str) -> Result<Self, GuestResumeError> {
    let bytes = URL_SAFE_NO_PAD
      .decode(text)
      .map_err(|_| GuestResumeError::BadKey)?;
    if bytes.len() != SYMMETRIC_KEY_LEN {
      return Err(GuestResumeError::BadKey);
    }
    SymmetricKey::from_slice(&bytes)
      .map(Self)
      .ok_or(GuestResumeError::BadKey)
  }

  /// The key as it goes into the address fragment (base64url, no padding).
  pub fn to_fragment(&self) -> String {
    URL_SAFE_NO_PAD.encode(self.0.expose_secret())
  }

  /// Seals the session. A fresh random nonce is drawn per call.
  pub fn seal(
    &self,
    plaintext: &[u8],
    rng: &mut (impl CryptoRngCore + ?Sized),
  ) -> Result<Vec<u8>, GuestResumeError> {
    let (nonce, ciphertext) =
      aead::seal(&self.0, &aad(), plaintext, rng).map_err(|_| GuestResumeError::Encrypt)?;
    let mut sealed = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
    sealed.push(SEALED_VERSION);
    sealed.extend_from_slice(&nonce);
    sealed.extend_from_slice(&ciphertext);
    Ok(sealed)
  }

  /// Opens what [`Self::seal`] sealed with this key.
  pub fn open(&self, sealed: &[u8]) -> Result<SecretBytes, GuestResumeError> {
    let (&version, rest) = sealed.split_first().ok_or(GuestResumeError::BadSealed)?;
    if version != SEALED_VERSION || rest.len() < NONCE_LEN + TAG_LEN {
      return Err(GuestResumeError::BadSealed);
    }
    let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
    let nonce: &[u8; NONCE_LEN] = nonce.try_into().map_err(|_| GuestResumeError::BadSealed)?;
    aead::open(&self.0, nonce, &aad(), ciphertext).map_err(|_| GuestResumeError::BadSealed)
  }
}

impl fmt::Debug for GuestResumeKey {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("GuestResumeKey([redacted])")
  }
}

fn aad() -> Vec<u8> {
  domain::labeled(GUEST_RESUME, &[SEALED_VERSION])
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;

  use super::*;

  const SESSION: &[u8] = br#"{"guest_token":"t"}"#;

  #[test]
  fn sealed_session_round_trips_through_the_fragment() {
    let key = GuestResumeKey::generate(&mut OsRng);
    let sealed = key.seal(SESSION, &mut OsRng).unwrap();
    let again = GuestResumeKey::from_fragment(&key.to_fragment()).unwrap();
    assert_eq!(again.open(&sealed).unwrap().expose_secret(), SESSION);
  }

  #[test]
  fn fragment_is_url_safe_and_holds_the_whole_key() {
    let fragment = GuestResumeKey::generate(&mut OsRng).to_fragment();
    assert_eq!(fragment.len(), 43);
    assert!(
      fragment
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    );
  }

  mod rejected {
    use super::*;

    #[test]
    fn another_key() {
      let sealed = GuestResumeKey::generate(&mut OsRng)
        .seal(SESSION, &mut OsRng)
        .unwrap();
      let other = GuestResumeKey::generate(&mut OsRng);
      assert!(matches!(other.open(&sealed), Err(GuestResumeError::BadSealed)));
    }

    #[test]
    fn tampered_truncated_and_empty() {
      let key = GuestResumeKey::generate(&mut OsRng);
      let mut sealed = key.seal(SESSION, &mut OsRng).unwrap();
      let last = sealed.len() - 1;
      sealed[last] ^= 1;
      assert!(key.open(&sealed).is_err());
      assert!(key.open(&sealed[..NONCE_LEN]).is_err());
      assert!(key.open(&[]).is_err());
      let mut other_version = key.seal(SESSION, &mut OsRng).unwrap();
      other_version[0] = 2;
      assert!(key.open(&other_version).is_err());
    }

    #[test]
    fn malformed_fragments() {
      assert!(GuestResumeKey::from_fragment("").is_err());
      assert!(GuestResumeKey::from_fragment("not base64!").is_err());
      assert!(GuestResumeKey::from_fragment(&URL_SAFE_NO_PAD.encode([1u8; 16])).is_err());
      assert!(GuestResumeKey::from_fragment(&URL_SAFE_NO_PAD.encode([1u8; 40])).is_err());
    }

    #[test]
    fn debug_does_not_show_the_key() {
      let key = GuestResumeKey::generate(&mut OsRng);
      assert!(!format!("{key:?}").contains(&key.to_fragment()));
    }
  }
}
