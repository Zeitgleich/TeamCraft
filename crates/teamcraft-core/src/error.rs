//! Errors of the client core.

use teamcraft_crypto::backup::BackupError;
use teamcraft_crypto::content_key::ContentKeyError;
use teamcraft_crypto::identity::IdentityError;
use teamcraft_crypto::key_bag::KeyBagError;
use teamcraft_crypto::membership::MembershipError;
use teamcraft_crypto::message::MessageError;
use teamcraft_crypto::mls::MlsError;
use teamcraft_proto::{ConversationId, DeviceId, UserId};

#[cfg(feature = "ui")]
use teamcraft_client_api::failure::{ActionFailure, CallFailure, FailureKind};

use crate::api::ApiError;
#[cfg(feature = "ui")]
use crate::api::Interception;
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
  /// The link request is gone: another device took it up, the new device gave up, or it expired.
  #[error("this device link request is no longer open")]
  LinkRequestGone,
  /// A device of the user denied this device's link request.
  #[error("a device of this account denied the request to link this device")]
  LinkDenied,
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
  /// The roster change would leave nobody who can sign the next one; nothing is signed
  /// (admin.md §2.3). The message starts with the server's stable `last_owner` error code, which
  /// the UI localizes.
  #[error(
    "last_owner: the roster must keep at least one owner; add the new owner before removing the last one"
  )]
  LastOwner,
  /// The roster changed since the editor loaded it (another owner signed); nothing is signed. The
  /// message starts with a stable code the UI localizes.
  #[error("roster_changed: another owner changed the roster meanwhile; review the new one and edit it again")]
  RosterBaseMismatch,
  /// Only an owner of the verified roster signs the next roster or policy; nothing is signed. The
  /// message starts with a stable code the UI localizes.
  #[error("not_an_owner: only owners of the signed roster can sign it; this user is not one")]
  NotAnOwner,
  #[error("user {0} isn't an admin in the signed roster")]
  NotAnAdmin(UserId),
  #[error("user {0} has no identity key yet (never enrolled)")]
  UnknownIdentity(UserId),
  #[error("conversation {0} has no MLS group on this device")]
  NoGroup(ConversationId),
  /// A call needs this device in the conversation's MLS group (e2ee.md §8), and it isn't yet:
  /// the welcome hasn't come, or another device of the user or a member has to let it in. Its
  /// failure kind is `CallFailure::EncryptionPending` ([`CoreError::to_failure`]).
  #[error("this device is not in the encryption group of conversation {0} yet")]
  CallEncryptionPending(ConversationId),
  #[error("conversation {0} is not a meeting chat")]
  NotAMeetingChat(ConversationId),
  /// The call of `from` should move into `to`, but that doesn't check out: no member of `from`'s
  /// group announced it, or `to`'s group holds people the announcement didn't name.
  #[error("moving the call of {from} into {to} refused: {reason}")]
  CallMoveRefused {
    from: ConversationId,
    to: ConversationId,
    reason: &'static str,
  },
  #[error("conversation {0} has no content key on this device yet")]
  NoContentKey(ConversationId),
  #[error("gave up after {attempts} attempts: {reason}")]
  GaveUp { attempts: u32, reason: String },
  /// The server's answer to this device's last commit is still unknown (lost on the way); no new
  /// commit until it is resolved (e2ee.md §4.6).
  #[error("the outcome of this device's last commit in conversation {0} is not known yet")]
  CommitPending(ConversationId),
  /// A leave waits until its goodbye reached the server: once the removal is committed, the
  /// leaver can't write in the conversation any more (e2ee.md §4.6).
  #[error("the goodbye in conversation {0} hasn't reached the server yet")]
  GoodbyePending(ConversationId),
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

/// Why the local data can't be opened with this version, now or after a restart (issue #160).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnreadableStore {
  /// The file is damaged, or its key doesn't open it (the keystore lost or replaced it).
  Damaged,
  /// A newer version of the app changed its layout.
  NewerVersion,
}

impl CoreError {
  /// Whether opening the local data failed in a way a retry won't fix: the app asks the user to
  /// set the device up again (or to update), never just "failed" (issue #160). Only meaningful
  /// for errors of opening the store and the client on it.
  pub fn unreadable_store(&self) -> Option<UnreadableStore> {
    match self {
      Self::Store(StoreError::NewerSchema { .. }) => Some(UnreadableStore::NewerVersion),
      Self::Store(error) if error.is_unreadable() => Some(UnreadableStore::Damaged),
      // Records that don't decode, or an account whose key bag doesn't open with its own key.
      Self::Corrupt(_) | Self::KeyBag(_) | Self::Identity(_) => Some(UnreadableStore::Damaged),
      _ => None,
    }
  }

  /// Worth retrying later without changes (offline, server overloaded).
  pub fn is_transient(&self) -> bool {
    match self {
      Self::Api(error) => error.is_transient(),
      Self::CommitPending(_) | Self::GoodbyePending(_) => true,
      _ => false,
    }
  }

  /// See [`ApiError::is_retry_later`].
  pub fn is_retry_later(&self) -> bool {
    matches!(self, Self::Api(error) if error.is_retry_later())
  }

  /// See [`ApiError::is_sign_in_required`].
  pub fn is_sign_in_required(&self) -> bool {
    matches!(self, Self::Api(error) if error.is_sign_in_required())
  }

  /// The failure as the UI words it (issue #322): the server's error code, an unreachable server,
  /// or anything else with this error's text as the detail.
  #[cfg(feature = "ui")]
  pub fn to_failure(&self) -> ActionFailure {
    let kind = match self {
      Self::Api(ApiError::Network(_)) => FailureKind::Network,
      Self::Api(ApiError::Intercepted { kind, .. }) => match kind {
        Interception::CaptivePortal => FailureKind::CaptivePortal,
        Interception::UntrustedCertificate => FailureKind::UntrustedCertificate,
      },
      Self::Api(error) => error
        .error_code()
        .map_or(FailureKind::Other, |code| FailureKind::Server(code.clone())),
      Self::CallEncryptionPending(_) => FailureKind::Call(CallFailure::EncryptionPending),
      Self::CallMoveRefused { .. } => FailureKind::Call(CallFailure::MoveRefused),
      _ => FailureKind::Other,
    };
    ActionFailure::new(kind, self.to_string())
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::ErrorCode;

  use super::*;

  #[test]
  fn the_non_owner_refusal_carries_its_stable_code() {
    assert!(CoreError::NotAnOwner.to_string().starts_with("not_an_owner:"));
    assert!(
      CoreError::RosterBaseMismatch
        .to_string()
        .starts_with("roster_changed:")
    );
  }

  #[cfg(feature = "ui")]
  #[test]
  fn failures_keep_the_server_code_and_tell_network_errors_apart() {
    use teamcraft_client_api::failure::FailureKind;

    let refused = CoreError::Api(ApiError::status(
      400,
      ErrorCode::NotSpaceMember,
      "not in the space",
    ));
    assert_eq!(
      refused.to_failure().kind,
      FailureKind::Server(ErrorCode::NotSpaceMember)
    );
    let offline = CoreError::Api(ApiError::Network("offline".into()));
    assert_eq!(offline.to_failure().kind, FailureKind::Network);
    let portal = CoreError::Api(ApiError::Intercepted {
      kind: crate::api::Interception::CaptivePortal,
      detail: "HTML".into(),
    });
    assert_eq!(portal.to_failure().kind, FailureKind::CaptivePortal);
    assert_eq!(CoreError::NotEnrolled.to_failure().kind, FailureKind::Other);
  }

  #[test]
  fn the_last_owner_refusal_carries_the_stable_code() {
    let text = CoreError::LastOwner.to_string();
    assert!(
      text.starts_with(&format!("{}:", ErrorCode::LastOwner.as_str())),
      "{text}"
    );
  }
}
