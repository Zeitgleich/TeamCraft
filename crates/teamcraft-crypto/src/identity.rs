//! Identity key and device keys (e2ee.md §2, §3).
//!
//! The identity key (Ed25519) *is* the user for other clients. It signs a [`DeviceCertificate`] for
//! every device; other clients only trust a device key that carries a valid certificate from the
//! identity key they pinned. The device key pair (Ed25519 + X25519) is the device's MLS credential
//! key, signs TeamCraft structures (e.g. membership changes) and receives key transfers.

use std::fmt;

use ed25519_dalek::{Signer as _, SigningKey, VerifyingKey};
use openmls_traits::signatures::{Signer as MlsSigner, SignerError as MlsSignerError};
use openmls_traits::types::SignatureScheme;
use rand_core::CryptoRngCore;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tls_codec::{DeserializeBytes as _, Serialize as _, TlsDeserializeBytes, TlsSerialize, TlsSize};
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};
use zeroize::Zeroizing;

use crate::domain;
use crate::ids::{DeviceId, UserId};
use crate::secret::SymmetricKey;

/// Version of the [`DeviceCertificate`] encoding.
const DEVICE_CERTIFICATE_VERSION: u8 = 1;

/// The user's long-term identity key pair. Lives in the key bag and on every device of the user.
pub struct IdentityKeyPair {
  signing_key: SigningKey,
}

/// Public part of an identity key. Clients pin it per user (trust on first use, e2ee.md §3).
#[derive(Clone, Copy, PartialEq, Eq, Hash, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct IdentityPublicKey([u8; 32]);

/// SHA-256 fingerprint of an identity public key, shown for optional verification and stored as
/// the OIDC claim `tc_identity_key` (e2ee.md §3.1).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct IdentityFingerprint([u8; 32]);

/// One device's key pairs.
pub struct DeviceKeyPair {
  user_id: UserId,
  device_id: DeviceId,
  signing_key: SigningKey,
  agreement_key: StaticSecret,
}

/// Public Ed25519 key of a device; also its MLS signature key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct DeviceSigningPublicKey([u8; 32]);

/// Public X25519 key of a device, used to receive key transfers (e2ee.md §5).
#[derive(Clone, Copy, PartialEq, Eq, Hash, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct DeviceAgreementPublicKey([u8; 32]);

/// Statement "this device key belongs to this user", signed by the user's identity key.
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct DeviceCertificate {
  version: u8,
  pub user_id: UserId,
  pub device_id: DeviceId,
  pub identity_key: IdentityPublicKey,
  pub signing_key: DeviceSigningPublicKey,
  pub agreement_key: DeviceAgreementPublicKey,
}

/// A device certificate with the identity key's signature. Uploaded to the server; untrusted until
/// [`SignedDeviceCertificate::verify`] succeeds against the pinned identity key.
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct SignedDeviceCertificate {
  certificate: DeviceCertificate,
  signature: Ed25519Signature,
}

/// A device certificate whose signature was checked against a pinned identity key. Only
/// [`SignedDeviceCertificate::verify`] creates it, so holding one proves the check happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedDevice {
  certificate: DeviceCertificate,
}

/// An Ed25519 signature.
#[derive(Clone, Copy, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct Ed25519Signature([u8; 64]);

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
  #[error("device certificate is signed by a different identity key than the pinned one")]
  IdentityKeyMismatch,
  #[error("device certificate signature is invalid")]
  InvalidSignature,
  #[error("unsupported device certificate version {0}")]
  UnsupportedVersion(u8),
  #[error("public key is not a valid Ed25519 point")]
  InvalidPublicKey,
  #[error("device certificate is for a different device key than the presented one")]
  DeviceKeyMismatch,
  #[error("encoding failed: {0}")]
  Encoding(#[from] tls_codec::Error),
}

impl IdentityKeyPair {
  pub fn generate(rng: &mut impl CryptoRngCore) -> Self {
    Self {
      signing_key: SigningKey::generate(rng),
    }
  }

  /// Restores the identity key from its secret seed (from the key bag or a device transfer).
  pub fn from_secret(secret: &SymmetricKey) -> Self {
    Self {
      signing_key: SigningKey::from_bytes(secret.expose_secret()),
    }
  }

  /// The secret seed, for storing in the key bag.
  pub fn to_secret(&self) -> SymmetricKey {
    SymmetricKey::from_bytes(self.signing_key.to_bytes())
  }

  pub fn public_key(&self) -> IdentityPublicKey {
    IdentityPublicKey(self.signing_key.verifying_key().to_bytes())
  }

  /// Signs a certificate for one of this user's devices.
  pub fn certify_device(
    &self,
    user_id: UserId,
    device: &DevicePublicKeys,
  ) -> Result<SignedDeviceCertificate, IdentityError> {
    let certificate = DeviceCertificate {
      version: DEVICE_CERTIFICATE_VERSION,
      user_id,
      device_id: device.device_id,
      identity_key: self.public_key(),
      signing_key: device.signing_key,
      agreement_key: device.agreement_key,
    };
    let message = certificate.signing_input()?;
    let signature = Ed25519Signature(self.signing_key.sign(&message).to_bytes());
    Ok(SignedDeviceCertificate {
      certificate,
      signature,
    })
  }
}

impl IdentityKeyPair {
  /// Signs a domain-separated statement of the user (e.g. the device list).
  pub(crate) fn sign_labeled(&self, label: &str, body: &[u8]) -> Ed25519Signature {
    Ed25519Signature(self.signing_key.sign(&domain::labeled(label, body)).to_bytes())
  }
}

impl fmt::Debug for IdentityKeyPair {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("IdentityKeyPair")
      .field("public_key", &self.public_key())
      .finish_non_exhaustive()
  }
}

impl IdentityPublicKey {
  pub fn from_bytes(bytes: [u8; 32]) -> Self {
    Self(bytes)
  }

  pub fn as_bytes(&self) -> &[u8; 32] {
    &self.0
  }

  pub fn fingerprint(&self) -> IdentityFingerprint {
    let digest = Sha256::digest(domain::labeled(domain::IDENTITY_FINGERPRINT, &self.0));
    IdentityFingerprint(digest.into())
  }

  fn verifying_key(&self) -> Result<VerifyingKey, IdentityError> {
    VerifyingKey::from_bytes(&self.0).map_err(|_| IdentityError::InvalidPublicKey)
  }

  pub(crate) fn verify_labeled(
    &self,
    label: &str,
    body: &[u8],
    signature: &Ed25519Signature,
  ) -> Result<(), IdentityError> {
    let signature = ed25519_dalek::Signature::from_bytes(&signature.0);
    self
      .verifying_key()?
      .verify_strict(&domain::labeled(label, body), &signature)
      .map_err(|_| IdentityError::InvalidSignature)
  }
}

impl ConstantTimeEq for IdentityPublicKey {
  fn ct_eq(&self, other: &Self) -> subtle::Choice {
    self.0.ct_eq(&other.0)
  }
}

impl fmt::Debug for IdentityPublicKey {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "IdentityPublicKey({})", hex(&self.0))
  }
}

impl IdentityFingerprint {
  pub fn as_bytes(&self) -> &[u8; 32] {
    &self.0
  }
}

impl fmt::Display for IdentityFingerprint {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&hex(&self.0))
  }
}

impl fmt::Debug for IdentityFingerprint {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "IdentityFingerprint({self})")
  }
}

/// Public keys of a device, as uploaded to the server and put into its certificate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DevicePublicKeys {
  pub device_id: DeviceId,
  pub signing_key: DeviceSigningPublicKey,
  pub agreement_key: DeviceAgreementPublicKey,
}

impl DeviceKeyPair {
  pub fn generate(user_id: UserId, device_id: DeviceId, rng: &mut impl CryptoRngCore) -> Self {
    let signing_key = SigningKey::generate(rng);
    let mut agreement_seed = Zeroizing::new([0u8; 32]);
    rng.fill_bytes(agreement_seed.as_mut());
    let agreement_key = StaticSecret::from(*agreement_seed);
    Self {
      user_id,
      device_id,
      signing_key,
      agreement_key,
    }
  }

  /// Restores a device key pair from local secure storage.
  pub fn from_secrets(
    user_id: UserId,
    device_id: DeviceId,
    signing_secret: &SymmetricKey,
    agreement_secret: &SymmetricKey,
  ) -> Self {
    Self {
      user_id,
      device_id,
      signing_key: SigningKey::from_bytes(signing_secret.expose_secret()),
      agreement_key: StaticSecret::from(*agreement_secret.expose_secret()),
    }
  }

  /// The secret seeds `(signing, agreement)`, for local secure storage only.
  pub fn to_secrets(&self) -> (SymmetricKey, SymmetricKey) {
    (
      SymmetricKey::from_bytes(self.signing_key.to_bytes()),
      SymmetricKey::from_bytes(self.agreement_key.to_bytes()),
    )
  }

  pub fn user_id(&self) -> UserId {
    self.user_id
  }

  pub fn device_id(&self) -> DeviceId {
    self.device_id
  }

  pub fn public_keys(&self) -> DevicePublicKeys {
    DevicePublicKeys {
      device_id: self.device_id,
      signing_key: DeviceSigningPublicKey(self.signing_key.verifying_key().to_bytes()),
      agreement_key: DeviceAgreementPublicKey(X25519PublicKey::from(&self.agreement_key).to_bytes()),
    }
  }

  /// X25519 Diffie-Hellman with another device, for key transfers (e2ee.md §5). The raw shared
  /// secret must go through a KDF before use.
  pub fn agree(&self, peer: &DeviceAgreementPublicKey) -> SymmetricKey {
    let shared = self.agreement_key.diffie_hellman(&X25519PublicKey::from(peer.0));
    SymmetricKey::from_bytes(shared.to_bytes())
  }

  /// Signs `len(label) || label || body` with the device key.
  pub(crate) fn sign_labeled(&self, label: &str, body: &[u8]) -> Ed25519Signature {
    Ed25519Signature(self.signing_key.sign(&domain::labeled(label, body)).to_bytes())
  }
}

/// The device key signs MLS handshake and application messages as the device's MLS credential.
impl MlsSigner for DeviceKeyPair {
  fn sign(&self, payload: &[u8]) -> Result<Vec<u8>, MlsSignerError> {
    Ok(self.signing_key.sign(payload).to_bytes().to_vec())
  }

  fn signature_scheme(&self) -> SignatureScheme {
    SignatureScheme::ED25519
  }
}

impl fmt::Debug for DeviceKeyPair {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("DeviceKeyPair")
      .field("user_id", &self.user_id)
      .field("device_id", &self.device_id)
      .finish_non_exhaustive()
  }
}

impl DeviceSigningPublicKey {
  pub fn from_bytes(bytes: [u8; 32]) -> Self {
    Self(bytes)
  }

  pub fn as_bytes(&self) -> &[u8; 32] {
    &self.0
  }

  /// Verifies a signature over `len(label) || label || body`.
  pub(crate) fn verify_labeled(
    &self,
    label: &str,
    body: &[u8],
    signature: &Ed25519Signature,
  ) -> Result<(), IdentityError> {
    let key = VerifyingKey::from_bytes(&self.0).map_err(|_| IdentityError::InvalidPublicKey)?;
    let signature = ed25519_dalek::Signature::from_bytes(&signature.0);
    key
      .verify_strict(&domain::labeled(label, body), &signature)
      .map_err(|_| IdentityError::InvalidSignature)
  }
}

impl ConstantTimeEq for DeviceSigningPublicKey {
  fn ct_eq(&self, other: &Self) -> subtle::Choice {
    self.0.ct_eq(&other.0)
  }
}

impl fmt::Debug for DeviceSigningPublicKey {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "DeviceSigningPublicKey({})", hex(&self.0))
  }
}

impl DeviceAgreementPublicKey {
  pub fn from_bytes(bytes: [u8; 32]) -> Self {
    Self(bytes)
  }

  pub fn as_bytes(&self) -> &[u8; 32] {
    &self.0
  }
}

impl fmt::Debug for DeviceAgreementPublicKey {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "DeviceAgreementPublicKey({})", hex(&self.0))
  }
}

impl DeviceCertificate {
  fn signing_input(&self) -> Result<Vec<u8>, IdentityError> {
    Ok(domain::labeled(
      domain::DEVICE_CERTIFICATE,
      &self.tls_serialize_detached()?,
    ))
  }
}

impl SignedDeviceCertificate {
  /// Decodes a certificate received from the server. Decoding does not verify it.
  pub fn from_bytes(bytes: &[u8]) -> Result<Self, IdentityError> {
    Ok(Self::tls_deserialize_exact_bytes(bytes)?)
  }

  pub fn to_bytes(&self) -> Result<Vec<u8>, IdentityError> {
    Ok(self.tls_serialize_detached()?)
  }

  /// The claimed, not yet verified, certificate content. Use it only to look up the pinned
  /// identity key for [`Self::verify`].
  pub fn unverified(&self) -> &DeviceCertificate {
    &self.certificate
  }

  /// Checks that the certificate was signed by `pinned_identity`, the identity key this client
  /// pinned for the certificate's user.
  pub fn verify(&self, pinned_identity: &IdentityPublicKey) -> Result<VerifiedDevice, IdentityError> {
    let certificate = &self.certificate;
    if certificate.version != DEVICE_CERTIFICATE_VERSION {
      return Err(IdentityError::UnsupportedVersion(certificate.version));
    }
    if !bool::from(certificate.identity_key.ct_eq(pinned_identity)) {
      return Err(IdentityError::IdentityKeyMismatch);
    }
    let signature = ed25519_dalek::Signature::from_bytes(&self.signature.0);
    pinned_identity
      .verifying_key()?
      .verify_strict(&certificate.signing_input()?, &signature)
      .map_err(|_| IdentityError::InvalidSignature)?;
    Ok(VerifiedDevice {
      certificate: certificate.clone(),
    })
  }
}

impl VerifiedDevice {
  pub fn user_id(&self) -> UserId {
    self.certificate.user_id
  }

  pub fn device_id(&self) -> DeviceId {
    self.certificate.device_id
  }

  pub fn signing_key(&self) -> &DeviceSigningPublicKey {
    &self.certificate.signing_key
  }

  pub fn agreement_key(&self) -> &DeviceAgreementPublicKey {
    &self.certificate.agreement_key
  }

  /// Fails unless `presented` is the device key this certificate vouches for.
  pub fn ensure_signing_key(&self, presented: &[u8]) -> Result<(), IdentityError> {
    if bool::from(self.certificate.signing_key.0.as_slice().ct_eq(presented)) {
      Ok(())
    } else {
      Err(IdentityError::DeviceKeyMismatch)
    }
  }
}

impl Ed25519Signature {
  pub fn from_bytes(bytes: [u8; 64]) -> Self {
    Self(bytes)
  }

  pub fn as_bytes(&self) -> &[u8; 64] {
    &self.0
  }
}

impl fmt::Debug for Ed25519Signature {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "Ed25519Signature({})", hex(&self.0))
  }
}

/// Lower-case hex of public data for `Debug` and fingerprints. Never used for secrets.
fn hex(bytes: &[u8]) -> String {
  bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;

  use super::*;

  const ALICE: UserId = UserId::new(1);
  const MALLORY: UserId = UserId::new(2);
  const LAPTOP: DeviceId = DeviceId::new(10);

  #[test]
  fn certificate_round_trip_verifies() {
    let identity = IdentityKeyPair::generate(&mut OsRng);
    let device = DeviceKeyPair::generate(ALICE, LAPTOP, &mut OsRng);
    let signed = identity.certify_device(ALICE, &device.public_keys()).unwrap();

    let decoded = SignedDeviceCertificate::from_bytes(&signed.to_bytes().unwrap()).unwrap();
    let verified = decoded.verify(&identity.public_key()).unwrap();

    assert_eq!(verified.user_id(), ALICE);
    assert_eq!(verified.device_id(), LAPTOP);
    assert_eq!(verified.signing_key(), &device.public_keys().signing_key);
    verified
      .ensure_signing_key(device.public_keys().signing_key.as_bytes())
      .unwrap();
  }

  #[test]
  fn identity_secret_round_trip() {
    let identity = IdentityKeyPair::generate(&mut OsRng);
    let restored = IdentityKeyPair::from_secret(&identity.to_secret());
    assert_eq!(restored.public_key(), identity.public_key());
  }

  #[test]
  fn device_secret_round_trip_and_agreement() {
    let alice = DeviceKeyPair::generate(ALICE, LAPTOP, &mut OsRng);
    let (signing, agreement) = alice.to_secrets();
    let restored = DeviceKeyPair::from_secrets(ALICE, LAPTOP, &signing, &agreement);
    assert_eq!(restored.public_keys(), alice.public_keys());

    let bob = DeviceKeyPair::generate(MALLORY, DeviceId::new(11), &mut OsRng);
    let ab = alice.agree(&bob.public_keys().agreement_key);
    let ba = bob.agree(&alice.public_keys().agreement_key);
    assert!(bool::from(ab.ct_eq(&ba)));
  }

  #[test]
  fn fingerprint_is_stable_and_distinct() {
    let first = IdentityKeyPair::generate(&mut OsRng).public_key();
    let second = IdentityKeyPair::generate(&mut OsRng).public_key();
    assert_eq!(first.fingerprint(), first.fingerprint());
    assert_ne!(first.fingerprint(), second.fingerprint());
    assert_eq!(first.fingerprint().to_string().len(), 64);
  }

  #[test]
  fn debug_does_not_leak_secrets() {
    let identity = IdentityKeyPair::generate(&mut OsRng);
    let secret_hex = hex(identity.to_secret().expose_secret());
    assert!(!format!("{identity:?}").contains(&secret_hex));
    let device = DeviceKeyPair::generate(ALICE, LAPTOP, &mut OsRng);
    let (signing, _) = device.to_secrets();
    assert!(!format!("{device:?}").contains(&hex(signing.expose_secret())));
  }

  mod rejection {
    use super::*;

    #[test]
    fn rejects_certificate_from_other_identity() {
      let identity = IdentityKeyPair::generate(&mut OsRng);
      let attacker = IdentityKeyPair::generate(&mut OsRng);
      let device = DeviceKeyPair::generate(ALICE, LAPTOP, &mut OsRng);
      let forged = attacker.certify_device(ALICE, &device.public_keys()).unwrap();

      assert!(matches!(
        forged.verify(&identity.public_key()),
        Err(IdentityError::IdentityKeyMismatch)
      ));
    }

    #[test]
    fn rejects_certificate_with_swapped_identity_field() {
      // An attacker signs with its own key but claims the victim's identity key in the certificate.
      let identity = IdentityKeyPair::generate(&mut OsRng);
      let attacker = IdentityKeyPair::generate(&mut OsRng);
      let device = DeviceKeyPair::generate(ALICE, LAPTOP, &mut OsRng);
      let mut forged = attacker.certify_device(ALICE, &device.public_keys()).unwrap();
      forged.certificate.identity_key = identity.public_key();

      assert!(matches!(
        forged.verify(&identity.public_key()),
        Err(IdentityError::InvalidSignature)
      ));
    }

    #[test]
    fn rejects_tampered_certificate_fields() {
      let identity = IdentityKeyPair::generate(&mut OsRng);
      let device = DeviceKeyPair::generate(ALICE, LAPTOP, &mut OsRng);
      let other_device = DeviceKeyPair::generate(ALICE, LAPTOP, &mut OsRng);
      let signed = identity.certify_device(ALICE, &device.public_keys()).unwrap();

      let mut wrong_user = signed.clone();
      wrong_user.certificate.user_id = MALLORY;
      let mut wrong_key = signed.clone();
      wrong_key.certificate.signing_key = other_device.public_keys().signing_key;
      let mut wrong_signature = signed.clone();
      wrong_signature.signature.0[0] ^= 1;

      for tampered in [wrong_user, wrong_key, wrong_signature] {
        assert!(matches!(
          tampered.verify(&identity.public_key()),
          Err(IdentityError::InvalidSignature)
        ));
      }
    }

    #[test]
    fn rejects_unknown_version() {
      let identity = IdentityKeyPair::generate(&mut OsRng);
      let device = DeviceKeyPair::generate(ALICE, LAPTOP, &mut OsRng);
      let mut signed = identity.certify_device(ALICE, &device.public_keys()).unwrap();
      signed.certificate.version = 2;
      assert!(matches!(
        signed.verify(&identity.public_key()),
        Err(IdentityError::UnsupportedVersion(2))
      ));
    }

    #[test]
    fn rejects_other_device_key() {
      let identity = IdentityKeyPair::generate(&mut OsRng);
      let device = DeviceKeyPair::generate(ALICE, LAPTOP, &mut OsRng);
      let other = DeviceKeyPair::generate(ALICE, LAPTOP, &mut OsRng);
      let verified = identity
        .certify_device(ALICE, &device.public_keys())
        .unwrap()
        .verify(&identity.public_key())
        .unwrap();
      assert!(matches!(
        verified.ensure_signing_key(other.public_keys().signing_key.as_bytes()),
        Err(IdentityError::DeviceKeyMismatch)
      ));
    }

    #[test]
    fn rejects_malformed_bytes() {
      assert!(SignedDeviceCertificate::from_bytes(&[]).is_err());
      assert!(SignedDeviceCertificate::from_bytes(&[1, 2, 3]).is_err());
      let identity = IdentityKeyPair::generate(&mut OsRng);
      let device = DeviceKeyPair::generate(ALICE, LAPTOP, &mut OsRng);
      let mut bytes = identity
        .certify_device(ALICE, &device.public_keys())
        .unwrap()
        .to_bytes()
        .unwrap();
      bytes.pop();
      assert!(SignedDeviceCertificate::from_bytes(&bytes).is_err());
      bytes.extend_from_slice(&[0, 0]);
      assert!(SignedDeviceCertificate::from_bytes(&bytes).is_err());
    }
  }
}
