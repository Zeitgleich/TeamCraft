import UserNotifications

/// The actions on a message notification: reply from the lock screen and mark as read (like the
/// Android notification). The extension puts `categoryIdentifier = message` on what it shows.
enum NotificationCategories {
  static let messageCategory = "message"
  static let replyAction = "reply"
  static let markReadAction = "mark_read"
  /// Key of the conversation ID in a notification's `userInfo`.
  static let conversationKey = "conversation_id"

  static func register(in center: UNUserNotificationCenter) {
    let reply = UNTextInputNotificationAction(
      identifier: replyAction,
      title: String(localized: "notification_reply"),
      options: [],
      textInputButtonTitle: String(localized: "send"),
      textInputPlaceholder: String(localized: "composer_hint"))
    let markRead = UNNotificationAction(
      identifier: markReadAction, title: String(localized: "notification_mark_read"), options: [])
    let category = UNNotificationCategory(
      identifier: messageCategory, actions: [reply, markRead], intentIdentifiers: [],
      options: [])
    center.setNotificationCategories([category])
  }
}
