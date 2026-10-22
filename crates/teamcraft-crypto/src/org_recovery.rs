//! Organization recovery (e2ee.md §6.3.1).
//!
//! - The org key pair is derived from a random 32-byte secret `s` with HPKE `DeriveKeyPair`
//!   (RFC 9180 §7.1.3, DHKEM(X25519, HKDF-SHA256)).
//! - `s` is split t-of-N with Shamir's secret sharing over GF(2⁸) (`vsss-rs`).
//! - Members wrap their backup key to the org public key with HPKE base mode
//!   ([`OrgRecoveryWrapper`], a [`BackupKeyWrapper`]).
//! - Shares travel to admin devices, and a recovered backup key to the recovering device's one-time
//!   [`RecoveryRequestKey`], sealed with HPKE to their X25519 keys.
//! - The request key reaches the admins through the server, so it is only trusted after the admin
//!   compared the request's verification code with the user (by phone or in person): only a
//!   [`ConfirmedRecoveryRequest`] can receive the backup key, and every seal for a recovery is bound
//!   to that request (security review B3).

use std::fmt;

use hpke_rs::{Hpke, HpkePrivateKey, HpkePublicKey, Mode};
use hpke_rs_crypto::types::{AeadAlgorithm, KdfAlgorithm, KemAlgorithm};
use hpke_rs_rust_crypto::HpkeRustCrypto;
use rand_chacha_010::ChaCha20Rng;
use rand_chacha_010::rand_core::SeedableRng;
use rand_core::CryptoRngCore;
use subtle::ConstantTimeEq as _;
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};
use zeroize::Zeroizing;

use crate::backup::{BackupError, BackupKey, BackupKeyWrapper, WrapMethod, WrappedBackupKey, wrap_aad};
use crate::domain;
use crate::identity::{DeviceAgreementPublicKey, DeviceKeyPair};
use crate::ids::UserId;
use crate::secret::{SYMMETRIC_KEY_LEN, SecretBytes, SymmetricKey};

/// Length of an X25519 public key and of the HPKE encapsulated key.
const KEY_LEN: usize = 32;

/// The org recovery public key, published in the signed admin roster.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct OrgRecoveryPublicKey([u8; KEY_LEN]);

/// The org secret `s`. Exists only while setting up and during one recovery.
pub struct OrgRecoverySecret(SymmetricKey);

/// One admin's share of `s` (a participant ID byte followed by the share bytes).
pub struct OrgRecoveryShare(SecretBytes);

/// Result of [`create_org_recovery`]: the public key to publish and one share per admin.
pub struct OrgRecoverySetup {
  pub public_key: OrgRecoveryPublicKey,
  pub shares: Vec<OrgRecoveryShare>,
}

/// Wraps backup keys to the org public key; with the recovered secret it also unwraps.
pub struct OrgRecoveryWrapper {
  public_key: OrgRecoveryPublicKey,
  maybe_secret: Option<OrgRecoverySecret>,
}

/// A one-time X25519 key of a device that is being recovered; admins seal the backup key to it.
pub struct RecoveryRequestKey {
  secret: StaticSecret,
}

/// Public part of a [`RecoveryRequestKey`], shown to the approving admins.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RecoveryRequestPublicKey([u8; KEY_LEN]);

/// A recovering device's request: who is recovered, and the one-time key the backup key goes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryRequest {
  request_id: [u8; 16],
  user_id: UserId,
  public_key: RecoveryRequestPublicKey,
}

/// A request whose verification code an admin compared with the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfirmedRecoveryRequest(RecoveryRequest);

/// What a sealed payload is; part of the HPKE info, so one kind can't be passed off as another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum SealPurpose {
  Share = 1,
  BackupKey = 2,
  /// An approving admin's share handed to the combining admin for one request.
  ShareForRequest = 3,
}

#[derive(Debug, thiserror::Error)]
pub enum OrgRecoveryError {
  #[error("the verification code doesn't match the recovery request")]
  CodeMismatch,
  #[error("threshold {threshold} of {share_count} shares is not possible (need 2 <= t <= n <= 255)")]
  InvalidThreshold { threshold: u8, share_count: u8 },
  #[error("secret sharing failed: {0}")]
  Sharing(String),
  #[error("HPKE failed: {0}")]
  Hpke(String),
  #[error("sealed payload could not be opened (wrong key, wrong recipient or tampered)")]
  OpenFailed,
  #[error("recovered secret has the wrong length")]
  BadSecret,
}

/// Creates the org key and splits its secret into `share_count` shares, `threshold` of which are
/// needed to recover.
pub fn create_org_recovery(
  threshold: u8,
  share_count: u8,
  rng: &mut impl CryptoRngCore,
) -> Result<OrgRecoverySetup, OrgRecoveryError> {
  if threshold < 2 || threshold > share_count {
    return Err(OrgRecoveryError::InvalidThreshold {
      threshold,
      share_count,
    });
  }
  let secret = OrgRecoverySecret(SymmetricKey::generate(rng));
  let public_key = secret.public_key()?;
  let mut seed = Zeroizing::new([0u8; 32]);
  rng.fill_bytes(seed.as_mut());
  let shares = vsss_rs::Gf256::split_bytes(
    usize::from(threshold),
    usize::from(share_count),
    secret.0.expose_secret(),
    ChaCha20Rng::from_seed(*seed),
  )
  .map_err(|error| OrgRecoveryError::Sharing(format!("{error:?}")))?;
  Ok(OrgRecoverySetup {
    public_key,
    shares: shares
      .into_iter()
      .map(|share| OrgRecoveryShare(SecretBytes::new(share)))
      .collect(),
  })
}

/// Combines at least `threshold` shares into the org secret.
pub fn combine_shares(shares: &[OrgRecoveryShare]) -> Result<OrgRecoverySecret, OrgRecoveryError> {
  let raw: Vec<Vec<u8>> = shares
    .iter()
    .map(|share| share.0.expose_secret().to_vec())
    .collect();
  let combined = Zeroizing::new(
    vsss_rs::Gf256::combine_bytes(&raw).map_err(|error| OrgRecoveryError::Sharing(format!("{error:?}")))?,
  );
  for mut share in raw {
    zeroize::Zeroize::zeroize(&mut share);
  }
  SymmetricKey::from_slice(&combined)
    .map(OrgRecoverySecret)
    .ok_or(OrgRecoveryError::BadSecret)
}

impl OrgRecoverySecret {
  pub fn public_key(&self) -> Result<OrgRecoveryPublicKey, OrgRecoveryError> {
    let (_, public_key) = self.key_pair()?;
    Ok(public_key)
  }

  fn key_pair(&self) -> Result<(HpkePrivateKey, OrgRecoveryPublicKey), OrgRecoveryError> {
    let pair = hpke()
      .derive_key_pair(self.0.expose_secret())
      .map_err(|error| OrgRecoveryError::Hpke(format!("{error:?}")))?;
    let (private_key, public_key) = pair.into_keys();
    let public_key =
      <[u8; KEY_LEN]>::try_from(public_key.as_slice()).map_err(|_| OrgRecoveryError::BadSecret)?;
    Ok((private_key, OrgRecoveryPublicKey(public_key)))
  }
}

impl fmt::Debug for OrgRecoverySecret {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("OrgRecoverySecret([redacted])")
  }
}

impl OrgRecoveryShare {
  pub fn from_bytes(bytes: Vec<u8>) -> Self {
    Self(SecretBytes::new(bytes))
  }

  pub fn expose_secret(&self) -> &[u8] {
    self.0.expose_secret()
  }

  /// Seals the share to one of the admin's devices (HPKE, info binds the admin).
  pub fn seal_for_device(
    &self,
    admin: UserId,
    device: &DeviceAgreementPublicKey,
  ) -> Result<Vec<u8>, OrgRecoveryError> {
    seal(
      device.as_bytes(),
      SealPurpose::Share,
      admin,
      self.0.expose_secret(),
    )
  }

  /// An approving admin hands its share to the combining admin's device for one request only.
  pub fn seal_for_request(
    &self,
    combiner_device: &DeviceAgreementPublicKey,
    request: &RecoveryRequest,
  ) -> Result<Vec<u8>, OrgRecoveryError> {
    seal_with(
      combiner_device.as_bytes(),
      &request.seal_info(SealPurpose::ShareForRequest),
      self.0.expose_secret(),
    )
  }

  /// The combining admin opens a share handed over for `request`.
  pub fn open_for_request(
    device: &DeviceKeyPair,
    request: &RecoveryRequest,
    sealed: &[u8],
  ) -> Result<Self, OrgRecoveryError> {
    let (_, agreement) = device.to_secrets();
    let opened = open_with(
      agreement.expose_secret(),
      &request.seal_info(SealPurpose::ShareForRequest),
      sealed,
    )?;
    Ok(Self(opened))
  }

  /// Opens a share sealed to this device.
  pub fn open_on_device(device: &DeviceKeyPair, sealed: &[u8]) -> Result<Self, OrgRecoveryError> {
    let (_, agreement) = device.to_secrets();
    let opened = open(
      agreement.expose_secret(),
      SealPurpose::Share,
      device.user_id(),
      sealed,
    )?;
    Ok(Self(opened))
  }
}

impl fmt::Debug for OrgRecoveryShare {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("OrgRecoveryShare([redacted])")
  }
}

impl OrgRecoveryPublicKey {
  pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
    Self(bytes)
  }

  pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
    &self.0
  }
}

impl fmt::Debug for OrgRecoveryPublicKey {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("OrgRecoveryPublicKey(")?;
    for byte in self.0 {
      write!(f, "{byte:02x}")?;
    }
    f.write_str(")")
  }
}

impl OrgRecoveryWrapper {
  /// Members: wrap the own backup key to the org key.
  pub fn for_wrapping(public_key: OrgRecoveryPublicKey) -> Self {
    Self {
      public_key,
      maybe_secret: None,
    }
  }

  /// The combining admin: unwrap a member's backup key with the recovered secret.
  pub fn for_unwrapping(secret: OrgRecoverySecret) -> Result<Self, OrgRecoveryError> {
    let public_key = secret.public_key()?;
    Ok(Self {
      public_key,
      maybe_secret: Some(secret),
    })
  }
}

impl BackupKeyWrapper for OrgRecoveryWrapper {
  fn method(&self) -> WrapMethod {
    WrapMethod::OrgRecovery
  }

  fn wrap_key(
    &self,
    user_id: UserId,
    backup_key: &BackupKey,
    _rng: &mut dyn CryptoRngCore,
  ) -> Result<WrappedBackupKey, BackupError> {
    // The org public key is the wrap's public parameter: it tells wraps of rotated keys apart.
    let parameters = self.public_key.0.to_vec();
    let aad = wrap_aad(user_id, WrapMethod::OrgRecovery, &parameters)?;
    let info = domain::labeled(domain::ORG_RECOVERY_WRAP, &user_id.get().to_be_bytes());
    let (encapsulated, ciphertext) = hpke()
      .seal(
        &HpkePublicKey::new(parameters.clone()),
        &info,
        &aad,
        backup_key.key().expose_secret(),
        None,
        None,
        None,
      )
      .map_err(|_| BackupError::EncryptionFailed)?;
    let mut combined = encapsulated;
    combined.extend_from_slice(&ciphertext);
    Ok(WrappedBackupKey::from_parts(
      WrapMethod::OrgRecovery,
      parameters,
      combined,
    ))
  }

  fn unwrap_key(&self, user_id: UserId, wrapped: &WrappedBackupKey) -> Result<BackupKey, BackupError> {
    if wrapped.method() != WrapMethod::OrgRecovery {
      return Err(BackupError::WrongMethod {
        expected: WrapMethod::OrgRecovery,
        actual: wrapped.method(),
      });
    }
    if wrapped.version() != 1 {
      return Err(BackupError::UnsupportedVersion(wrapped.version()));
    }
    let secret = self.maybe_secret.as_ref().ok_or(BackupError::MissingPrivateKey)?;
    if wrapped.parameters() != self.public_key.0.as_slice() {
      // Wrapped to another (older or newer) org key.
      return Err(BackupError::UnwrapFailed);
    }
    let (private_key, _) = secret.key_pair().map_err(|_| BackupError::UnwrapFailed)?;
    let aad = wrap_aad(user_id, WrapMethod::OrgRecovery, wrapped.parameters())?;
    let info = domain::labeled(domain::ORG_RECOVERY_WRAP, &user_id.get().to_be_bytes());
    let (encapsulated, ciphertext) =
      split_encapsulated(wrapped.ciphertext()).ok_or(BackupError::UnwrapFailed)?;
    let plaintext = Zeroizing::new(
      hpke()
        .open(
          encapsulated,
          &private_key,
          &info,
          &aad,
          ciphertext,
          None,
          None,
          None,
        )
        .map_err(|_| BackupError::UnwrapFailed)?,
    );
    SymmetricKey::from_slice(&plaintext)
      .map(BackupKey::from_key)
      .ok_or(BackupError::UnwrapFailed)
  }
}

impl fmt::Debug for OrgRecoveryWrapper {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("OrgRecoveryWrapper")
      .field("public_key", &self.public_key)
      .finish_non_exhaustive()
  }
}

impl RecoveryRequestKey {
  pub fn generate(rng: &mut impl CryptoRngCore) -> Self {
    let mut bytes = Zeroizing::new([0u8; KEY_LEN]);
    rng.fill_bytes(bytes.as_mut());
    Self {
      secret: StaticSecret::from(*bytes),
    }
  }

  pub fn public_key(&self) -> RecoveryRequestPublicKey {
    RecoveryRequestPublicKey(X25519PublicKey::from(&self.secret).to_bytes())
  }

  /// Opens the backup key the combining admin sealed to `request` (made from this key).
  pub fn open_backup_key(
    &self,
    request: &RecoveryRequest,
    sealed: &[u8],
  ) -> Result<BackupKey, OrgRecoveryError> {
    let info = request.seal_info(SealPurpose::BackupKey);
    let opened = open_with(&self.secret.to_bytes(), &info, sealed)?;
    SymmetricKey::from_slice(opened.expose_secret())
      .map(BackupKey::from_key)
      .ok_or(OrgRecoveryError::OpenFailed)
  }
}

impl RecoveryRequestPublicKey {
  /// What the recovering device shows and the approving admins compare: eight groups of four hex
  /// digits from a hash of the key, so a swapped key is noticed.
  pub fn fingerprint(&self) -> String {
    use sha2::{Digest as _, Sha256};
    let digest = Sha256::digest(domain::labeled(domain::RECOVERY_REQUEST_FINGERPRINT, &self.0));
    digest[..16]
      .chunks(2)
      .map(|pair| format!("{:02x}{:02x}", pair[0], pair[1]))
      .collect::<Vec<_>>()
      .join("-")
  }
}

impl fmt::Debug for RecoveryRequestKey {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("RecoveryRequestKey")
      .field("public_key", &self.public_key().0)
      .finish_non_exhaustive()
  }
}

impl RecoveryRequestPublicKey {
  pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
    Self(bytes)
  }

  pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
    &self.0
  }
}

impl RecoveryRequest {
  /// The recovering device's request for `user` with a fresh request ID.
  pub fn new(user_id: UserId, key: &RecoveryRequestKey, rng: &mut impl CryptoRngCore) -> Self {
    let mut request_id = [0u8; 16];
    rng.fill_bytes(&mut request_id);
    Self {
      request_id,
      user_id,
      public_key: key.public_key(),
    }
  }

  /// The request the server registered under `server_id` (its recovery request ID): the ID is
  /// bound into the seals, so a sealed key or share can't be moved to another request.
  pub fn for_server_id(server_id: u64, user_id: UserId, public_key: RecoveryRequestPublicKey) -> Self {
    let mut request_id = [0u8; 16];
    request_id[..8].copy_from_slice(&server_id.to_be_bytes());
    Self::from_parts(request_id, user_id, public_key)
  }

  /// A request as it arrived (from the server).
  pub fn from_parts(request_id: [u8; 16], user_id: UserId, public_key: RecoveryRequestPublicKey) -> Self {
    Self {
      request_id,
      user_id,
      public_key,
    }
  }

  pub fn request_id(&self) -> &[u8; 16] {
    &self.request_id
  }

  pub fn user_id(&self) -> UserId {
    self.user_id
  }

  pub fn public_key(&self) -> &RecoveryRequestPublicKey {
    &self.public_key
  }

  /// The code the recovering device shows and the user reads to the admin: the request key's
  /// [`RecoveryRequestPublicKey::fingerprint`]. A key the server swapped has another code; the
  /// request ID and user are bound through the seals.
  pub fn verification_code(&self) -> String {
    self.public_key.fingerprint()
  }

  /// The admin entered the code the user read out; separators and case don't matter.
  pub fn confirm(self, code: &str) -> Result<ConfirmedRecoveryRequest, OrgRecoveryError> {
    let normalize = |text: &str| {
      text
        .chars()
        .filter(char::is_ascii_hexdigit)
        .map(|character| character.to_ascii_lowercase())
        .collect::<String>()
    };
    let expected = normalize(&self.verification_code());
    if bool::from(expected.as_bytes().ct_eq(normalize(code).as_bytes())) {
      Ok(ConfirmedRecoveryRequest(self))
    } else {
      Err(OrgRecoveryError::CodeMismatch)
    }
  }

  fn seal_info(&self, purpose: SealPurpose) -> Vec<u8> {
    let mut info = seal_info(purpose, self.user_id);
    info.extend_from_slice(&self.request_id);
    info.extend_from_slice(&self.public_key.0);
    info
  }
}

impl ConfirmedRecoveryRequest {
  pub fn request(&self) -> &RecoveryRequest {
    &self.0
  }

  /// The combining admin seals the recovered backup key to the confirmed request.
  pub fn seal_backup_key(&self, backup_key: &BackupKey) -> Result<Vec<u8>, OrgRecoveryError> {
    seal_with(
      &self.0.public_key.0,
      &self.0.seal_info(SealPurpose::BackupKey),
      backup_key.key().expose_secret(),
    )
  }
}

impl fmt::Debug for RecoveryRequestPublicKey {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("RecoveryRequestPublicKey(")?;
    for byte in self.0 {
      write!(f, "{byte:02x}")?;
    }
    f.write_str(")")
  }
}

fn hpke() -> Hpke<HpkeRustCrypto> {
  Hpke::new(
    Mode::Base,
    KemAlgorithm::DhKem25519,
    KdfAlgorithm::HkdfSha256,
    AeadAlgorithm::ChaCha20Poly1305,
  )
}

fn seal_info(purpose: SealPurpose, user: UserId) -> Vec<u8> {
  let mut body = vec![purpose as u8];
  body.extend_from_slice(&user.get().to_be_bytes());
  domain::labeled(domain::ORG_RECOVERY_SEAL, &body)
}

/// HPKE base-mode seal to an X25519 public key: `encapsulated key (32) || ciphertext`.
fn seal(
  recipient: &[u8; KEY_LEN],
  purpose: SealPurpose,
  user: UserId,
  plaintext: &[u8],
) -> Result<Vec<u8>, OrgRecoveryError> {
  seal_with(recipient, &seal_info(purpose, user), plaintext)
}

fn seal_with(recipient: &[u8; KEY_LEN], info: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, OrgRecoveryError> {
  let (mut sealed, ciphertext) = hpke()
    .seal(
      &HpkePublicKey::new(recipient.to_vec()),
      info,
      &[],
      plaintext,
      None,
      None,
      None,
    )
    .map_err(|error| OrgRecoveryError::Hpke(format!("{error:?}")))?;
  sealed.extend_from_slice(&ciphertext);
  Ok(sealed)
}

fn open(
  private_key: &[u8; KEY_LEN],
  purpose: SealPurpose,
  user: UserId,
  sealed: &[u8],
) -> Result<SecretBytes, OrgRecoveryError> {
  open_with(private_key, &seal_info(purpose, user), sealed)
}

fn open_with(
  private_key: &[u8; KEY_LEN],
  info: &[u8],
  sealed: &[u8],
) -> Result<SecretBytes, OrgRecoveryError> {
  let (encapsulated, ciphertext) = split_encapsulated(sealed).ok_or(OrgRecoveryError::OpenFailed)?;
  hpke()
    .open(
      encapsulated,
      &HpkePrivateKey::new(private_key.to_vec()),
      info,
      &[],
      ciphertext,
      None,
      None,
      None,
    )
    .map(SecretBytes::new)
    .map_err(|_| OrgRecoveryError::OpenFailed)
}

fn split_encapsulated(sealed: &[u8]) -> Option<(&[u8], &[u8])> {
  (sealed.len() > KEY_LEN).then(|| sealed.split_at(KEY_LEN))
}

// Invariant: the org secret is a 256-bit key (HPKE DeriveKeyPair accepts any length >= Nsk).
const _: () = assert!(SYMMETRIC_KEY_LEN == KEY_LEN);

#[cfg(test)]
mod tests {
  use rand_core::OsRng;
  use subtle::ConstantTimeEq;

  use super::*;
  use crate::membership::tests::TestUser;

  const ALICE: UserId = UserId::new(1);

  #[test]
  fn request_fingerprints_are_stable_distinct_and_readable() {
    let a = RecoveryRequestPublicKey::from_bytes([1; KEY_LEN]);
    let b = RecoveryRequestPublicKey::from_bytes([2; KEY_LEN]);
    assert_eq!(
      a.fingerprint(),
      RecoveryRequestPublicKey::from_bytes([1; KEY_LEN]).fingerprint()
    );
    assert_ne!(a.fingerprint(), b.fingerprint());
    let text = a.fingerprint();
    assert_eq!(text.len(), 8 * 4 + 7);
    assert!(
      text
        .split('-')
        .all(|group| group.len() == 4 && group.bytes().all(|c| c.is_ascii_hexdigit()))
    );
  }

  #[test]
  fn two_of_three_recover_a_wrapped_backup_key() {
    let setup = create_org_recovery(2, 3, &mut OsRng).unwrap();
    let backup_key = BackupKey::generate(&mut OsRng);
    let wrapped = OrgRecoveryWrapper::for_wrapping(setup.public_key)
      .wrap_key(ALICE, &backup_key, &mut OsRng)
      .unwrap();
    let wrapped = WrappedBackupKey::from_bytes(&wrapped.to_bytes().unwrap()).unwrap();

    // Every pair of the three admins can recover.
    for (first, second) in [(0, 1), (0, 2), (1, 2)] {
      let pair = [
        share_copy(&setup.shares[first]),
        share_copy(&setup.shares[second]),
      ];
      let secret = combine_shares(&pair).unwrap();
      assert_eq!(secret.public_key().unwrap(), setup.public_key);
      let unwrapped = OrgRecoveryWrapper::for_unwrapping(secret)
        .unwrap()
        .unwrap_key(ALICE, &wrapped)
        .unwrap();
      assert!(bool::from(unwrapped.key().ct_eq(backup_key.key())));
    }
  }

  #[test]
  fn shares_reach_admin_devices_and_backup_key_reaches_the_request() {
    let admin = TestUser::new(7);
    let setup = create_org_recovery(2, 2, &mut OsRng).unwrap();
    let sealed = setup.shares[0]
      .seal_for_device(admin.user_id, &admin.device.public_keys().agreement_key)
      .unwrap();
    let opened = OrgRecoveryShare::open_on_device(&admin.device, &sealed).unwrap();
    assert_eq!(opened.expose_secret(), setup.shares[0].expose_secret());

    let key = RecoveryRequestKey::generate(&mut OsRng);
    let request = RecoveryRequest::new(ALICE, &key, &mut OsRng);
    let code = request.verification_code();
    assert_eq!(code, key.public_key().fingerprint());
    let confirmed = request
      .clone()
      .confirm(&code.replace('-', " ").to_uppercase())
      .unwrap();
    let backup_key = BackupKey::generate(&mut OsRng);
    let sealed_key = confirmed.seal_backup_key(&backup_key).unwrap();
    let received = key.open_backup_key(&request, &sealed_key).unwrap();
    assert!(bool::from(received.key().ct_eq(backup_key.key())));

    // An approver's share for the combiner works for this request only.
    let combiner = TestUser::new(9);
    let handed = setup.shares[1]
      .seal_for_request(&combiner.device.public_keys().agreement_key, &request)
      .unwrap();
    let other_request = RecoveryRequest::new(ALICE, &key, &mut OsRng);
    assert!(OrgRecoveryShare::open_for_request(&combiner.device, &other_request, &handed).is_err());
    let opened = OrgRecoveryShare::open_for_request(&combiner.device, &request, &handed).unwrap();
    assert_eq!(opened.expose_secret(), setup.shares[1].expose_secret());
  }

  fn share_copy(share: &OrgRecoveryShare) -> OrgRecoveryShare {
    OrgRecoveryShare::from_bytes(share.expose_secret().to_vec())
  }

  mod rejection {
    use super::*;

    #[test]
    fn one_share_is_not_enough() {
      let setup = create_org_recovery(2, 3, &mut OsRng).unwrap();
      let single = combine_shares(&setup.shares[..1]);
      // One share reveals nothing; whatever comes out is not the org secret.
      assert!(single.map_or(true, |secret| secret.public_key().unwrap() != setup.public_key));
    }

    #[test]
    fn rejects_invalid_thresholds() {
      for (threshold, count) in [(1, 3), (0, 2), (4, 3)] {
        assert!(matches!(
          create_org_recovery(threshold, count, &mut OsRng),
          Err(OrgRecoveryError::InvalidThreshold { .. })
        ));
      }
    }

    #[test]
    fn wrap_is_bound_to_user_and_org_key() {
      let setup = create_org_recovery(2, 2, &mut OsRng).unwrap();
      let other = create_org_recovery(2, 2, &mut OsRng).unwrap();
      let backup_key = BackupKey::generate(&mut OsRng);
      let wrapped = OrgRecoveryWrapper::for_wrapping(setup.public_key)
        .wrap_key(ALICE, &backup_key, &mut OsRng)
        .unwrap();
      let secret = combine_shares(&setup.shares).unwrap();
      let unwrapper = OrgRecoveryWrapper::for_unwrapping(secret).unwrap();

      assert!(matches!(
        unwrapper.unwrap_key(UserId::new(2), &wrapped),
        Err(BackupError::UnwrapFailed)
      ));
      let other_unwrapper =
        OrgRecoveryWrapper::for_unwrapping(combine_shares(&other.shares).unwrap()).unwrap();
      assert!(matches!(
        other_unwrapper.unwrap_key(ALICE, &wrapped),
        Err(BackupError::UnwrapFailed)
      ));
      assert!(matches!(
        OrgRecoveryWrapper::for_wrapping(setup.public_key).unwrap_key(ALICE, &wrapped),
        Err(BackupError::MissingPrivateKey)
      ));
    }

    #[test]
    fn sealed_payloads_are_bound_to_recipient_user_and_purpose() {
      let admin = TestUser::new(7);
      let stranger = TestUser::new(8);
      let setup = create_org_recovery(2, 2, &mut OsRng).unwrap();
      let sealed = setup.shares[0]
        .seal_for_device(admin.user_id, &admin.device.public_keys().agreement_key)
        .unwrap();
      assert!(OrgRecoveryShare::open_on_device(&stranger.device, &sealed).is_err());
      let mut tampered = sealed.clone();
      let last = tampered.len() - 1;
      tampered[last] ^= 1;
      assert!(OrgRecoveryShare::open_on_device(&admin.device, &tampered).is_err());

      let key = RecoveryRequestKey::generate(&mut OsRng);
      let request = RecoveryRequest::new(ALICE, &key, &mut OsRng);
      let backup_key = BackupKey::generate(&mut OsRng);
      let sealed_key = request
        .clone()
        .confirm(&request.verification_code())
        .unwrap()
        .seal_backup_key(&backup_key)
        .unwrap();
      let for_bob = RecoveryRequest::from_parts(*request.request_id(), UserId::new(2), *request.public_key());
      assert!(key.open_backup_key(&for_bob, &sealed_key).is_err());
      // A share sealed to the request key isn't accepted as a backup key.
      let share_to_request = seal(key.public_key().as_bytes(), SealPurpose::Share, ALICE, &[1; 32]).unwrap();
      assert!(key.open_backup_key(&request, &share_to_request).is_err());
    }

    #[test]
    fn a_request_key_the_server_swapped_fails_the_code_check() {
      // Security review B3: the server replaces the recovering device's request key with its own.
      let device_key = RecoveryRequestKey::generate(&mut OsRng);
      let request = RecoveryRequest::new(ALICE, &device_key, &mut OsRng);
      let code_on_the_device = request.verification_code();
      let server_key = RecoveryRequestKey::generate(&mut OsRng);
      let swapped = RecoveryRequest::from_parts(*request.request_id(), ALICE, server_key.public_key());
      assert!(matches!(
        swapped.confirm(&code_on_the_device),
        Err(OrgRecoveryError::CodeMismatch)
      ));
    }
  }
}
