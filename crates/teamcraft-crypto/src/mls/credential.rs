//! MLS credentials carry the device certificate.
//!
//! A device's MLS credential is a `BasicCredential` whose identity is the device's
//! [`SignedDeviceCertificate`], and its MLS signature key is the device's Ed25519 key. Any member
//! can therefore check a leaf on its own: decode the certificate, verify it against the pinned
//! identity key of the claimed user, and check that it vouches for the leaf's signature key. The
//! server can't add a device that the user's identity key didn't sign (e2ee.md §3).

use openmls::prelude::{BasicCredential, Credential, CredentialType, CredentialWithKey};

use super::{GroupMember, MlsError};
use crate::identity::{SignedDeviceCertificate, VerifiedDevice};
use crate::membership::IdentityDirectory;

pub(super) fn credential_with_key(
  certificate: &SignedDeviceCertificate,
) -> Result<CredentialWithKey, MlsError> {
  let credential = BasicCredential::new(certificate.to_bytes().map_err(MlsError::InvalidCredential)?);
  let signature_key = certificate.unverified().signing_key.as_bytes().to_vec();
  Ok(CredentialWithKey {
    credential: credential.into(),
    signature_key: signature_key.into(),
  })
}

/// Verifies a leaf's credential and signature key against the pinned identity of its user.
pub(super) fn verify(
  credential: &Credential,
  signature_key: &[u8],
  directory: &impl IdentityDirectory,
) -> Result<VerifiedDevice, MlsError> {
  let certificate = decode(credential)?;
  let user_id = certificate.unverified().user_id;
  let pinned = directory
    .pinned_identity(user_id)
    .ok_or(MlsError::UnknownUser(user_id))?;
  let device = certificate.verify(&pinned).map_err(MlsError::InvalidCredential)?;
  device
    .ensure_signing_key(signature_key)
    .map_err(MlsError::InvalidCredential)?;
  Ok(device)
}

/// Reads who a leaf belongs to without verifying it. Only for leaves already in the group, which
/// were verified when they were added.
pub(super) fn member_of(credential: &Credential) -> Result<GroupMember, MlsError> {
  let claimed = decode(credential)?;
  let certificate = claimed.unverified();
  Ok(GroupMember {
    user_id: certificate.user_id,
    device_id: certificate.device_id,
  })
}

fn decode(credential: &Credential) -> Result<SignedDeviceCertificate, MlsError> {
  if credential.credential_type() != CredentialType::Basic {
    return Err(MlsError::UnsupportedCredential);
  }
  SignedDeviceCertificate::from_bytes(credential.serialized_content())
    .map_err(|_| MlsError::UnsupportedCredential)
}
