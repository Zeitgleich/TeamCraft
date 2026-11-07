//! macOS: whether the user lets the app notify (#895), from the notification center's settings. The
//! completion handler hands the settings as a raw pointer, which only `unsafe` code may read, so
//! it lives here and not in the shell.

/// What macOS says about the app's notifications.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationAuthorization {
  /// The user has not been asked yet.
  NotDetermined,
  /// Turned off in System Settings, or declined at the prompt.
  Denied,
  /// Allowed (also provisionally or for a while).
  Authorized,
}

/// Calls `on_answer` once with the current authorization (from a thread of the system's choosing).
/// Only from a bundled app: without a bundle the notification center aborts the process.
#[cfg(target_os = "macos")]
pub fn notification_authorization(on_answer: impl FnOnce(NotificationAuthorization) + Send + 'static) {
  imp::read(Box::new(on_answer));
}

#[cfg(target_os = "macos")]
mod imp {
  use std::ptr::NonNull;
  use std::sync::Mutex;

  use block2::RcBlock;
  use objc2_user_notifications::{UNAuthorizationStatus, UNNotificationSettings, UNUserNotificationCenter};

  use super::NotificationAuthorization;

  type OnAnswer = Box<dyn FnOnce(NotificationAuthorization) + Send + 'static>;

  pub(super) fn read(on_answer: OnAnswer) {
    let on_answer = Mutex::new(Some(on_answer));
    let done = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
      #[allow(unsafe_code)]
      // SAFETY: the notification center hands a valid settings object that lives for the call.
      let status = unsafe { settings.as_ref() }.authorizationStatus();
      let authorization = match status {
        UNAuthorizationStatus::NotDetermined => NotificationAuthorization::NotDetermined,
        UNAuthorizationStatus::Denied => NotificationAuthorization::Denied,
        _ => NotificationAuthorization::Authorized,
      };
      if let Some(on_answer) = on_answer
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
      {
        on_answer(authorization);
      }
    });
    UNUserNotificationCenter::currentNotificationCenter().getNotificationSettingsWithCompletionHandler(&done);
  }
}
