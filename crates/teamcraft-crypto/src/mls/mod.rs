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

use openmls::prelude::{
  Capabilities, Ciphersuite, ExtensionType, KeyPackage, KeyPackageIn, Lifetime, ProtocolVersion,
};
use openmls_traits::OpenMlsProvider;
use tls_codec::{Deserialize as _, Serialize as _};

pub use content_keys::ContentKeyBundle;
pub use group::{
  ChangeHistory, CommitOutput, ConversationGroup, GroupEvent, MAX_CHANGE_AGE_EPOCHS, MembershipUpdate,
};

use crate::identity::{DeviceKeyPair, IdentityError, SignedDeviceCertificate, VerifiedDevice};
use crate::ids::{ConversationId, DeviceId, UserId};
use crate::membership::{ChangeId, IdentityDirectory, MembershipError};

/// The only ciphersuite: X25519, ChaCha20-Poly1305, SHA-256, Ed25519 (matches the device keys).
pub const CIPHERSUITE: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519;

/// How long a key package is valid after its creation. With [`KEY_PACKAGE_BACKDATE`] this is the
/// longest range `openmls` accepts (84 days and an hour). Clients replace packages long before
/// (e2ee.md §4.3).
pub const KEY_PACKAGE_LIFETIME_SECS: u64 = 83 * DAY_SECS;
/// How far a key package's validity starts before its creation, so a creator whose clock runs up to
/// a day fast still uploads packages others accept.
pub const KEY_PACKAGE_BACKDATE_SECS: u64 = DAY_SECS;
const DAY_SECS: u64 = 24 * 60 * 60;

/// The default `openmls` provider (RustCrypto primitives, in-memory storage). Clients will plug
/// in persistent storage behind the same `OpenMlsProvider` trait.
pub type DefaultMlsProvider = openmls_rust_crypto::OpenMlsRustCrypto;

/// A member device of a group, as identified by its verified credential.
#[derive(
  Clone,
  Copy,
  Debug,
  PartialEq,
  Eq,
  Hash,
  PartialOrd,
  Ord,
  tls_codec::TlsSerialize,
  tls_codec::TlsDeserializeBytes,
  tls_codec::TlsSize,
)]
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
  #[error("device {0} isn't in its user's signed device list")]
  InactiveDevice(DeviceId),
  #[error("device {0} was removed from this group and can't come back")]
  RemovedDevice(DeviceId),
  #[error("commit adds or removes members without a signed membership change")]
  MissingSignedChange,
  #[error("membership change {0:?} was already applied")]
  ReplayedChange(ChangeId),
  #[error("membership change signed in epoch {signed_epoch} is stale in epoch {current_epoch}")]
  StaleChange { signed_epoch: u64, current_epoch: u64 },
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

/// What the server and the owning device need to know about an uploaded key package, read from the
/// package itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyPackageInfo {
  /// Seconds since the Unix epoch after which nobody accepts the package any more.
  pub not_after_unix_secs: u64,
  pub is_last_resort: bool,
}

/// Creates a key package for this device and stores its private part in the provider's storage.
/// Upload the returned bytes to the server so others can add this device. `now_unix_secs` is the
/// creator's clock (injectable for tests); the package is valid for [`KEY_PACKAGE_LIFETIME_SECS`].
pub fn create_key_package(
  provider: &impl OpenMlsProvider,
  device: &DeviceKeyPair,
  certificate: &SignedDeviceCertificate,
  now_unix_secs: u64,
) -> Result<Vec<u8>, MlsError> {
  let builder = KeyPackage::builder().key_package_lifetime(lifetime_from(now_unix_secs));
  build_key_package(provider, device, certificate, builder)
}

/// Creates the device's last-resort key package (RFC 9420 §16.8): the server hands it out when the
/// regular ones are used up, and its private key survives joining, so it can be used repeatedly.
/// The device replaces it regularly (e2ee.md §4.3).
pub fn create_last_resort_key_package(
  provider: &impl OpenMlsProvider,
  device: &DeviceKeyPair,
  certificate: &SignedDeviceCertificate,
  now_unix_secs: u64,
) -> Result<Vec<u8>, MlsError> {
  // The leaf must announce support for the last_resort extension the key package carries.
  let capabilities = Capabilities::new(None, None, Some(&[ExtensionType::LastResort]), None, None);
  let builder = KeyPackage::builder()
    .leaf_node_capabilities(capabilities)
    .key_package_lifetime(lifetime_from(now_unix_secs))
    .mark_as_last_resort();
  build_key_package(provider, device, certificate, builder)
}

/// Checks an uploaded key package (encoding, signature, ciphersuite, lifetime against this
/// machine's clock) and reads its expiry. The server records the expiry so it never hands out an
/// expired package; it does not check the credential, which members do when they add the device.
pub fn inspect_key_package(bytes: &[u8]) -> Result<KeyPackageInfo, MlsError> {
  let crypto = openmls_rust_crypto::RustCrypto::default();
  let key_package = KeyPackageIn::tls_deserialize_exact(bytes)?
    .validate(&crypto, ProtocolVersion::Mls10)
    .map_err(|error| MlsError::openmls("key package validation", error))?;
  if key_package.ciphersuite() != CIPHERSUITE {
    return Err(MlsError::UnexpectedMessage(
      "key package with another ciphersuite",
    ));
  }
  Ok(KeyPackageInfo {
    not_after_unix_secs: key_package.life_time().not_after(),
    is_last_resort: key_package.last_resort(),
  })
}

fn lifetime_from(now_unix_secs: u64) -> Lifetime {
  Lifetime::init(
    now_unix_secs.saturating_sub(KEY_PACKAGE_BACKDATE_SECS),
    now_unix_secs.saturating_add(KEY_PACKAGE_LIFETIME_SECS),
  )
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
  if !directory.is_active_device(&device) {
    return Err(MlsError::InactiveDevice(device.device_id()));
  }
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
