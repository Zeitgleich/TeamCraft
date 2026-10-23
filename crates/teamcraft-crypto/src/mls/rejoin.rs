//! A device's signed request to be added back to a group it fell out of (e2ee.md §4.6).
//!
//! A device that can no longer follow its group (it missed or refused a commit the others merged)
//! can't commit itself. It signs a [`RejoinRequest`] with its device key; a member who verifies it
//! commits "remove the device's old leaf, add the same device from a fresh key package" in one
//! commit that carries the request. Membership doesn't change, so no admin signature is needed, but
//! the device's own signature is: nobody else can make a device give up its leaf.

use rand_core::CryptoRngCore;
use tls_codec::{DeserializeBytes as _, Serialize as _, TlsDeserializeBytes, TlsSerialize, TlsSize};

use super::{GroupMember, MlsError};
use crate::domain;
use crate::identity::{DeviceKeyPair, Ed25519Signature, SignedDeviceCertificate, VerifiedDevice};
use crate::ids::ConversationId;
use crate::membership::{ChangeId, IdentityDirectory, MembershipError};

const REJOIN_REQUEST_VERSION: u8 = 1;

/// "Add this device back to the group": what the device asks for, unsigned.
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct RejoinRequest {
  version: u8,
  /// Random, so a member commits each request at most once.
  request_id: ChangeId,
  conversation_id: ConversationId,
  /// The group's epoch on the server when the device asked. A request is only committed up to
  /// [`super::MAX_CHANGE_AGE_EPOCHS`] later, so an old one can't be replayed.
  epoch: u64,
  device: GroupMember,
}

/// A [`RejoinRequest`] with the device's certificate and signature.
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct SignedRejoinRequest {
  request: RejoinRequest,
  signer: SignedDeviceCertificate,
  signature: Ed25519Signature,
}

/// A request whose signature and signer were checked against the pinned identities.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedRejoinRequest {
  request: RejoinRequest,
  device: VerifiedDevice,
}

impl RejoinRequest {
  pub fn new(
    conversation_id: ConversationId,
    epoch: u64,
    device: GroupMember,
    rng: &mut impl CryptoRngCore,
  ) -> Self {
    Self {
      version: REJOIN_REQUEST_VERSION,
      request_id: ChangeId::generate(rng),
      conversation_id,
      epoch,
      device,
    }
  }

  pub fn request_id(&self) -> ChangeId {
    self.request_id
  }

  pub fn conversation_id(&self) -> ConversationId {
    self.conversation_id
  }

  pub fn epoch(&self) -> u64 {
    self.epoch
  }

  /// The device that wants back in.
  pub fn device(&self) -> GroupMember {
    self.device
  }

  /// Signs the request with the device's own key; only the device itself can ask.
  pub fn sign(
    self,
    device: &DeviceKeyPair,
    certificate: SignedDeviceCertificate,
  ) -> Result<SignedRejoinRequest, MlsError> {
    let claimed = certificate.unverified();
    let own = device.public_keys();
    if claimed.user_id != device.user_id()
      || claimed.device_id != own.device_id
      || claimed.signing_key != own.signing_key
      || self.device.user_id != claimed.user_id
      || self.device.device_id != claimed.device_id
    {
      return Err(MembershipError::CertificateMismatch.into());
    }
    let signature = device.sign_labeled(domain::REJOIN_REQUEST, &self.tls_serialize_detached()?);
    Ok(SignedRejoinRequest {
      request: self,
      signer: certificate,
      signature,
    })
  }
}

impl SignedRejoinRequest {
  pub fn to_bytes(&self) -> Result<Vec<u8>, MlsError> {
    Ok(self.tls_serialize_detached()?)
  }

  pub fn from_bytes(bytes: &[u8]) -> Result<Self, MlsError> {
    Ok(Self::tls_deserialize_exact_bytes(bytes)?)
  }

  /// The claimed, unverified request. Don't act on it before [`Self::verify`].
  pub fn unverified(&self) -> &RejoinRequest {
    &self.request
  }

  /// Checks the signer's certificate against its pinned identity key and the device signature;
  /// the signer must be the device the request is about.
  pub fn verify(&self, directory: &impl IdentityDirectory) -> Result<VerifiedRejoinRequest, MlsError> {
    if self.request.version != REJOIN_REQUEST_VERSION {
      return Err(MembershipError::UnsupportedVersion(self.request.version).into());
    }
    let user = self.signer.unverified().user_id;
    let pinned = directory
      .pinned_identity(user)
      .ok_or(MlsError::UnknownUser(user))?;
    let device = self.signer.verify(&pinned).map_err(MlsError::InvalidCredential)?;
    if GroupMember::from(&device) != self.request.device {
      return Err(MembershipError::CertificateMismatch.into());
    }
    device
      .signing_key()
      .verify_labeled(
        domain::REJOIN_REQUEST,
        &self.request.tls_serialize_detached()?,
        &self.signature,
      )
      .map_err(|_| MlsError::Membership(MembershipError::InvalidSignature))?;
    Ok(VerifiedRejoinRequest {
      request: self.request.clone(),
      device,
    })
  }
}

impl VerifiedRejoinRequest {
  pub fn request(&self) -> &RejoinRequest {
    &self.request
  }

  pub fn device(&self) -> &VerifiedDevice {
    &self.device
  }
}
