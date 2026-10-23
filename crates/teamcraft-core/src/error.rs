//! Errors of the client core.

use teamcraft_crypto::backup::BackupError;
use teamcraft_crypto::content_key::ContentKeyError;
use teamcraft_crypto::identity::IdentityError;
use teamcraft_crypto::key_bag::KeyBagError;
use teamcraft_crypto::membership::MembershipError;
use teamcraft_crypto::message::MessageError;
use teamcraft_crypto::mls::MlsError;
use teamcraft_proto::{ConversationId, DeviceId, UserId};

use crate::api::ApiError;
use crate::store::StoreError;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
  #[error("local store: {0}")]
  Store(#[from] StoreError),
  #[error("server: {0}")]
  Api(#[from] ApiError),
  #[error("push gateway: {0}")]
  PushGateway(String),
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
  #[error("attachment: {0}")]
  Attachment(#[from] teamcraft_crypto::attachment::AttachmentError),
  #[error("workspace log: {0}")]
  WorkspaceLog(#[from] teamcraft_crypto::workspace::WorkspaceLogError),
  #[error("membership change: {0}")]
  Membership(#[from] MembershipError),
  #[error("device list: {0}")]
  DeviceList(#[from] teamcraft_crypto::device_list::DeviceListError),
  #[error("this device is not enrolled yet")]
  NotEnrolled,
  #[error("the account already has an identity key; recover it instead of enrolling anew")]
  AlreadyEnrolled,
  #[error("the server's identity key for this account doesn't match the recovered key bag")]
  IdentityMismatch,
  /// A linked or recovering device was told the account has no identity: it never creates one
  /// (security review B1).
  #[error("the server reports no identity key for this account, but this device expects one")]
  IdentityMissing,
  /// The server has no key bag for an enrolled account: uploading one now would bind it to no
  /// recovery factor, so the user sets a recovery passphrase again instead.
  #[error("the server has no key bag for this account; set a recovery passphrase again")]
  KeyBagMissing,
  /// The two devices speak different versions of the device-linking protocol (e2ee.md §5).
  #[error("the other device uses device linking version {theirs}, this one {ours}; update both apps")]
  LinkVersionMismatch { ours: u32, theirs: u32 },
  /// The new device never confirmed the transfer (a lost frame, a closed relay or a timeout).
  #[error("the new device did not confirm that it received the keys; link it again")]
  LinkNotConfirmed,
  /// The new device received the transfer but could not open the key bag in it.
  #[error("the new device could not open the keys it received; link it again")]
  LinkRejected,
  /// The server gave this device the ID of another (or a revoked) device of the user.
  #[error("the server assigned device ID {0}, which the signed device list gives another device")]
  DeviceIdTaken(DeviceId),
  /// The server offered a key bag older than one this device already saw.
  #[error("the server offered key bag {offered}, older than {seen} seen before")]
  KeyBagRollback { seen: u64, offered: u64 },
  #[error("no verified key yet for device {device} of user {user}")]
  UnknownSender { user: UserId, device: DeviceId },
  /// The policy an admin edited is not the one this device verified: the server showed something
  /// else, or a newer policy arrived. Nothing is signed.
  #[error("the policy shown differs from the policy this device verified; reload the console and review it")]
  PolicyBaseMismatch,
  #[error("user {0} isn't an admin in the signed roster")]
  NotAnAdmin(UserId),
  #[error("user {0} has no identity key yet (never enrolled)")]
  UnknownIdentity(UserId),
  #[error("conversation {0} has no MLS group on this device")]
  NoGroup(ConversationId),
  #[error("conversation {0} has no content key on this device yet")]
  NoContentKey(ConversationId),
  #[error("gave up after {attempts} attempts: {reason}")]
  GaveUp { attempts: u32, reason: String },
  /// The server's answer to this device's last commit is still unknown (lost on the way); no new
  /// commit until it is resolved (e2ee.md §4.6).
  #[error("the outcome of this device's last commit in conversation {0} is not known yet")]
  CommitPending(ConversationId),
  #[error("gateway: {0}")]
  Gateway(String),
  /// A browser API failed or is missing (no service worker, permission denied, ...).
  #[error("browser: {0}")]
  Browser(String),
  /// The app asked for something that can't be done as asked (a caller bug or stale UI state).
  #[error("invalid request: {0}")]
  InvalidRequest(String),
  #[error("local data is unreadable: {0}")]
  Corrupt(String),
}

impl CoreError {
  /// Worth retrying later without changes (offline, server overloaded).
  pub fn is_transient(&self) -> bool {
    match self {
      Self::Api(error) => error.is_transient(),
      Self::CommitPending(_) => true,
      _ => false,
    }
  }

  /// See [`ApiError::is_retry_later`].
  pub fn is_retry_later(&self) -> bool {
    matches!(self, Self::Api(error) if error.is_retry_later())
  }
}
