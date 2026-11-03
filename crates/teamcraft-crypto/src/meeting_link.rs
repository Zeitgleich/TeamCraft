//! The token of a meeting link (e2ee.md §4.6, "Meeting guests"; docs/meetings.md, "Links").
//!
//! A meeting link is `<base>/meeting/<token>#v=<organizer's key fingerprint>`. The token replaces
//! the meeting's snowflake ID as its public handle: an ID is short and guessable, the token is
//! 192 random bits. It is no secret from the server, which stores it and resolves it to the
//! meeting, and it carries no key: it only lets its holder knock at the lobby.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand_core::CryptoRngCore;

/// Random bytes in a token: 192 bits, well above the 128 bits a guess must beat.
const TOKEN_RANDOM_BYTES: usize = 24;
/// Characters of a token: 24 bytes in unpadded base64url.
pub const MEETING_LINK_TOKEN_LEN: usize = 32;

/// A meeting link token. Not secret from the server, so `Debug` shows it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MeetingLinkToken(String);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MeetingLinkError {
  #[error("not a meeting link token")]
  Malformed,
}

impl MeetingLinkToken {
  /// A fresh token from `rng`.
  pub fn generate(rng: &mut impl CryptoRngCore) -> Self {
    let mut bytes = [0u8; TOKEN_RANDOM_BYTES];
    rng.fill_bytes(&mut bytes);
    Self(URL_SAFE_NO_PAD.encode(bytes))
  }

  /// A token from a link's path, if it has the shape of one. Says nothing about whether a
  /// meeting has it.
  pub fn parse(text: &str) -> Result<Self, MeetingLinkError> {
    if is_meeting_link_token(text) {
      Ok(Self(text.to_owned()))
    } else {
      Err(MeetingLinkError::Malformed)
    }
  }

  pub fn as_str(&self) -> &str {
    &self.0
  }
}

/// Whether `text` has the shape of a meeting link token: 32 base64url characters that decode to
/// 24 bytes. A snowflake ID (at most 19 digits) never has it.
pub fn is_meeting_link_token(text: &str) -> bool {
  text.len() == MEETING_LINK_TOKEN_LEN
    && URL_SAFE_NO_PAD
      .decode(text)
      .is_ok_and(|bytes| bytes.len() == TOKEN_RANDOM_BYTES)
}

#[cfg(test)]
mod tests {
  use std::collections::HashSet;

  use rand_core::OsRng;

  use super::*;

  #[test]
  fn a_token_has_192_random_bits_in_base64url() {
    let token = MeetingLinkToken::generate(&mut OsRng);
    let text = token.as_str();
    assert_eq!(text.len(), MEETING_LINK_TOKEN_LEN);
    assert!(
      text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    );
    let bytes = URL_SAFE_NO_PAD.decode(text).unwrap();
    assert_eq!(bytes.len() * 8, 192);
    assert_eq!(MeetingLinkToken::parse(text), Ok(token));
  }

  #[test]
  fn tokens_differ_and_spread_over_every_bit() {
    let tokens: Vec<Vec<u8>> = (0..256)
      .map(|_| {
        URL_SAFE_NO_PAD
          .decode(MeetingLinkToken::generate(&mut OsRng).as_str())
          .unwrap()
      })
      .collect();
    assert_eq!(tokens.iter().collect::<HashSet<_>>().len(), tokens.len());
    // Every bit position is set in some token and clear in another: no fixed bits.
    for bit in 0..TOKEN_RANDOM_BYTES * 8 {
      let ones = tokens
        .iter()
        .filter(|bytes| bytes[bit / 8] & (1 << (bit % 8)) != 0)
        .count();
      assert!(ones > 0 && ones < tokens.len(), "bit {bit} never changes");
    }
  }

  #[test]
  fn ids_and_damaged_tokens_are_not_tokens() {
    let token = MeetingLinkToken::generate(&mut OsRng);
    let text = token.as_str();
    for bad in [
      "",
      "7340032000123456789",
      &text[..31],
      &format!("{text}A"),
      &format!("{}+", &text[..31]),
      &format!("{}/", &text[..31]),
      &format!("{}=", &text[..31]),
      &format!("{}.", &text[..31]),
    ] {
      assert!(!is_meeting_link_token(bad), "{bad:?}");
      assert_eq!(MeetingLinkToken::parse(bad), Err(MeetingLinkError::Malformed));
    }
  }
}
