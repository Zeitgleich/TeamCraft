//! Ballots of anonymous polls (issue #2121, e2ee.md §4.12.1).
//!
//! An anonymous poll carries a random [`BallotKey`] inside its encrypted message, so every member
//! (and nobody else) can seal and open its ballots. A ballot goes to the server's ballot box
//! instead of through MLS, so members get it without its sender; the server can't read it.
//!
//! Layout: `version (1) || nonce (24) || ciphertext || tag (16)`, XChaCha20-Poly1305 with a fresh
//! random nonce per ballot. The plaintext is the chosen options as a 4-byte little-endian bitmap,
//! so every ballot of a poll has the same length whatever it chooses. The associated data binds
//! the version, the conversation and the poll's client message ID: a ballot copied into another
//! poll's box doesn't open there.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand_core::CryptoRngCore;
use zeroize::Zeroizing;

use crate::aead::{self, NONCE_LEN, TAG_LEN};
use crate::domain::{self, POLL_BALLOT};
use crate::secret::{SYMMETRIC_KEY_LEN, SymmetricKey};

const BALLOT_VERSION: u8 = 1;
/// Length of the plaintext: the option bitmap.
const CHOICES_LEN: usize = 4;
/// Length of every ballot of this version.
pub const BALLOT_LEN: usize = 1 + NONCE_LEN + CHOICES_LEN + TAG_LEN;
/// Most options a ballot can choose from: one bit each.
pub const MAX_BALLOT_OPTIONS: usize = CHOICES_LEN * 8;

/// The key of one anonymous poll's ballots. It travels inside the poll's encrypted message only.
pub struct BallotKey(SymmetricKey);

/// The poll a ballot belongs to: its conversation and its client message ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BallotScope {
  pub conversation_id: i64,
  pub poll_message_id: [u8; 16],
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PollBallotError {
  #[error("the ballot key is malformed")]
  BadKey,
  #[error("the ballot is malformed, was tampered with, or belongs to another poll")]
  BadBallot,
  #[error("a ballot can't choose option {0}")]
  OptionOutOfRange(u32),
  #[error("encryption failed")]
  Encrypt,
}

impl BallotKey {
  pub fn generate(rng: &mut impl CryptoRngCore) -> Self {
    Self(SymmetricKey::generate(rng))
  }

  /// Reads the key as [`Self::to_text`] wrote it.
  pub fn from_text(text: &str) -> Result<Self, PollBallotError> {
    let bytes = Zeroizing::new(
      URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|_| PollBallotError::BadKey)?,
    );
    if bytes.len() != SYMMETRIC_KEY_LEN {
      return Err(PollBallotError::BadKey);
    }
    SymmetricKey::from_slice(&bytes)
      .map(Self)
      .ok_or(PollBallotError::BadKey)
  }

  /// The key as it goes into the poll message (base64url, no padding).
  pub fn to_text(&self) -> Zeroizing<String> {
    Zeroizing::new(URL_SAFE_NO_PAD.encode(self.0.expose_secret()))
  }

  /// Seals a ballot choosing the options at `choices` (positions below
  /// [`MAX_BALLOT_OPTIONS`]; repeats collapse). Whether the poll takes such a vote is the caller's
  /// check. A fresh random nonce is drawn per ballot, so two ballots never share their bytes.
  pub fn seal(
    &self,
    scope: BallotScope,
    choices: &[u32],
    rng: &mut (impl CryptoRngCore + ?Sized),
  ) -> Result<Vec<u8>, PollBallotError> {
    let mut bitmap = 0u32;
    for &choice in choices {
      let bit = 1u32
        .checked_shl(choice)
        .ok_or(PollBallotError::OptionOutOfRange(choice))?;
      bitmap |= bit;
    }
    let (nonce, ciphertext) =
      aead::seal(&self.0, &aad(scope), &bitmap.to_le_bytes(), rng).map_err(|_| PollBallotError::Encrypt)?;
    let mut ballot = Vec::with_capacity(BALLOT_LEN);
    ballot.push(BALLOT_VERSION);
    ballot.extend_from_slice(&nonce);
    ballot.extend_from_slice(&ciphertext);
    Ok(ballot)
  }

  /// Opens a ballot of the poll `scope`: the chosen option positions, ascending. Anything not
  /// sealed with this key for this poll fails.
  pub fn open(&self, scope: BallotScope, ballot: &[u8]) -> Result<Vec<u32>, PollBallotError> {
    if ballot.len() != BALLOT_LEN {
      return Err(PollBallotError::BadBallot);
    }
    let (&version, rest) = ballot.split_first().ok_or(PollBallotError::BadBallot)?;
    if version != BALLOT_VERSION {
      return Err(PollBallotError::BadBallot);
    }
    let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
    let nonce: &[u8; NONCE_LEN] = nonce.try_into().map_err(|_| PollBallotError::BadBallot)?;
    let plaintext =
      aead::open(&self.0, nonce, &aad(scope), ciphertext).map_err(|_| PollBallotError::BadBallot)?;
    let bitmap: [u8; CHOICES_LEN] = plaintext
      .expose_secret()
      .try_into()
      .map_err(|_| PollBallotError::BadBallot)?;
    let bitmap = u32::from_le_bytes(bitmap);
    Ok((0..32u32).filter(|bit| bitmap & (1 << bit) != 0).collect())
  }
}

impl fmt::Debug for BallotKey {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("BallotKey([redacted])")
  }
}

fn aad(scope: BallotScope) -> Vec<u8> {
  let mut body = Vec::with_capacity(1 + 8 + 16);
  body.push(BALLOT_VERSION);
  body.extend_from_slice(&scope.conversation_id.to_be_bytes());
  body.extend_from_slice(&scope.poll_message_id);
  domain::labeled(POLL_BALLOT, &body)
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;

  use super::*;

  const SCOPE: BallotScope = BallotScope {
    conversation_id: 7,
    poll_message_id: [3; 16],
  };

  #[test]
  fn a_ballot_round_trips_through_the_key_text() {
    let key = BallotKey::generate(&mut OsRng);
    let ballot = key.seal(SCOPE, &[2, 0, 31], &mut OsRng).unwrap();
    let again = BallotKey::from_text(&key.to_text()).unwrap();
    assert_eq!(again.open(SCOPE, &ballot).unwrap(), vec![0, 2, 31]);
  }

  #[test]
  fn every_ballot_has_the_same_length_and_fresh_bytes() {
    let key = BallotKey::generate(&mut OsRng);
    let none = key.seal(SCOPE, &[], &mut OsRng).unwrap();
    let many = key.seal(SCOPE, &(0..32).collect::<Vec<_>>(), &mut OsRng).unwrap();
    let same = key.seal(SCOPE, &[], &mut OsRng).unwrap();
    assert_eq!(none.len(), BALLOT_LEN);
    assert_eq!(many.len(), BALLOT_LEN);
    assert_ne!(none, same);
    assert_eq!(key.open(SCOPE, &none).unwrap(), Vec::<u32>::new());
  }

  #[test]
  fn an_option_past_the_bitmap_is_refused() {
    let key = BallotKey::generate(&mut OsRng);
    assert_eq!(
      key.seal(SCOPE, &[32], &mut OsRng),
      Err(PollBallotError::OptionOutOfRange(32))
    );
  }

  mod tampering {
    use super::*;

    #[test]
    fn a_flipped_bit_anywhere_is_refused() {
      let key = BallotKey::generate(&mut OsRng);
      let ballot = key.seal(SCOPE, &[1], &mut OsRng).unwrap();
      for at in 0..ballot.len() {
        let mut tampered = ballot.clone();
        tampered[at] ^= 1;
        assert_eq!(
          key.open(SCOPE, &tampered),
          Err(PollBallotError::BadBallot),
          "byte {at}"
        );
      }
    }

    #[test]
    fn a_ballot_of_another_poll_or_conversation_is_refused() {
      let key = BallotKey::generate(&mut OsRng);
      let ballot = key.seal(SCOPE, &[1], &mut OsRng).unwrap();
      let other_poll = BallotScope {
        poll_message_id: [4; 16],
        ..SCOPE
      };
      let other_conversation = BallotScope {
        conversation_id: 8,
        ..SCOPE
      };
      assert_eq!(key.open(other_poll, &ballot), Err(PollBallotError::BadBallot));
      assert_eq!(
        key.open(other_conversation, &ballot),
        Err(PollBallotError::BadBallot)
      );
    }

    #[test]
    fn a_ballot_under_another_key_is_refused() {
      let ballot = BallotKey::generate(&mut OsRng)
        .seal(SCOPE, &[1], &mut OsRng)
        .unwrap();
      let other = BallotKey::generate(&mut OsRng);
      assert_eq!(other.open(SCOPE, &ballot), Err(PollBallotError::BadBallot));
    }

    #[test]
    fn truncated_padded_or_empty_ballots_are_refused() {
      let key = BallotKey::generate(&mut OsRng);
      let ballot = key.seal(SCOPE, &[1], &mut OsRng).unwrap();
      assert_eq!(key.open(SCOPE, &[]), Err(PollBallotError::BadBallot));
      assert_eq!(
        key.open(SCOPE, &ballot[..ballot.len() - 1]),
        Err(PollBallotError::BadBallot)
      );
      let mut padded = ballot.clone();
      padded.push(0);
      assert_eq!(key.open(SCOPE, &padded), Err(PollBallotError::BadBallot));
    }

    #[test]
    fn malformed_keys_are_refused() {
      assert_eq!(BallotKey::from_text("").unwrap_err(), PollBallotError::BadKey);
      assert_eq!(BallotKey::from_text("!!").unwrap_err(), PollBallotError::BadKey);
      assert_eq!(
        BallotKey::from_text(&URL_SAFE_NO_PAD.encode([0u8; 31])).unwrap_err(),
        PollBallotError::BadKey
      );
    }
  }
}
