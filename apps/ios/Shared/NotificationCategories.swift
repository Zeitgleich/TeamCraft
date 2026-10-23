import UserNotifications

/// The actions on a notification: reply from the lock screen and mark as read on messages (like the
/// Android notification), answer and decline on an incoming-call alert. The extension puts the
/// category on what it shows.
enum NotificationCategories {
  static let messageCategory = "message"
  static let callCategory = "call"
  static let replyAction = "reply"
  static let markReadAction = "mark_read"
  static let answerCallAction = "answer_call"
  static let declineCallAction = "decline_call"
  /// Key of the conversation ID in a notification's `userInfo`.
  static let conversationKey = "conversation_id"

  static func register(in center: UNUserNotificationCenter) {
    let reply = UNTextInputNotificationAction(
      identifier: replyAction,
      title: String(localized: "notification_reply"),
      // Unlock first: a reply sends as the user.
      options: [.authenticationRequired],
      textInputButtonTitle: String(localized: "send"),
      textInputPlaceholder: String(localized: "composer_hint"))
    let markRead = UNNotificationAction(
      identifier: markReadAction, title: String(localized: "notification_mark_read"), options: [])
    let message = UNNotificationCategory(
      identifier: messageCategory, actions: [reply, markRead], intentIdentifiers: [],
      options: [])
    // Answer opens the app, which joins the call (the microphone needs the app in the foreground).
    let answer = UNNotificationAction(
      identifier: answerCallAction, title: String(localized: "notification_answer_call"),
      options: [.foreground, .authenticationRequired])
    let decline = UNNotificationAction(
      identifier: declineCallAction, title: String(localized: "notification_decline_call"),
      options: [.destructive])
    let call = UNNotificationCategory(
      identifier: callCategory, actions: [answer, decline], intentIdentifiers: [], options: [])
    center.setNotificationCategories([message, call])
  }
}
