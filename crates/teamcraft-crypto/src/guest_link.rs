//! The secret of a meeting guest link and the guest's proof that they hold it (e2ee.md §4.6,
//! "Meeting guests").
//!
//! The server signs guest session tokens and decides admission, so it could admit a guest of its own
//! and hand the members that guest's identity key. A guest link therefore carries a random secret in
//! its `#fragment`, which no server sees. The guest sends a MAC over their user, identity key and
//! name under that secret; the host's device that made the link recomputes it before it adds the
//! guest's device. The server stores the proof but can neither make nor check one.

use hmac::{Hmac, KeyInit as _, Mac as _};
use rand_core::CryptoRngCore;
use sha2::Sha256;
use subtle::ConstantTimeEq as _;

use crate::domain;
use crate::identity::IdentityPublicKey;
use crate::ids::UserId;
use crate::secret::SymmetricKey;

const GUEST_LINK_PROOF: &str = "TeamCraft-v1 guest link proof";
/// Hex digits of the key code a host compares when the proof can't be checked: 64 bits, so
/// finding an identity key with a chosen code costs 2^64 key generations, not an afternoon.
const KEY_CODE_HEX_DIGITS: usize = 16;
/// Digits per group of the displayed code.
const KEY_CODE_GROUP: usize = 4;

/// Length of a proof in bytes.
pub const GUEST_LINK_PROOF_LEN: usize = 32;

/// The random secret of one guest link, made by the host's device. Never logged.
#[derive(Debug)]
pub struct GuestLinkSecret(SymmetricKey);

/// A guest's proof that they hold the link's secret.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct GuestLinkProof([u8; GUEST_LINK_PROOF_LEN]);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GuestLinkError {
  #[error("the guest link secret has the wrong size")]
  BadSecret,
  #[error("the guest link proof has the wrong size")]
  BadProofSize,
  #[error("the guest link proof does not match")]
  ProofMismatch,
}

impl GuestLinkSecret {
  pub fn generate(rng: &mut impl CryptoRngCore) -> Self {
    Self(SymmetricKey::generate(rng))
  }

  pub fn from_bytes(bytes: &[u8]) -> Result<Self, GuestLinkError> {
    SymmetricKey::from_slice(bytes)
      .map(Self)
      .ok_or(GuestLinkError::BadSecret)
  }

  pub fn expose_secret(&self) -> &[u8] {
    self.0.expose_secret()
  }

  /// The proof a guest with this secret gives for their user, identity key and display name, in
  /// `meeting` (the meeting's ID, so a proof for one meeting is no proof for another).
  pub fn prove(
    &self,
    meeting: u64,
    user: UserId,
    identity_key: &IdentityPublicKey,
    display_name: &str,
  ) -> GuestLinkProof {
    GuestLinkProof(
      self
        .mac(meeting, user, identity_key, display_name)
        .finalize()
        .into_bytes()
        .into(),
    )
  }

  /// Checks a guest's proof in constant time.
  pub fn verify(
    &self,
    meeting: u64,
    user: UserId,
    identity_key: &IdentityPublicKey,
    display_name: &str,
    proof: &[u8],
  ) -> Result<(), GuestLinkError> {
    if proof.len() != GUEST_LINK_PROOF_LEN {
      return Err(GuestLinkError::BadProofSize);
    }
    self
      .mac(meeting, user, identity_key, display_name)
      .verify_slice(proof)
      .map_err(|_| GuestLinkError::ProofMismatch)
  }

  fn mac(
    &self,
    meeting: u64,
    user: UserId,
    identity_key: &IdentityPublicKey,
    display_name: &str,
  ) -> Hmac<Sha256> {
    // Fixed-length fields first, the free-form name last, so no two inputs share an encoding.
    let mut body = Vec::with_capacity(16 + 32 + display_name.len());
    body.extend_from_slice(&meeting.to_be_bytes());
    body.extend_from_slice(&user.get().to_be_bytes());
    body.extend_from_slice(identity_key.as_bytes());
    body.extend_from_slice(display_name.as_bytes());
    // Invariant: HMAC takes keys of any length.
    let mut mac = match Hmac::<Sha256>::new_from_slice(self.0.expose_secret()) {
      Ok(mac) => mac,
      Err(_) => unreachable!("HMAC-SHA-256 takes a key of any size"),
    };
    mac.update(&domain::labeled(GUEST_LINK_PROOF, &body));
    mac
  }
}

impl GuestLinkProof {
  pub fn from_slice(bytes: &[u8]) -> Result<Self, GuestLinkError> {
    <[u8; GUEST_LINK_PROOF_LEN]>::try_from(bytes)
      .map(Self)
      .map_err(|_| GuestLinkError::BadProofSize)
  }

  pub fn as_bytes(&self) -> &[u8; GUEST_LINK_PROOF_LEN] {
    &self.0
  }
}

impl std::fmt::Debug for GuestLinkProof {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("GuestLinkProof(..)")
  }
}

/// The code of an identity key that a host compares with the guest's screen when there is no
/// proof to check: the first 64 bits of the key's fingerprint, in groups (`1a2b-3c4d-5e6f-7a8b`).
pub fn key_code(identity_key: &IdentityPublicKey) -> String {
  let digits: Vec<char> = identity_key
    .fingerprint()
    .to_string()
    .chars()
    .filter(char::is_ascii_hexdigit)
    .take(KEY_CODE_HEX_DIGITS)
    .collect();
  digits
    .chunks(KEY_CODE_GROUP)
    .map(|group| group.iter().collect::<String>())
    .collect::<Vec<_>>()
    .join("-")
}

/// Whether `typed` (any case, with or without the dashes and spaces) is the key code of the key.
pub fn key_code_matches(identity_key: &IdentityPublicKey, typed: &str) -> bool {
  let normalized = |code: &str| -> String {
    code
      .chars()
      .filter(char::is_ascii_hexdigit)
      .map(|c| c.to_ascii_lowercase())
      .collect()
  };
  let (expected, given) = (normalized(&key_code(identity_key)), normalized(typed));
  given.len() == KEY_CODE_HEX_DIGITS && expected.as_bytes().ct_eq(given.as_bytes()).into()
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;

  use super::*;
  use crate::identity::IdentityKeyPair;

  const MEETING: u64 = 7;

  #[test]
  fn a_proof_verifies_for_exactly_its_inputs() {
    let secret = GuestLinkSecret::generate(&mut OsRng);
    let key = IdentityKeyPair::generate(&mut OsRng).public_key();
    let other_key = IdentityKeyPair::generate(&mut OsRng).public_key();
    let proof = secret.prove(MEETING, UserId::new(1), &key, "Gus");

    assert_eq!(
      secret.verify(MEETING, UserId::new(1), &key, "Gus", proof.as_bytes()),
      Ok(())
    );
    for (meeting, user, key, name) in [
      (8, UserId::new(1), &key, "Gus"),
      (MEETING, UserId::new(2), &key, "Gus"),
      (MEETING, UserId::new(1), &other_key, "Gus"),
      (MEETING, UserId::new(1), &key, "Gus "),
    ] {
      assert_eq!(
        secret.verify(meeting, user, key, name, proof.as_bytes()),
        Err(GuestLinkError::ProofMismatch)
      );
    }
  }

  #[test]
  fn another_secret_or_a_short_proof_is_refused() {
    let secret = GuestLinkSecret::generate(&mut OsRng);
    let server_guess = GuestLinkSecret::generate(&mut OsRng);
    let key = IdentityKeyPair::generate(&mut OsRng).public_key();
    let forged = server_guess.prove(MEETING, UserId::new(1), &key, "Gus");

    assert_eq!(
      secret.verify(MEETING, UserId::new(1), &key, "Gus", forged.as_bytes()),
      Err(GuestLinkError::ProofMismatch)
    );
    assert_eq!(
      secret.verify(MEETING, UserId::new(1), &key, "Gus", &forged.as_bytes()[..31]),
      Err(GuestLinkError::BadProofSize)
    );
    assert_eq!(
      GuestLinkSecret::from_bytes(&[0; 5]).unwrap_err(),
      GuestLinkError::BadSecret
    );
  }

  #[test]
  fn the_key_code_is_sixteen_hex_digits_of_the_fingerprint_in_groups() {
    let key = IdentityKeyPair::generate(&mut OsRng).public_key();
    let code = key_code(&key);
    assert_eq!(code.len(), KEY_CODE_HEX_DIGITS + 3);
    assert_eq!(code.matches('-').count(), 3);
    let digits = code.replace('-', "");
    assert!(key.fingerprint().to_string().starts_with(&digits));
    assert!(key_code_matches(&key, &code.to_uppercase()));
    assert!(key_code_matches(&key, &digits));
    assert!(!key_code_matches(&key, &code[..code.len() - 1]));
    assert!(!key_code_matches(&key, "0000-0000-0000-0000"));
  }
}
