//! Digests that say whether a server answer changed (issue #714: a client coming back compares
//! them instead of reading every member list again). Not a security mechanism: the client trusts
//! nothing in the answer because of it, it only skips a read whose answer would be the same.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

/// Bytes of SHA-256 kept: 128 bits make an accidental match of two different answers negligible.
const DIGEST_BYTES: usize = 16;

/// A short, stable digest of `bytes` (URL-safe base64 of truncated SHA-256).
pub fn change_digest(bytes: &[u8]) -> String {
  let hash = Sha256::digest(bytes);
  URL_SAFE_NO_PAD.encode(&hash[..DIGEST_BYTES])
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn equal_answers_have_equal_digests_and_different_ones_differ() {
    assert_eq!(change_digest(b"members"), change_digest(b"members"));
    assert_ne!(change_digest(b"members"), change_digest(b"members "));
    assert_eq!(change_digest(b"").len(), 22);
  }
}
