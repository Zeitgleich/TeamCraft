import Foundation
import UserNotifications

/// Removes delivered notifications and sets the badge, for the extension and the app alike. Which
/// notifications go is `NotificationMatcher`'s decision.
enum DeliveredNotifications {
  static func all(in center: UNUserNotificationCenter = .current()) async -> [DeliveredPush] {
    await center.deliveredNotifications().map {
      DeliveredPush(identifier: $0.request.identifier, userInfo: $0.request.content.userInfo)
    }
  }

  /// A `dismiss` wake-up: the conversation or thread was read elsewhere, or a ring ended.
  static func apply(dismiss notification: PushNotification, in center: UNUserNotificationCenter = .current()) async {
    let target = DismissTarget(
      conversationId: notification.conversationId, maybeThreadId: notification.maybeThreadId,
      maybeMessageId: notification.maybeMessageId, maybeCallId: notification.maybeCallId)
    let identifiers = NotificationMatcher.identifiers(await all(in: center), dismissing: target)
    if !identifiers.isEmpty { center.removeDeliveredNotifications(withIdentifiers: identifiers) }
    await setBadge(notification.maybeBadge, in: center)
  }

  /// Before a message is shown again (urgent repeat, or a duplicate shown quietly): its earlier
  /// notification goes, so the message is listed once.
  static func removeEarlier(of notification: PushNotification, in center: UNUserNotificationCenter = .current())
    async
  {
    guard let messageId = notification.maybeMessageId else { return }
    let identifiers = NotificationMatcher.identifiers(
      await all(in: center), sameMessageAs: messageId, in: notification.conversationId)
    if !identifiers.isEmpty { center.removeDeliveredNotifications(withIdentifiers: identifiers) }
  }

  /// Top-level notifications of conversations that are fully read now.
  static func removeRead(conversations: Set<String>, in center: UNUserNotificationCenter = .current()) async {
    guard !conversations.isEmpty else { return }
    let identifiers = NotificationMatcher.identifiers(await all(in: center), readConversations: conversations)
    if !identifiers.isEmpty { center.removeDeliveredNotifications(withIdentifiers: identifiers) }
  }

  static func setBadge(_ maybeBadge: UInt32?, in center: UNUserNotificationCenter = .current()) async {
    guard let badge = maybeBadge else { return }
    // Best effort: without the badge permission the system refuses, which changes nothing.
    try? await center.setBadgeCount(Int(badge))
  }
}
