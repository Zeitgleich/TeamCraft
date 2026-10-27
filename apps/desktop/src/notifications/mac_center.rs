//! macOS: notifications through `UNUserNotificationCenter`, identified by their tag, so a newer one
//! replaces the older and a read conversation's or an answered call's notification is removed.
//!
//! Only a bundled, signed app may use the notification center (an unbundled development binary
//! crashes asking for it), so [`is_available`] checks for a bundle first and the caller falls back
//! to the notification plugin. Buttons and the reply field come from a category registered for the
//! notification (labels are translated, so categories are built as notifications arrive); clicks,
//! buttons and replies come back through the delegate of `teamcraft-desktop-native`, which is also
//! why banners show while the app is in front.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2_foundation::{NSArray, NSBundle, NSError, NSSet, NSString};
use objc2_user_notifications::{
  UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationAction, UNNotificationActionOptions,
  UNNotificationCategory, UNNotificationCategoryOptions, UNNotificationInterruptionLevel,
  UNNotificationRequest, UNNotificationSound, UNTextInputNotificationAction, UNUserNotificationCenter,
};
use teamcraft_client_api::os_notify::{NotificationAction, OsNotification, TagTarget};

/// Categories kept registered: one per combination of buttons and labels, which changes with the
/// language only.
const MAX_CATEGORIES: usize = 32;

/// The unread count last asked for, so a badge set before the user answered the permission prompt is
/// applied once they allow it.
static WANTED_BADGE: AtomicU32 = AtomicU32::new(0);

/// The buttons and reply field of a notification, which macOS wants registered as a category
/// before a notification of that category is shown.
#[derive(Clone, Debug, PartialEq, Eq)]
struct CategorySpec {
  id: String,
  /// The reply field first: its action's title and placeholder.
  maybe_reply: Option<(String, String)>,
  buttons: Vec<(NotificationAction, String)>,
}

/// Every category registered so far; `setNotificationCategories` replaces the whole set.
static CATEGORIES: Mutex<Vec<CategorySpec>> = Mutex::new(Vec::new());

/// Whether the app runs from a bundle, which the notification center requires.
pub fn is_available() -> bool {
  NSBundle::mainBundle().bundleIdentifier().is_some()
}

/// Asks once for permission to show alerts and play sounds; macOS remembers the answer.
pub fn request_permission() {
  let center = UNUserNotificationCenter::currentNotificationCenter();
  let done = RcBlock::new(|is_granted: Bool, _error: *mut NSError| {
    if is_granted.as_bool() {
      apply_badge(WANTED_BADGE.load(Ordering::Relaxed));
    } else {
      tracing::info!("notifications are not allowed for this app, so there is no dock badge");
    }
  });
  center.requestAuthorizationWithOptions_completionHandler(
    UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound | UNAuthorizationOptions::Badge,
    &done,
  );
}

/// Shows the unread count on the Dock icon (0 removes it). Since macOS 13 the badge belongs to the
/// notification center and is shown only while the app may badge (`UNAuthorizationOptions::Badge`);
/// the older `NSDockTile` label alone no longer appears for every user (#489).
pub fn set_badge(unread: u32) {
  WANTED_BADGE.store(unread, Ordering::Relaxed);
  apply_badge(unread);
}

fn apply_badge(count: u32) {
  let done = RcBlock::new(|error: *mut NSError| {
    if !error.is_null() {
      tracing::debug!("could not set the dock badge through the notification center");
    }
  });
  UNUserNotificationCenter::currentNotificationCenter()
    .setBadgeCount_withCompletionHandler(isize::try_from(count).unwrap_or(isize::MAX), Some(&done));
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
    TagTarget::JoinRequest { conversation, .. } => conversation,
    TagTarget::Reminder => String::new(),
  };
  content.setThreadIdentifier(&NSString::from_str(&thread));
  // A ringing call is silent (the app plays its own ringtone) and time-sensitive: it breaks through
  // a Focus that lets time-sensitive notifications through, as a phone call does. The app's own
  // do-not-disturb already kept it from ringing (#650). A message is silent when the app plays its
  // own message sound, or none (#800).
  if notification.kind.is_ringing() {
    content.setInterruptionLevel(UNNotificationInterruptionLevel::TimeSensitive);
  }
  if !notification.is_quiet() {
    content.setSound(Some(&UNNotificationSound::defaultSound()));
  }
  if let Some(spec) = category_spec(notification) {
    register_category(&spec);
    content.setCategoryIdentifier(&NSString::from_str(&spec.id));
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

fn category_spec(notification: &OsNotification) -> Option<CategorySpec> {
  if notification.actions.is_empty() && notification.maybe_reply.is_none() {
    return None;
  }
  let maybe_reply = notification
    .maybe_reply
    .as_ref()
    .map(|reply| (reply.send_label.clone(), reply.placeholder.clone()));
  let buttons: Vec<_> = notification
    .actions
    .iter()
    .map(|button| (button.action, button.label.clone()))
    .collect();
  let id = category_id(maybe_reply.as_ref(), &buttons);
  Some(CategorySpec {
    id,
    maybe_reply,
    buttons,
  })
}

/// The same buttons with the same labels always give the same identifier.
fn category_id(maybe_reply: Option<&(String, String)>, buttons: &[(NotificationAction, String)]) -> String {
  let mut id = String::from("teamcraft");
  if let Some((title, placeholder)) = maybe_reply {
    id.push_str(&format!("|reply={title}={placeholder}"));
  }
  for (action, label) in buttons {
    id.push_str(&format!("|{}={label}", action.key()));
  }
  id
}

/// Makes the category known to the notification center, with all the earlier ones.
fn register_category(spec: &CategorySpec) {
  let specs = {
    let mut specs = CATEGORIES
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    if specs.contains(spec) {
      return;
    }
    if specs.len() >= MAX_CATEGORIES {
      specs.remove(0);
    }
    specs.push(spec.clone());
    specs.clone()
  };
  let categories: Vec<Retained<UNNotificationCategory>> = specs.iter().map(category).collect();
  UNUserNotificationCenter::currentNotificationCenter()
    .setNotificationCategories(&NSSet::from_retained_slice(&categories));
}

fn category(spec: &CategorySpec) -> Retained<UNNotificationCategory> {
  let mut actions: Vec<Retained<UNNotificationAction>> = Vec::new();
  if let Some((title, placeholder)) = &spec.maybe_reply {
    actions.push(
      UNTextInputNotificationAction::actionWithIdentifier_title_options_textInputButtonTitle_textInputPlaceholder(
        &NSString::from_str(NotificationAction::Reply.key()),
        &NSString::from_str(title),
        UNNotificationActionOptions::empty(),
        &NSString::from_str(title),
        &NSString::from_str(placeholder),
      )
      .into_super(),
    );
  }
  for (action, label) in &spec.buttons {
    // Answering or joining brings the window up; marking read and declining stay in the background.
    let options = if action.brings_window_up() {
      UNNotificationActionOptions::Foreground
    } else {
      UNNotificationActionOptions::empty()
    };
    actions.push(UNNotificationAction::actionWithIdentifier_title_options(
      &NSString::from_str(action.key()),
      &NSString::from_str(label),
      options,
    ));
  }
  UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
    &NSString::from_str(&spec.id),
    &NSArray::from_retained_slice(&actions),
    &NSArray::new(),
    UNNotificationCategoryOptions::empty(),
  )
}
