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
use teamcraft_client_api::failure::{ActionFailure, CallFailure, FailureKind, LinkFailure};

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
  #[error("channel roles: {0}")]
  ChannelRoles(#[from] crate::channel_roles::ChannelRoleError),
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
  /// The verified workspace policy lets members protect their backup only with a recovery kit
  /// (e2ee.md §6.5.8). The message starts with the server's stable code, which the UI localizes.
  #[error("recovery_passphrase_not_allowed: the workspace policy allows only a recovery kit")]
  PassphraseNotAllowed,
  /// The text is not a recovery key (a typo, another format); never echoes the text.
  #[error("recovery key: {0}")]
  RecoveryKeyInvalid(#[from] teamcraft_crypto::recovery_key::RecoveryKeyError),
  /// The two devices speak different versions of the device-linking protocol (e2ee.md §5).
  #[error("the other device uses device linking version {theirs}, this one {ours}; update both apps")]
  LinkVersionMismatch { ours: u32, theirs: u32 },
  /// The scanned or typed text is no device link code (another app's QR code, a cut-off paste).
  #[error("this is not a TeamCraft device link code; scan the code the new device shows")]
  LinkCodeInvalid,
  /// A signed-in device read a code a signed-in device shows for a new phone ("Link a phone"):
  /// the new device has to scan it (issue #998).
  #[error("this code adds a new device; scan it with the new device")]
  LinkCodeForNewDevice,
  /// The new device read a code another new device shows: a signed-in device has to scan it.
  #[error("this code comes from a new device; scan it with a device that is already signed in")]
  LinkCodeForSignedInDevice,
  /// The new device no longer waits for this code (it gave up, went to sleep or the code expired):
  /// joining its relay would only wait for nobody (issue #645).
  #[error("the new device is no longer waiting for this code; show a new code there and scan again")]
  LinkPeerGone,
  /// The other device stopped answering during the handshake.
  #[error("the other device did not answer in time; start linking again")]
  LinkTimedOut,
  /// The code expired before another device took it up; the new device shows a new one.
  #[error("nobody took up this code in time; show a new code")]
  LinkCodeExpired,
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
  /// This device stopped waiting for the new device's receipt (the user closed the dialog, the app
  /// stopped). The keys were sent; whether they arrived is unknown.
  #[error("stopped waiting for the new device to confirm the keys")]
  LinkCancelled,
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
  /// An admin's request to join can't be approved on this device (admin.md §2.4.1): this device's
  /// own state disagrees with the server's request. The message starts with a stable code.
  #[error("join_request_not_approvable: {0}")]
  JoinRequestNotApprovable(JoinRequestRefusal),
  #[error("user {0} has no identity key yet (never enrolled)")]
  UnknownIdentity(UserId),
  /// The signed roster holds this user with an identity key this device can't use, and the new
  /// roster keeps them: their entry can't be signed again as it is, and it is never changed or
  /// dropped silently (issue #765). The message starts with a stable code the UI localizes.
  #[error(
    "roster_key_unusable: the signed roster's identity key of user {0} can't be used on this device; remove them explicitly, or let another owner sign"
  )]
  RosterKeyUnusable(UserId),
  #[error("conversation {0} has no MLS group on this device")]
  NoGroup(ConversationId),
  /// A call needs this device in the conversation's MLS group (e2ee.md §8), and it isn't yet:
  /// the welcome hasn't come, or another device of the user or a member has to let it in. Its
  /// failure kind is `CallFailure::EncryptionPending` ([`CoreError::to_failure`]).
  #[error("this device is not in the encryption group of conversation {0} yet")]
  CallEncryptionPending(ConversationId),
  #[error("conversation {0} is not a meeting chat")]
  NotAMeetingChat(ConversationId),
  /// Letting a guest into a meeting chat's group failed a check of the guest link (e2ee.md §4.6,
  /// "Meeting guests"): nothing was added.
  #[error("the guest cannot be let in: {0}")]
  GuestRefused(GuestRefusal),
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
  /// Writing a file on this device failed (a download into the folder or file the user picked):
  /// the operating system's reason, without the path, which names the file.
  #[error("writing the file on this device failed: {0}")]
  LocalFile(#[source] std::io::Error),
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

/// Why this device doesn't approve an admin's request to join (admin.md §2.4.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum JoinRequestRefusal {
  /// The verified policy doesn't ask for an owner's approval (any more).
  #[error("the verified policy doesn't ask for an owner's approval")]
  PolicyWithoutApproval,
  /// Not a private, unarchived channel this device knows: a DM, a meeting chat, or unknown.
  #[error("only requests to join a private channel are approved")]
  NotAPrivateChannel,
  /// This device's verified channel roles don't make the approving owner an admin of the channel:
  /// members refuse an admin's addition by anybody else (issue #380, e2ee.md §4.6.1). A signed
  /// admin or a workspace admin signs the role first.
  #[error("this user's owner role in the channel isn't signed")]
  OwnerRoleNotSigned,
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
      Self::LinkCodeInvalid => FailureKind::Link(LinkFailure::CodeInvalid),
      Self::LinkCodeForNewDevice => FailureKind::Link(LinkFailure::ForNewDevice),
      Self::LinkCodeForSignedInDevice => FailureKind::Link(LinkFailure::ForSignedInDevice),
      Self::LinkVersionMismatch { .. } => FailureKind::Link(LinkFailure::OtherVersion),
      Self::LinkPeerGone => FailureKind::Link(LinkFailure::PeerGone),
      Self::LinkTimedOut => FailureKind::Link(LinkFailure::TimedOut),
      Self::LinkCodeExpired => FailureKind::Link(LinkFailure::CodeExpired),
      Self::LinkDenied => FailureKind::Link(LinkFailure::Denied),
      Self::LinkNotConfirmed | Self::LinkRejected => FailureKind::Link(LinkFailure::NotConfirmed),
      Self::LinkRequestGone => FailureKind::Link(LinkFailure::RequestGone),
      Self::CallMoveRefused { .. } => FailureKind::Call(CallFailure::MoveRefused),
      Self::PassphraseNotAllowed => {
        FailureKind::Server(teamcraft_proto::ErrorCode::RecoveryPassphraseNotAllowed)
      },
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
    let user = <UserId as teamcraft_proto::Id>::from_snowflake(teamcraft_proto::Snowflake::new(2).unwrap());
    assert!(
      CoreError::RosterKeyUnusable(user)
        .to_string()
        .starts_with("roster_key_unusable:")
    );
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

/// Why a guest was not let into a meeting chat's group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GuestRefusal {
  /// The server offers another identity key for the guest than the one pinned before.
  #[error("the guest's identity key changed while they were let in")]
  KeyChanged,
  /// Only the meeting organizer's devices let guests into the encryption: a guest accepts a
  /// welcome from nobody else (the fingerprint in the link).
  #[error("only the organizer's devices can let guests in")]
  NotOrganizer,
  /// The key the server presents for the guest is not the one their page reported with its device.
  #[error("the guest's identity key is not the one their device reported")]
  NotTheKnockingKey,
  /// The guest has not enrolled a device yet.
  #[error("the guest has no identity key yet")]
  NotEnrolled,
}
