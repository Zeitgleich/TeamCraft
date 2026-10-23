//! macOS: notifications through `UNUserNotificationCenter`, identified by their tag, so a newer one
//! replaces the older and a read conversation's or an answered call's notification is removed.
//!
//! Only a bundled, signed app may use the notification center (an unbundled development binary
//! crashes asking for it), so [`is_available`] checks for a bundle first and the caller falls back
//! to the notification plugin. Buttons and a reply field need a notification center delegate, an
//! Objective-C class this crate cannot define without `unsafe`; until then a click brings the app to
//! the front, where the conversation or the ringing call's card is.

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_foundation::{NSArray, NSBundle, NSError, NSString};
use objc2_user_notifications::{
  UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest, UNNotificationSound,
  UNUserNotificationCenter,
};
use teamcraft_client_api::os_notify::{OsNotification, TagTarget};

/// Whether the app runs from a bundle, which the notification center requires.
pub fn is_available() -> bool {
  NSBundle::mainBundle().bundleIdentifier().is_some()
}

/// Asks once for permission to show alerts and play sounds; macOS remembers the answer.
pub fn request_permission() {
  let center = UNUserNotificationCenter::currentNotificationCenter();
  let done = RcBlock::new(|is_granted: Bool, _error: *mut NSError| {
    if !is_granted.as_bool() {
      tracing::info!("notifications are not allowed for this app");
    }
  });
  center.requestAuthorizationWithOptions_completionHandler(
    UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound | UNAuthorizationOptions::Badge,
    &done,
  );
}

/// Shows (or replaces) the notification with the same tag.
pub fn show(notification: &OsNotification) {
  let content = UNMutableNotificationContent::new();
  content.setTitle(&NSString::from_str(&notification.title));
  if let Some(body) = &notification.maybe_body {
    content.setBody(&NSString::from_str(body));
  }
  // Notification Center groups a conversation's notifications.
  let thread = match notification.tag.target() {
    TagTarget::Conversation(id) | TagTarget::Call(id) | TagTarget::MissedCall(id) => id,
    TagTarget::Meeting(id) => id,
    TagTarget::Reminder => String::new(),
  };
  content.setThreadIdentifier(&NSString::from_str(&thread));
  // A ringing call is silent: the app plays its own ringtone.
  if !notification.kind.is_ringing() {
    content.setSound(Some(&UNNotificationSound::defaultSound()));
  }
  let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
    &NSString::from_str(notification.tag.as_str()),
    &content,
    None,
  );
  UNUserNotificationCenter::currentNotificationCenter()
    .addNotificationRequest_withCompletionHandler(&request, None);
}

/// Removes the notifications with these tags from the screen and Notification Center.
pub fn withdraw(tags: &[String]) {
  let identifiers: Vec<_> = tags.iter().map(|tag| NSString::from_str(tag)).collect();
  let identifiers = NSArray::from_retained_slice(&identifiers);
  UNUserNotificationCenter::currentNotificationCenter()
    .removeDeliveredNotificationsWithIdentifiers(&identifiers);
}
