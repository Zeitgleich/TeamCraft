//! Tokens of integration-bridge sources (docs/bridge.md, "Sources"). The token is the secret part
//! of a webhook or log URL. The app generates it, shows it once, and only its SHA-256 travels in
//! the channel's control message; the bridge hashes the token of each request and looks the hash
//! up in constant time.
//!
//! A plain hash (no salt, no slow KDF) is enough: the token carries 256 random bits, so the hash
//! can't be reversed by guessing.

use std::fmt;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand_core::CryptoRngCore;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Prefix of generated tokens, so secret scanners and people recognize a leaked one.
pub const SOURCE_TOKEN_PREFIX: &str = "tcs_";
const TOKEN_RANDOM_BYTES: usize = 32;

/// A source token: `tcs_` and 43 URL-safe characters. Zeroized on drop, redacted in `Debug`.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SourceToken(String);

impl SourceToken {
  /// A fresh token with 256 random bits.
  pub fn generate(rng: &mut impl CryptoRngCore) -> Self {
    let mut bytes = [0u8; TOKEN_RANDOM_BYTES];
    rng.fill_bytes(&mut bytes);
    Self::from_random(bytes)
  }

  /// A fresh token from the operating system's (or browser's) random source; `None` if it fails,
  /// never a weak token.
  pub fn random() -> Option<Self> {
    let mut bytes = [0u8; TOKEN_RANDOM_BYTES];
    rand_core::RngCore::try_fill_bytes(&mut rand_core::OsRng, &mut bytes).ok()?;
    Some(Self::from_random(bytes))
  }

  fn from_random(mut bytes: [u8; TOKEN_RANDOM_BYTES]) -> Self {
    let token = format!("{SOURCE_TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes));
    bytes.zeroize();
    Self(token)
  }

  /// The token for showing it once (and putting it into a URL).
  pub fn expose_secret(&self) -> &str {
    &self.0
  }

  /// What the control message carries instead of the token.
  pub fn sha256(&self) -> [u8; 32] {
    source_token_sha256(&self.0)
  }
}

impl fmt::Debug for SourceToken {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("SourceToken([redacted])")
  }
}

/// SHA-256 of a token as it appears in a request path.
pub fn source_token_sha256(token: &str) -> [u8; 32] {
  Sha256::digest(token.as_bytes()).into()
}

/// Which of `known` hashes equals the hash of `token`. Every entry is compared, in constant time
/// each, so the answer's timing tells nothing about which entry (or how much of one) matched.
pub fn find_source_token<'a, T>(
  token: &str,
  known: impl IntoIterator<Item = (&'a [u8; 32], T)>,
) -> Option<T> {
  let hash = source_token_sha256(token);
  let mut found = None;
  for (candidate, value) in known {
    if bool::from(candidate.ct_eq(&hash)) {
      found = Some(value);
    }
  }
  found
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;

  use super::*;

  #[test]
  fn generated_tokens_are_long_url_safe_and_distinct() {
    let one = SourceToken::generate(&mut OsRng);
    let two = SourceToken::generate(&mut OsRng);
    assert_ne!(one.expose_secret(), two.expose_secret());
    assert_ne!(
      SourceToken::random().unwrap().expose_secret(),
      one.expose_secret()
    );
    let text = one.expose_secret();
    assert!(text.starts_with(SOURCE_TOKEN_PREFIX));
    assert_eq!(text.len(), SOURCE_TOKEN_PREFIX.len() + 43);
    assert!(
      text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    );
    assert_eq!(format!("{one:?}"), "SourceToken([redacted])");
  }

  #[test]
  fn the_hash_is_sha256_of_the_token_text() {
    // Known answer: SHA-256("abc").
    assert_eq!(source_token_sha256("abc")[..4], [0xba, 0x78, 0x16, 0xbf]);
    let token = SourceToken::generate(&mut OsRng);
    assert_eq!(token.sha256(), source_token_sha256(token.expose_secret()));
  }

  #[test]
  fn lookup_finds_only_the_matching_hash() {
    let (a, b) = (source_token_sha256("token-a"), source_token_sha256("token-b"));
    let known = [(&a, 1), (&b, 2)];
    assert_eq!(find_source_token("token-b", known), Some(2));
    assert_eq!(find_source_token("token-c", known), None);
    assert_eq!(find_source_token("", known), None);
    assert_eq!(find_source_token::<u8>("token-a", []), None);
  }
}
