//! MLS (RFC 9420, `openmls`) for key distribution (e2ee.md §4.1, §4.6).
//!
//! Every conversation is one MLS group and every device a member. MLS carries only key material:
//! content key bundles travel as MLS application messages, message bodies are encrypted with the
//! content keys ([`crate::message`]). Membership changes are only committed – and only accepted
//! from other committers – when they carry a valid [`SignedMembershipChange`] in the commit's
//! authenticated data, or are additions of further cross-signed devices of an existing member.
//!
//! [`SignedMembershipChange`]: crate::membership::SignedMembershipChange

mod content_keys;
mod credential;
mod group;

use std::fmt;

use openmls::prelude::{Capabilities, Ciphersuite, ExtensionType, KeyPackage, KeyPackageIn, ProtocolVersion};
use openmls_traits::OpenMlsProvider;
use tls_codec::{Deserialize as _, Serialize as _};

pub use content_keys::ContentKeyBundle;
pub use group::{CommitOutput, ConversationGroup, GroupEvent, MembershipUpdate};

use crate::identity::{DeviceKeyPair, IdentityError, SignedDeviceCertificate, VerifiedDevice};
use crate::ids::{ConversationId, DeviceId, UserId};
use crate::membership::{ChangeId, IdentityDirectory, MembershipError};

/// The only ciphersuite: X25519, ChaCha20-Poly1305, SHA-256, Ed25519 (matches the device keys).
pub const CIPHERSUITE: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519;

/// The default `openmls` provider (RustCrypto primitives, in-memory storage). Clients will plug
/// in persistent storage behind the same `OpenMlsProvider` trait.
pub type DefaultMlsProvider = openmls_rust_crypto::OpenMlsRustCrypto;

/// A member device of a group, as identified by its verified credential.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GroupMember {
  pub user_id: UserId,
  pub device_id: DeviceId,
}

#[derive(Debug, thiserror::Error)]
pub enum MlsError {
  #[error("MLS {operation} failed: {message}")]
  Openmls {
    operation: &'static str,
    message: String,
  },
  #[error("group belongs to conversation {expected}, got {actual}")]
  WrongConversation {
    expected: ConversationId,
    actual: ConversationId,
  },
  #[error("credential is not a TeamCraft device credential")]
  UnsupportedCredential,
  #[error("no pinned identity key for user {0}")]
  UnknownUser(UserId),
  #[error("credential failed verification: {0}")]
  InvalidCredential(IdentityError),
  #[error("membership change rejected: {0}")]
  Membership(#[from] MembershipError),
  #[error("commit adds or removes members without a signed membership change")]
  MissingSignedChange,
  #[error("membership change {0:?} was already applied")]
  ReplayedChange(ChangeId),
  #[error("commit does not match its signed membership change")]
  ChangeDoesNotMatchCommit,
  #[error("commit contains a proposal type TeamCraft does not use")]
  UnsupportedProposal,
  #[error("commit or message from a sender outside the group")]
  ExternalSender,
  #[error("a member's update changes its user or device identity")]
  IdentityChangedInUpdate,
  #[error("unexpected MLS message: {0}")]
  UnexpectedMessage(&'static str),
  #[error("there is no pending commit to confirm")]
  NoPendingCommit,
  #[error("encoding failed: {0}")]
  Encoding(#[from] tls_codec::Error),
}

/// Creates a key package for this device and stores its private part in the provider's storage.
/// Upload the returned bytes to the server so others can add this device.
pub fn create_key_package(
  provider: &impl OpenMlsProvider,
  device: &DeviceKeyPair,
  certificate: &SignedDeviceCertificate,
) -> Result<Vec<u8>, MlsError> {
  build_key_package(provider, device, certificate, KeyPackage::builder())
}

/// Creates the device's last-resort key package (RFC 9420 §16.8): the server hands it out when the
/// regular ones are used up, and its private key survives joining, so it can be used repeatedly.
pub fn create_last_resort_key_package(
  provider: &impl OpenMlsProvider,
  device: &DeviceKeyPair,
  certificate: &SignedDeviceCertificate,
) -> Result<Vec<u8>, MlsError> {
  // The leaf must announce support for the last_resort extension the key package carries.
  let capabilities = Capabilities::new(None, None, Some(&[ExtensionType::LastResort]), None, None);
  let builder = KeyPackage::builder()
    .leaf_node_capabilities(capabilities)
    .mark_as_last_resort();
  build_key_package(provider, device, certificate, builder)
}

fn build_key_package(
  provider: &impl OpenMlsProvider,
  device: &DeviceKeyPair,
  certificate: &SignedDeviceCertificate,
  builder: openmls::prelude::KeyPackageBuilder,
) -> Result<Vec<u8>, MlsError> {
  let credential = credential::credential_with_key(certificate)?;
  let bundle = builder
    .build(CIPHERSUITE, provider, device, credential)
    .map_err(|error| MlsError::openmls("key package creation", error))?;
  Ok(bundle.key_package().tls_serialize_detached()?)
}

/// Decodes, validates (signature, lifetime, ciphersuite) and checks the credential of a key
/// package fetched from the server.
pub(crate) fn validate_key_package(
  provider: &impl OpenMlsProvider,
  bytes: &[u8],
  directory: &impl IdentityDirectory,
) -> Result<(KeyPackage, VerifiedDevice), MlsError> {
  let key_package_in = KeyPackageIn::tls_deserialize_exact(bytes)?;
  let key_package = key_package_in
    .validate(provider.crypto(), ProtocolVersion::Mls10)
    .map_err(|error| MlsError::openmls("key package validation", error))?;
  if key_package.ciphersuite() != CIPHERSUITE {
    return Err(MlsError::UnexpectedMessage(
      "key package with another ciphersuite",
    ));
  }
  let leaf = key_package.leaf_node();
  let device = credential::verify(leaf.credential(), leaf.signature_key().as_slice(), directory)?;
  Ok((key_package, device))
}

impl MlsError {
  fn openmls(operation: &'static str, error: impl fmt::Display) -> Self {
    Self::Openmls {
      operation,
      message: error.to_string(),
    }
  }
}

impl From<&VerifiedDevice> for GroupMember {
  fn from(device: &VerifiedDevice) -> Self {
    Self {
      user_id: device.user_id(),
      device_id: device.device_id(),
    }
  }
}
