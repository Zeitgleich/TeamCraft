//! Errors of the client core.

use teamcraft_crypto::backup::BackupError;
use teamcraft_crypto::content_key::ContentKeyError;
use teamcraft_crypto::identity::IdentityError;
use teamcraft_crypto::key_bag::KeyBagError;
use teamcraft_crypto::membership::MembershipError;
use teamcraft_crypto::message::MessageError;
use teamcraft_crypto::mls::MlsError;
use teamcraft_proto::{ConversationId, UserId};

use crate::api::ApiError;
use crate::store::StoreError;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
  #[error("local store: {0}")]
  Store(#[from] StoreError),
  #[error("server: {0}")]
  Api(#[from] ApiError),
  #[error("MLS: {0}")]
  Mls(#[from] MlsError),
  #[error("message encryption: {0}")]
  Message(#[from] MessageError),
  #[error("identity: {0}")]
  Identity(#[from] IdentityError),
  #[error("backup key: {0}")]
  Backup(#[from] BackupError),
  #[error("key bag: {0}")]
  KeyBag(#[from] KeyBagError),
  #[error("content keys: {0}")]
  ContentKey(#[from] ContentKeyError),
  #[error("membership change: {0}")]
  Membership(#[from] MembershipError),
  #[error("this device is not enrolled yet")]
  NotEnrolled,
  #[error("the account already has an identity key; recover it instead of enrolling anew")]
  AlreadyEnrolled,
  #[error("the server's identity key for this account doesn't match the recovered key bag")]
  IdentityMismatch,
  #[error("user {0} has no identity key yet (never enrolled)")]
  UnknownIdentity(UserId),
  #[error("conversation {0} has no MLS group on this device")]
  NoGroup(ConversationId),
  #[error("conversation {0} has no content key on this device yet")]
  NoContentKey(ConversationId),
  #[error("gave up after {attempts} attempts: {reason}")]
  GaveUp { attempts: u32, reason: String },
  #[error("gateway: {0}")]
  Gateway(String),
  #[error("local data is unreadable: {0}")]
  Corrupt(String),
}

impl CoreError {
  /// Worth retrying later without changes (offline, server overloaded).
  pub fn is_transient(&self) -> bool {
    matches!(self, Self::Api(error) if error.is_transient())
  }
}
