//! Errors the app sees. Flat: Kotlin gets the variant and the message; the message is for logs,
//! the app shows its own (translated) text per variant.

use teamcraft_core::CoreError;
use teamcraft_core::api::ApiError;
use teamcraft_core::store::StoreError;
use teamcraft_crypto::backup::BackupError;
use teamcraft_crypto::key_bag::KeyBagError;

#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum TeamCraftError {
  /// The server couldn't be reached; try again later.
  #[error("network: {0}")]
  Network(String),
  /// No valid access token: sign in again.
  #[error("signed out")]
  SignedOut,
  /// This device has no identity yet: enroll or link it first.
  #[error("this device is not enrolled")]
  NotEnrolled,
  /// The recovery passphrase (or other recovery data) doesn't open the key backup: the wrap's
  /// authentication failed. Only that; other backup failures have their own variants.
  #[error("the recovery data doesn't open the key backup: {0}")]
  WrongPassphrase(String),
  /// The device couldn't allocate the memory the backup's key derivation asks for.
  #[error("not enough memory to unlock the key backup: {0}")]
  BackupOutOfMemory(String),
  /// The key backup uses a format version or method this app doesn't know: update the app.
  #[error("the key backup was made by a newer version: {0}")]
  BackupTooNew(String),
  /// The key backup doesn't decode or doesn't belong to this account.
  #[error("the key backup is damaged: {0}")]
  BackupDamaged(String),
  /// The local database couldn't be opened or written (wrong key, disk full).
  #[error("local storage: {0}")]
  Storage(String),
  /// A malformed ID or link code from the app.
  #[error("invalid input: {0}")]
  InvalidInput(String),
  /// A link or recovery step was called out of order.
  #[error("no device link in progress")]
  NoLinkInProgress,
  /// The code typed for a device link is not the one the new device shows; the user may try again.
  #[error("that is not the code on the new device")]
  LinkCodeRejected,
  /// The last try failed: the attempt is over and the new device has to start again.
  #[error("the code did not match; start again on the new device")]
  LinkAttemptEnded,
  /// The scanned or typed text is no TeamCraft link code.
  #[error("not a device link code")]
  LinkCodeInvalid,
  /// A signed-in device scanned the code a computer shows for a new phone: the new phone has to
  /// scan it (issue #998).
  #[error("this code is for the new phone")]
  LinkCodeForNewDevice,
  /// The new phone scanned the code of another new device: a signed-in device has to scan it.
  #[error("this code is for a signed-in device")]
  LinkCodeForSignedInDevice,
  /// The code ran out, or the device that showed it no longer waits: show a new one.
  #[error("the link code expired")]
  LinkCodeExpired,
  /// The admins haven't answered the recovery request yet; try again later.
  #[error("the recovery request isn't answered yet")]
  RecoveryPending,
  /// The engine thread is gone (the app closed the handle).
  #[error("the engine has stopped")]
  EngineStopped,
  /// The call couldn't be joined or broke (connection, audio device, key verification).
  #[error("call: {0}")]
  Call(String),
  /// A move of the call into a meeting room didn't check out (not announced in the DM's group,
  /// or the room holds somebody unnamed): stay in the call.
  #[error("call move refused: {0}")]
  CallMoveRefused(String),
  /// Anything else the core reported.
  #[error("{0}")]
  Core(String),
}

impl From<CoreError> for TeamCraftError {
  fn from(error: CoreError) -> Self {
    match error {
      CoreError::Api(ApiError::Token(_)) | CoreError::Api(ApiError::Status { status: 401, .. }) => {
        Self::SignedOut
      },
      CoreError::Api(api) if api.is_transient() => Self::Network(api.to_string()),
      CoreError::NotEnrolled => Self::NotEnrolled,
      CoreError::Backup(backup) => backup.into(),
      CoreError::KeyBag(key_bag) => key_bag.into(),
      CoreError::Store(store) => store.into(),
      refused @ CoreError::CallMoveRefused { .. } => Self::CallMoveRefused(refused.to_string()),
      CoreError::LinkCodeInvalid => Self::LinkCodeInvalid,
      CoreError::LinkCodeForNewDevice => Self::LinkCodeForNewDevice,
      CoreError::LinkCodeForSignedInDevice => Self::LinkCodeForSignedInDevice,
      CoreError::LinkCodeExpired | CoreError::LinkPeerGone => Self::LinkCodeExpired,
      other => Self::Core(other.to_string()),
    }
  }
}

impl From<BackupError> for TeamCraftError {
  fn from(error: BackupError) -> Self {
    let message = error.to_string();
    match error {
      BackupError::UnwrapFailed | BackupError::WrongMethod { .. } => Self::WrongPassphrase(message),
      BackupError::OutOfMemory { .. } => Self::BackupOutOfMemory(message),
      BackupError::UnsupportedVersion(_) | BackupError::UnsupportedKdf(_) => Self::BackupTooNew(message),
      BackupError::InvalidArgon2Settings(_)
      | BackupError::MalformedList
      | BackupError::TooManyWraps
      | BackupError::Encoding(_) => Self::BackupDamaged(message),
      BackupError::PassphraseTooShort | BackupError::PrfOutputTooShort => Self::InvalidInput(message),
      BackupError::KeyDerivation | BackupError::EncryptionFailed | BackupError::MissingPrivateKey => {
        Self::Core(message)
      },
    }
  }
}

impl From<KeyBagError> for TeamCraftError {
  fn from(error: KeyBagError) -> Self {
    let message = error.to_string();
    match error {
      KeyBagError::UnsupportedVersion(_) => Self::BackupTooNew(message),
      // The backup key behind the bag was already unwrapped (authenticated), so a bag that
      // doesn't open or check out is damage, not a wrong passphrase.
      KeyBagError::DecryptionFailed
      | KeyBagError::WrongUser { .. }
      | KeyBagError::DuplicateConversation(_)
      | KeyBagError::InvalidContentKeys(_)
      | KeyBagError::Encoding(_) => Self::BackupDamaged(message),
      KeyBagError::EncryptionFailed => Self::Core(message),
    }
  }
}

impl From<StoreError> for TeamCraftError {
  fn from(error: StoreError) -> Self {
    Self::Storage(error.to_string())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn core_errors_map_to_what_the_app_acts_on() {
    assert!(matches!(
      TeamCraftError::from(CoreError::Api(ApiError::Token("expired".into()))),
      TeamCraftError::SignedOut
    ));
    assert!(matches!(
      TeamCraftError::from(CoreError::Api(ApiError::Network("offline".into()))),
      TeamCraftError::Network(_)
    ));
    assert!(matches!(
      TeamCraftError::from(CoreError::NotEnrolled),
      TeamCraftError::NotEnrolled
    ));
    assert!(matches!(
      TeamCraftError::from(CoreError::Store(StoreError::WrongKey)),
      TeamCraftError::Storage(_)
    ));
    assert!(matches!(
      TeamCraftError::from(CoreError::Gateway("closed".into())),
      TeamCraftError::Core(_)
    ));
  }

  mod backup_errors {
    use super::*;

    #[test]
    fn only_a_failed_unwrap_is_a_wrong_passphrase() {
      assert!(matches!(
        backup(BackupError::UnwrapFailed),
        TeamCraftError::WrongPassphrase(_)
      ));
      assert!(matches!(
        TeamCraftError::from(CoreError::KeyBag(KeyBagError::DecryptionFailed)),
        TeamCraftError::BackupDamaged(_)
      ));
    }

    #[test]
    fn missing_memory_is_its_own_error() {
      let error = backup(BackupError::OutOfMemory { memory_kib: 262_144 });
      assert!(matches!(error, TeamCraftError::BackupOutOfMemory(_)));
      assert!(error.to_string().contains("262144 KiB"));
    }

    #[test]
    fn unknown_versions_ask_for_an_update() {
      assert!(matches!(
        backup(BackupError::UnsupportedVersion(9)),
        TeamCraftError::BackupTooNew(_)
      ));
      assert!(matches!(
        backup(BackupError::UnsupportedKdf(2)),
        TeamCraftError::BackupTooNew(_)
      ));
      assert!(matches!(
        TeamCraftError::from(CoreError::KeyBag(KeyBagError::UnsupportedVersion(9))),
        TeamCraftError::BackupTooNew(_)
      ));
    }

    #[test]
    fn malformed_backups_are_damaged() {
      assert!(matches!(
        backup(BackupError::MalformedList),
        TeamCraftError::BackupDamaged(_)
      ));
      assert!(matches!(
        backup(BackupError::TooManyWraps),
        TeamCraftError::BackupDamaged(_)
      ));
    }

    #[test]
    fn the_message_keeps_the_cause_for_the_log() {
      let TeamCraftError::BackupTooNew(message) = backup(BackupError::UnsupportedVersion(9)) else {
        panic!("not a version error");
      };
      assert!(message.contains("version 9"), "{message}");
    }

    fn backup(error: BackupError) -> TeamCraftError {
      TeamCraftError::from(CoreError::Backup(error))
    }
  }
}
