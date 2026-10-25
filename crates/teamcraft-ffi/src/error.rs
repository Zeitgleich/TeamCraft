//! Errors the app sees. Flat: Kotlin gets the variant and the message; the message is for logs,
//! the app shows its own (translated) text per variant.

use teamcraft_core::CoreError;
use teamcraft_core::api::ApiError;
use teamcraft_core::store::StoreError;

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
  /// The recovery passphrase (or other recovery data) doesn't open the key backup.
  #[error("the recovery data doesn't open the key backup: {0}")]
  WrongPassphrase(String),
  /// The local database couldn't be opened or written (wrong key, disk full).
  #[error("local storage: {0}")]
  Storage(String),
  /// A malformed ID or link code from the app.
  #[error("invalid input: {0}")]
  InvalidInput(String),
  /// A link or recovery step was called out of order.
  #[error("no device link in progress")]
  NoLinkInProgress,
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
      CoreError::Backup(backup) => Self::WrongPassphrase(backup.to_string()),
      CoreError::KeyBag(key_bag) => Self::WrongPassphrase(key_bag.to_string()),
      CoreError::Store(store) => store.into(),
      refused @ CoreError::CallMoveRefused { .. } => Self::CallMoveRefused(refused.to_string()),
      other => Self::Core(other.to_string()),
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
}
