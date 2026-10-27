//! macOS: a picture file as the attachment of a notification (#543), so Notification Center shows
//! the sender's avatar or the channel's picture beside the text. The constructor of
//! `UNNotificationAttachment` is an `unsafe` binding (its options are an untyped dictionary), which
//! is why it lives here and not in the shell.

/// Why a picture could not become an attachment.
#[derive(Debug, thiserror::Error)]
pub enum AttachmentError {
  #[error("the picture's path is not valid UTF-8")]
  Path,
  /// The notification center refused the file (missing, unreadable, an unsupported type).
  #[error("the notification center refused the picture: {0}")]
  Refused(String),
}

#[cfg(target_os = "macos")]
pub use imp::notification_attachment;

#[cfg(target_os = "macos")]
mod imp {
  use std::path::Path;

  use objc2::rc::Retained;
  use objc2_foundation::{NSString, NSURL};
  use objc2_user_notifications::UNNotificationAttachment;

  use super::AttachmentError;

  /// The attachment of the picture at `file`, under `identifier`. Once the notification is added
  /// the system moves the file into its own store, so every notification needs a copy of its
  /// own; a file that is refused stays where it is for the caller to delete.
  pub fn notification_attachment(
    identifier: &str,
    file: &Path,
  ) -> Result<Retained<UNNotificationAttachment>, AttachmentError> {
    let path = file.to_str().ok_or(AttachmentError::Path)?;
    let url = NSURL::fileURLWithPath(&NSString::from_str(path));
    #[allow(unsafe_code)]
    // SAFETY: the only `unsafe` part of the binding is the options dictionary's generic types; no
    // options are passed. Identifier and URL are valid objects for the call's duration.
    let attachment = unsafe {
      UNNotificationAttachment::attachmentWithIdentifier_URL_options_error(
        &NSString::from_str(identifier),
        &url,
        None,
      )
    };
    attachment.map_err(|error| AttachmentError::Refused(error.localizedDescription().to_string()))
  }
}
