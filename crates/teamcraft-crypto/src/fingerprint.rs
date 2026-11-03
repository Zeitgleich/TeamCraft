//! Fingerprints that tell whether two contents are the same: SHA-256 over what the caller feeds in,
//! piece by piece. The integration bridge compares them to tell a producer's retry from a new
//! request under a reused idempotency key (docs/bridge.md, "Retries"), for files of any size
//! without keeping them.
//!
//! Not a MAC and not a commitment: equal fingerprints only let a party skip work it already did. The
//! caller frames what it feeds in (length prefixes) so that different contents never feed the same
//! bytes.

use std::fmt;

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// SHA-256 of a content; compared in constant time, and kept out of `Debug`.
#[derive(Clone, Copy, Eq)]
pub struct Fingerprint([u8; 32]);

/// Builds a [`Fingerprint`] from pieces fed in order.
#[derive(Clone, Default)]
pub struct FingerprintHasher(Sha256);

impl Fingerprint {
  /// The fingerprint of `bytes` in one piece.
  pub fn of(bytes: &[u8]) -> Self {
    Self(Sha256::digest(bytes).into())
  }

  pub fn as_bytes(&self) -> &[u8; 32] {
    &self.0
  }
}

impl FingerprintHasher {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn update(&mut self, bytes: &[u8]) {
    self.0.update(bytes);
  }

  pub fn finish(self) -> Fingerprint {
    Fingerprint(self.0.finalize().into())
  }
}

impl PartialEq for Fingerprint {
  fn eq(&self, other: &Self) -> bool {
    self.0.ct_eq(&other.0).into()
  }
}

impl fmt::Debug for Fingerprint {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    // It confirms a guessed content, so it stays out of logs like the content itself.
    f.write_str("Fingerprint([redacted])")
  }
}

impl fmt::Debug for FingerprintHasher {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("FingerprintHasher")
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn pieces_hash_like_the_whole_and_differences_show() {
    let mut hasher = FingerprintHasher::new();
    hasher.update(b"a");
    hasher.update(b"bc");
    assert_eq!(hasher.finish(), Fingerprint::of(b"abc"));
    assert_ne!(Fingerprint::of(b"abc"), Fingerprint::of(b"abd"));
    // Known answer: SHA-256("abc").
    assert_eq!(Fingerprint::of(b"abc").as_bytes()[..4], [0xba, 0x78, 0x16, 0xbf]);
    assert_eq!(
      format!("{:?}", Fingerprint::of(b"abc")),
      "Fingerprint([redacted])"
    );
  }
}
