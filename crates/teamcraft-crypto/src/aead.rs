//! XChaCha20-Poly1305 with random 192-bit nonces, shared by messages, the key bag and key wrapping.
//!
//! Random nonces are safe at this size (no practical collision risk), so no nonce state has to be
//! kept per key – important because many devices encrypt under the same content key.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand_core::CryptoRngCore;

use crate::secret::{SecretBytes, SymmetricKey};

pub(crate) const NONCE_LEN: usize = 24;
pub(crate) const TAG_LEN: usize = 16;

/// Encryption or decryption failed. Deliberately carries no detail (wrong key, tampered data and
/// wrong associated data are indistinguishable).
#[derive(Debug)]
pub(crate) struct AeadFailure;

pub(crate) fn seal(
  key: &SymmetricKey,
  aad: &[u8],
  plaintext: &[u8],
  rng: &mut (impl CryptoRngCore + ?Sized),
) -> Result<([u8; NONCE_LEN], Vec<u8>), AeadFailure> {
  let mut nonce = [0u8; NONCE_LEN];
  rng.fill_bytes(&mut nonce);
  let cipher = XChaCha20Poly1305::new(key.expose_secret().into());
  let ciphertext = cipher
    .encrypt(XNonce::from_slice(&nonce), Payload { msg: plaintext, aad })
    .map_err(|_| AeadFailure)?;
  Ok((nonce, ciphertext))
}

pub(crate) fn open(
  key: &SymmetricKey,
  nonce: &[u8; NONCE_LEN],
  aad: &[u8],
  ciphertext: &[u8],
) -> Result<SecretBytes, AeadFailure> {
  let cipher = XChaCha20Poly1305::new(key.expose_secret().into());
  cipher
    .decrypt(XNonce::from_slice(nonce), Payload { msg: ciphertext, aad })
    .map(SecretBytes::new)
    .map_err(|_| AeadFailure)
}
