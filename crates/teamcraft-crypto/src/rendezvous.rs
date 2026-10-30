//! Device linking handshake (e2ee.md §5), modelled on Threema's connection rendezvous protocol.
//!
//! The new device (initiator) draws a 32-byte authentication key `AK` and shows it in a QR code.
//! The existing device (responder) scans it. Both run, over the server's opaque relay:
//!
//! 1. responder → initiator `hello`: its challenge and a *commitment* to its ephemeral X25519 key
//!    (a hash), sealed with the responder authentication key derived from `AK`;
//! 2. initiator → responder `auth_hello`: the responder's challenge back, its own challenge and
//!    ephemeral key, sealed with the initiator authentication key;
//! 3. responder → initiator `auth`: the initiator's challenge back and the committed key, which the
//!    initiator checks against the commitment.
//!
//! Both then derive per-direction transport keys and a *path hash* from `AK`, the X25519 shared
//! secret and the transcript (both ephemeral keys). Frames are ChaCha20-Poly1305 with a
//! per-direction counter as nonce, so reordering, replay and dropped frames fail. Handshake messages
//! carry a random nonce, so a code scanned twice never reuses one (security review B10).
//!
//! The 6-digit short code shown on both screens comes from the path hash. It matters when `AK`
//! travelled through the server (approval prompt instead of QR): a server in the middle has to fix
//! its key towards each side before it sees that side's key (the responder commits first, the
//! initiator sends before the reveal), so it can't search for a key that makes the codes match, and
//! low-order keys that would force a known shared secret are refused (security review B2).

use std::fmt;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use hkdf::Hkdf;
use rand_core::CryptoRngCore;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use x25519_dalek::{EphemeralSecret, PublicKey as X25519PublicKey, SharedSecret};
use zeroize::Zeroizing;

use crate::domain;
use crate::rng::CompatRng;
use crate::secret::{SYMMETRIC_KEY_LEN, SecretBytes, SymmetricKey};

const CHALLENGE_LEN: usize = 16;
const PUBLIC_KEY_LEN: usize = 32;
const COMMITMENT_LEN: usize = 32;
const NONCE_LEN: usize = 12;
/// Digits of the short code compared by the user (about 20 bits).
const SHORT_CODE_DIGITS: u32 = 6;

/// The authentication key `AK` (QR code content, together with the relay's rendezvous ID).
pub struct RendezvousKey(SymmetricKey);

/// Initiator after creating its state, waiting for `hello`.
pub struct Initiator {
  keys: AuthKeys,
  ephemeral: EphemeralSecret,
  challenge: [u8; CHALLENGE_LEN],
}

/// Initiator after sending `auth_hello`, waiting for `auth`.
pub struct InitiatorAwaitingAuth {
  keys: AuthKeys,
  ephemeral: EphemeralSecret,
  challenge: [u8; CHALLENGE_LEN],
  responder_commitment: [u8; COMMITMENT_LEN],
}

/// Responder after sending `hello`, waiting for `auth_hello`.
pub struct Responder {
  keys: AuthKeys,
  ephemeral: EphemeralSecret,
  challenge: [u8; CHALLENGE_LEN],
}

/// An authenticated, encrypted channel between the two devices.
pub struct RendezvousChannel {
  send: SymmetricKey,
  receive: SymmetricKey,
  send_counter: u64,
  receive_counter: u64,
  path_hash: [u8; 32],
}

struct AuthKeys {
  initiator: SymmetricKey,
  responder: SymmetricKey,
  ak: SymmetricKey,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
  Initiator,
  Responder,
}

#[derive(Debug, thiserror::Error)]
pub enum RendezvousError {
  #[error("handshake message could not be opened (wrong authentication key or tampered)")]
  BadHandshake,
  #[error("the peer did not answer the challenge correctly")]
  ChallengeMismatch,
  #[error("the peer's key doesn't match its commitment")]
  CommitmentMismatch,
  #[error("the peer's key is a low-order point")]
  NonContributory,
  #[error("frame could not be opened (tampered, replayed, reordered or wrong key)")]
  BadFrame,
  #[error("too many frames on this channel")]
  Exhausted,
}

impl RendezvousKey {
  pub fn generate(rng: &mut impl CryptoRngCore) -> Self {
    Self(SymmetricKey::generate(rng))
  }

  pub fn from_bytes(bytes: [u8; SYMMETRIC_KEY_LEN]) -> Self {
    Self(SymmetricKey::from_bytes(bytes))
  }

  pub fn expose_secret(&self) -> &[u8; SYMMETRIC_KEY_LEN] {
    self.0.expose_secret()
  }
}

impl fmt::Debug for RendezvousKey {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("RendezvousKey([redacted])")
  }
}

impl Initiator {
  pub fn new(key: &RendezvousKey, rng: &mut impl CryptoRngCore) -> Self {
    Self {
      keys: AuthKeys::derive(key),
      ephemeral: EphemeralSecret::random_from_rng(&mut CompatRng(&mut *rng)),
      challenge: challenge(rng),
    }
  }

  /// Opens the responder's `hello` and answers with `auth_hello`.
  pub fn receive_hello(
    self,
    hello: &[u8],
    rng: &mut impl CryptoRngCore,
  ) -> Result<(InitiatorAwaitingAuth, Vec<u8>), RendezvousError> {
    let opened = open_handshake(&self.keys.responder, hello)?;
    let body = opened.expose_secret();
    if body.len() != CHALLENGE_LEN + COMMITMENT_LEN {
      return Err(RendezvousError::BadHandshake);
    }
    let (responder_challenge, commitment) = body.split_at(CHALLENGE_LEN);
    let responder_commitment =
      <[u8; COMMITMENT_LEN]>::try_from(commitment).map_err(|_| RendezvousError::BadHandshake)?;
    let mut reply = responder_challenge.to_vec();
    reply.extend_from_slice(&self.challenge);
    reply.extend_from_slice(X25519PublicKey::from(&self.ephemeral).as_bytes());
    let auth_hello = seal_handshake(&self.keys.initiator, &reply, rng)?;
    Ok((
      InitiatorAwaitingAuth {
        keys: self.keys,
        ephemeral: self.ephemeral,
        challenge: self.challenge,
        responder_commitment,
      },
      auth_hello,
    ))
  }
}

impl InitiatorAwaitingAuth {
  /// Opens the responder's `auth`, checks its key against the commitment; the channel is ready
  /// afterwards.
  pub fn receive_auth(self, auth: &[u8]) -> Result<RendezvousChannel, RendezvousError> {
    let opened = open_handshake(&self.keys.responder, auth)?;
    let body = opened.expose_secret();
    if body.len() != CHALLENGE_LEN + PUBLIC_KEY_LEN {
      return Err(RendezvousError::BadHandshake);
    }
    let (response, responder_key) = body.split_at(CHALLENGE_LEN);
    if !bool::from(response.ct_eq(&self.challenge)) {
      return Err(RendezvousError::ChallengeMismatch);
    }
    let responder_key =
      <[u8; PUBLIC_KEY_LEN]>::try_from(responder_key).map_err(|_| RendezvousError::BadHandshake)?;
    if !bool::from(commitment(&responder_key).ct_eq(&self.responder_commitment)) {
      return Err(RendezvousError::CommitmentMismatch);
    }
    let own_key = X25519PublicKey::from(&self.ephemeral).to_bytes();
    let shared = contributory(
      self
        .ephemeral
        .diffie_hellman(&X25519PublicKey::from(responder_key)),
    )?;
    Ok(RendezvousChannel::derive(
      &self.keys.ak,
      &shared,
      &Transcript {
        initiator_key: own_key,
        responder_key,
      },
      Role::Initiator,
    ))
  }
}

impl Responder {
  /// Starts the handshake: the returned `hello` goes to the initiator first.
  pub fn new(key: &RendezvousKey, rng: &mut impl CryptoRngCore) -> Result<(Self, Vec<u8>), RendezvousError> {
    let keys = AuthKeys::derive(key);
    let ephemeral = EphemeralSecret::random_from_rng(&mut CompatRng(&mut *rng));
    let challenge = challenge(rng);
    let mut body = challenge.to_vec();
    body.extend_from_slice(&commitment(X25519PublicKey::from(&ephemeral).as_bytes()));
    let hello = seal_handshake(&keys.responder, &body, rng)?;
    Ok((
      Self {
        keys,
        ephemeral,
        challenge,
      },
      hello,
    ))
  }

  /// Opens `auth_hello`, checks the answer to its challenge and returns `auth` (revealing the
  /// committed key) plus the channel.
  pub fn receive_auth_hello(
    self,
    auth_hello: &[u8],
    rng: &mut impl CryptoRngCore,
  ) -> Result<(RendezvousChannel, Vec<u8>), RendezvousError> {
    let opened = open_handshake(&self.keys.initiator, auth_hello)?;
    let body = opened.expose_secret();
    if body.len() != 2 * CHALLENGE_LEN + PUBLIC_KEY_LEN {
      return Err(RendezvousError::BadHandshake);
    }
    let (response, rest) = body.split_at(CHALLENGE_LEN);
    let (initiator_challenge, initiator_key) = rest.split_at(CHALLENGE_LEN);
    if !bool::from(response.ct_eq(&self.challenge)) {
      return Err(RendezvousError::ChallengeMismatch);
    }
    let initiator_key =
      <[u8; PUBLIC_KEY_LEN]>::try_from(initiator_key).map_err(|_| RendezvousError::BadHandshake)?;
    let own_key = X25519PublicKey::from(&self.ephemeral).to_bytes();
    let shared = contributory(
      self
        .ephemeral
        .diffie_hellman(&X25519PublicKey::from(initiator_key)),
    )?;
    let mut reply = initiator_challenge.to_vec();
    reply.extend_from_slice(&own_key);
    let auth = seal_handshake(&self.keys.responder, &reply, rng)?;
    let transcript = Transcript {
      initiator_key,
      responder_key: own_key,
    };
    Ok((
      RendezvousChannel::derive(&self.keys.ak, &shared, &transcript, Role::Responder),
      auth,
    ))
  }
}

/// Both ephemeral keys, which the path hash and transport keys are bound to.
struct Transcript {
  initiator_key: [u8; PUBLIC_KEY_LEN],
  responder_key: [u8; PUBLIC_KEY_LEN],
}

impl RendezvousChannel {
  pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, RendezvousError> {
    let nonce = counter_nonce(self.send_counter);
    self.send_counter = self
      .send_counter
      .checked_add(1)
      .ok_or(RendezvousError::Exhausted)?;
    ChaCha20Poly1305::new(self.send.expose_secret().into())
      .encrypt(
        &nonce,
        Payload {
          msg: plaintext,
          aad: &[],
        },
      )
      .map_err(|_| RendezvousError::BadFrame)
  }

  pub fn open(&mut self, frame: &[u8]) -> Result<SecretBytes, RendezvousError> {
    let nonce = counter_nonce(self.receive_counter);
    let plaintext = ChaCha20Poly1305::new(self.receive.expose_secret().into())
      .decrypt(&nonce, Payload { msg: frame, aad: &[] })
      .map_err(|_| RendezvousError::BadFrame)?;
    self.receive_counter = self
      .receive_counter
      .checked_add(1)
      .ok_or(RendezvousError::Exhausted)?;
    Ok(SecretBytes::new(plaintext))
  }

  /// The code both screens show (e.g. "4719"); equal codes mean nobody sits in the middle.
  pub fn short_code(&self) -> String {
    let value = u32::from_be_bytes([
      self.path_hash[0],
      self.path_hash[1],
      self.path_hash[2],
      self.path_hash[3],
    ]);
    format!(
      "{:0width$}",
      value % 10u32.pow(SHORT_CODE_DIGITS),
      width = SHORT_CODE_DIGITS as usize
    )
  }

  fn derive(ak: &SymmetricKey, shared: &[u8; 32], transcript: &Transcript, role: Role) -> Self {
    let mut ikm = Zeroizing::new(ak.expose_secret().to_vec());
    ikm.extend_from_slice(shared);
    let mut salt = Vec::with_capacity(2 * PUBLIC_KEY_LEN);
    salt.extend_from_slice(&transcript.initiator_key);
    salt.extend_from_slice(&transcript.responder_key);
    let hkdf = Hkdf::<Sha256>::new(Some(&salt), &ikm);
    let initiator_to_responder = expand(&hkdf, domain::RENDEZVOUS_INITIATOR_TRANSPORT);
    let responder_to_initiator = expand(&hkdf, domain::RENDEZVOUS_RESPONDER_TRANSPORT);
    let path_hash = *expand(&hkdf, domain::RENDEZVOUS_PATH_HASH).expose_secret();
    let (send, receive) = match role {
      Role::Initiator => (initiator_to_responder, responder_to_initiator),
      Role::Responder => (responder_to_initiator, initiator_to_responder),
    };
    Self {
      send,
      receive,
      send_counter: 0,
      receive_counter: 0,
      path_hash,
    }
  }
}

impl fmt::Debug for RendezvousChannel {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("RendezvousChannel")
      .field("short_code", &self.short_code())
      .finish_non_exhaustive()
  }
}

impl AuthKeys {
  fn derive(key: &RendezvousKey) -> Self {
    let hkdf = Hkdf::<Sha256>::new(None, key.0.expose_secret());
    Self {
      initiator: expand(&hkdf, domain::RENDEZVOUS_INITIATOR_AUTH),
      responder: expand(&hkdf, domain::RENDEZVOUS_RESPONDER_AUTH),
      ak: key.0.duplicate(),
    }
  }
}

fn expand(hkdf: &Hkdf<Sha256>, label: &str) -> SymmetricKey {
  let mut output = Zeroizing::new([0u8; SYMMETRIC_KEY_LEN]);
  // Invariant: 32 bytes is far below HKDF-SHA256's 8160-byte output limit.
  if hkdf.expand(label.as_bytes(), output.as_mut()).is_err() {
    unreachable!("HKDF output length is valid");
  }
  SymmetricKey::from_bytes(*output)
}

fn challenge(rng: &mut impl CryptoRngCore) -> [u8; CHALLENGE_LEN] {
  let mut challenge = [0u8; CHALLENGE_LEN];
  rng.fill_bytes(&mut challenge);
  challenge
}

/// The responder's commitment to its ephemeral key.
fn commitment(public_key: &[u8; PUBLIC_KEY_LEN]) -> [u8; COMMITMENT_LEN] {
  Sha256::new()
    .chain_update(domain::RENDEZVOUS_COMMITMENT.as_bytes())
    .chain_update(public_key)
    .finalize()
    .into()
}

/// Refuses a shared secret a low-order peer key forced to a known value.
fn contributory(shared: SharedSecret) -> Result<Zeroizing<[u8; 32]>, RendezvousError> {
  if !shared.was_contributory() {
    return Err(RendezvousError::NonContributory);
  }
  Ok(Zeroizing::new(shared.to_bytes()))
}

/// Handshake messages are `nonce ‖ ciphertext` with a random nonce: a code used twice gets fresh
/// nonces under the same AK-derived keys.
fn seal_handshake(
  key: &SymmetricKey,
  body: &[u8],
  rng: &mut impl CryptoRngCore,
) -> Result<Vec<u8>, RendezvousError> {
  let mut nonce = [0u8; NONCE_LEN];
  rng.fill_bytes(&mut nonce);
  let sealed = ChaCha20Poly1305::new(key.expose_secret().into())
    .encrypt(&Nonce::from(nonce), Payload { msg: body, aad: &[] })
    .map_err(|_| RendezvousError::BadHandshake)?;
  let mut out = nonce.to_vec();
  out.extend_from_slice(&sealed);
  Ok(out)
}

fn open_handshake(key: &SymmetricKey, sealed: &[u8]) -> Result<SecretBytes, RendezvousError> {
  let (nonce, ciphertext) = sealed
    .split_first_chunk::<NONCE_LEN>()
    .ok_or(RendezvousError::BadHandshake)?;
  ChaCha20Poly1305::new(key.expose_secret().into())
    .decrypt(
      &Nonce::from(*nonce),
      Payload {
        msg: ciphertext,
        aad: &[],
      },
    )
    .map(SecretBytes::new)
    .map_err(|_| RendezvousError::BadHandshake)
}

fn counter_nonce(counter: u64) -> Nonce {
  let mut nonce = [0u8; 12];
  nonce[4..].copy_from_slice(&counter.to_be_bytes());
  Nonce::from(nonce)
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;

  use super::*;

  #[test]
  fn handshake_establishes_a_channel_with_matching_codes() {
    let key = RendezvousKey::generate(&mut OsRng);
    let (mut new_device, mut old_device) = handshake(&key, &key).unwrap();
    assert_eq!(new_device.short_code(), old_device.short_code());
    assert_eq!(new_device.short_code().len(), 6);

    let frame = old_device.seal(b"identity key and key bag").unwrap();
    assert_eq!(
      new_device.open(&frame).unwrap().expose_secret(),
      b"identity key and key bag"
    );
    let answer = new_device.seal(b"thanks").unwrap();
    assert_eq!(old_device.open(&answer).unwrap().expose_secret(), b"thanks");
  }

  #[test]
  fn a_code_used_twice_gets_fresh_handshake_nonces() {
    let key = RendezvousKey::generate(&mut OsRng);
    let (_, first) = Responder::new(&key, &mut OsRng).unwrap();
    let (_, second) = Responder::new(&key, &mut OsRng).unwrap();
    assert_ne!(first[..NONCE_LEN], second[..NONCE_LEN]);
  }

  mod rejection {
    use super::*;

    #[test]
    fn wrong_authentication_key_fails() {
      let key = RendezvousKey::generate(&mut OsRng);
      let other = RendezvousKey::generate(&mut OsRng);
      assert!(matches!(
        handshake(&key, &other),
        Err(RendezvousError::BadHandshake)
      ));
    }

    #[test]
    fn server_in_the_middle_shows_different_codes() {
      // The approval-prompt variant: the server learned AK and runs a handshake with each side.
      let key = RendezvousKey::generate(&mut OsRng);
      let (new_device, _attacker_towards_new) = handshake(&key, &key).unwrap();
      let (_attacker_towards_old, old_device) = handshake(&key, &key).unwrap();
      assert_ne!(new_device.short_code(), old_device.short_code());
    }

    #[test]
    fn low_order_keys_are_refused() {
      // A server knowing AK answers with the identity point, which would force a zero secret.
      let key = RendezvousKey::generate(&mut OsRng);
      let keys = AuthKeys::derive(&key);
      let (responder, _) = Responder::new(&key, &mut OsRng).unwrap();
      let mut forged = responder.challenge.to_vec();
      forged.extend_from_slice(&[7; CHALLENGE_LEN]);
      forged.extend_from_slice(&[0; PUBLIC_KEY_LEN]);
      let auth_hello = seal_handshake(&keys.initiator, &forged, &mut OsRng).unwrap();
      assert!(matches!(
        responder.receive_auth_hello(&auth_hello, &mut OsRng),
        Err(RendezvousError::NonContributory)
      ));
    }

    #[test]
    fn a_key_other_than_the_committed_one_is_refused() {
      // The server swaps the responder's revealed key for one it picked after seeing the
      // initiator's key.
      let key = RendezvousKey::generate(&mut OsRng);
      let keys = AuthKeys::derive(&key);
      let initiator = Initiator::new(&key, &mut OsRng);
      let (responder, hello) = Responder::new(&key, &mut OsRng).unwrap();
      let (initiator, auth_hello) = initiator.receive_hello(&hello, &mut OsRng).unwrap();
      let (_, honest_auth) = responder.receive_auth_hello(&auth_hello, &mut OsRng).unwrap();
      let opened = open_handshake(&keys.responder, &honest_auth).unwrap();
      let mut swapped = opened.expose_secret()[..CHALLENGE_LEN].to_vec();
      let other = EphemeralSecret::random_from_rng(&mut CompatRng(&mut OsRng));
      swapped.extend_from_slice(X25519PublicKey::from(&other).as_bytes());
      let forged = seal_handshake(&keys.responder, &swapped, &mut OsRng).unwrap();
      assert!(matches!(
        initiator.receive_auth(&forged),
        Err(RendezvousError::CommitmentMismatch)
      ));
    }

    #[test]
    fn frames_cannot_be_replayed_reordered_or_tampered() {
      let key = RendezvousKey::generate(&mut OsRng);
      let (mut new_device, mut old_device) = handshake(&key, &key).unwrap();
      let first = old_device.seal(b"first").unwrap();
      let second = old_device.seal(b"second").unwrap();
      assert!(matches!(new_device.open(&second), Err(RendezvousError::BadFrame)));
      let mut tampered = first.clone();
      tampered[0] ^= 1;
      assert!(matches!(
        new_device.open(&tampered),
        Err(RendezvousError::BadFrame)
      ));
      new_device.open(&first).unwrap();
      assert!(matches!(new_device.open(&first), Err(RendezvousError::BadFrame)));
      // A device's own frames are not accepted back (direction keys differ).
      let own = new_device.seal(b"echo").unwrap();
      assert!(new_device.open(&own).is_err());
    }

    #[test]
    fn tampered_handshake_messages_fail() {
      let key = RendezvousKey::generate(&mut OsRng);
      let initiator = Initiator::new(&key, &mut OsRng);
      let (_, mut hello) = Responder::new(&key, &mut OsRng).unwrap();
      hello[NONCE_LEN + 3] ^= 1;
      assert!(matches!(
        initiator.receive_hello(&hello, &mut OsRng),
        Err(RendezvousError::BadHandshake)
      ));
      for short in [&[][..], &[1, 2, 3]] {
        assert!(matches!(
          Initiator::new(&key, &mut OsRng).receive_hello(short, &mut OsRng),
          Err(RendezvousError::BadHandshake)
        ));
      }
    }
  }

  /// Runs the three handshake messages between an initiator and a responder.
  fn handshake(
    initiator_key: &RendezvousKey,
    responder_key: &RendezvousKey,
  ) -> Result<(RendezvousChannel, RendezvousChannel), RendezvousError> {
    let initiator = Initiator::new(initiator_key, &mut OsRng);
    let (responder, hello) = Responder::new(responder_key, &mut OsRng)?;
    let (initiator, auth_hello) = initiator.receive_hello(&hello, &mut OsRng)?;
    let (responder_channel, auth) = responder.receive_auth_hello(&auth_hello, &mut OsRng)?;
    let initiator_channel = initiator.receive_auth(&auth)?;
    Ok((initiator_channel, responder_channel))
  }
}
