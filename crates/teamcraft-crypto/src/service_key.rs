//! Hashes of service API keys that owners sign into the workspace policy: the notetaker's
//! OpenRouter key (e2ee.md §8.2). The owner's client hashes the key the admin typed, and the
//! notetaker uses a key it gets from the server only if its hash is the signed one, so a server
//! can't swap in a key of an account of its own.
//!
//! A plain SHA-256 is enough: API keys are long random strings that can't be guessed from their
//! hash.

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// SHA-256 of `key` as 64 lower-case hex digits (the policy's `api_key_sha256`). Surrounding
/// whitespace, which a pasted key often carries, is not part of the key.
pub fn api_key_sha256_hex(key: &str) -> String {
  let digest: [u8; 32] = Sha256::digest(key.trim().as_bytes()).into();
  digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Whether `key` hashes to `expected_hex`, compared in constant time.
pub fn is_api_key_sha256(key: &str, expected_hex: &str) -> bool {
  let actual = api_key_sha256_hex(key);
  actual.len() == expected_hex.len() && bool::from(actual.as_bytes().ct_eq(expected_hex.as_bytes()))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_hash_is_sha256_in_lower_case_hex() {
    // SHA-256("test"), a known answer.
    assert_eq!(
      api_key_sha256_hex("test"),
      "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
    );
    assert_eq!(api_key_sha256_hex(" test\n"), api_key_sha256_hex("test"));
  }

  #[test]
  fn only_the_signed_key_matches() {
    let signed = api_key_sha256_hex("sk-or-v1-right");
    assert!(is_api_key_sha256("sk-or-v1-right", &signed));
    assert!(!is_api_key_sha256("sk-or-v1-wrong", &signed));
    assert!(!is_api_key_sha256("sk-or-v1-right", &signed.to_uppercase()));
    assert!(!is_api_key_sha256("sk-or-v1-right", ""));
  }
}
