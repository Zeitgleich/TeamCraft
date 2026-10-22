import Foundation
import Intents
import UserNotifications

/// Turns what the core could read of a push into the notification the user sees: title, text,
/// thread grouping, reply actions and, where the system supports it, a communication notification
/// (sender name in the system's messaging style via `INSendMessageIntent`).
struct NotificationContentBuilder {
  let fallbackSender: String
  let fallbackText: String
  let fileText: (String) -> String

  /// What is shown while nothing could be decrypted (or the extension ran out of time).
  func generic(from original: UNNotificationContent) -> UNNotificationContent {
    guard let content = original.mutableCopy() as? UNMutableNotificationContent else { return original }
    // Nothing was decrypted, so nothing in the payload is trusted: no conversation, no actions.
    content.userInfo = [:]
    content.categoryIdentifier = ""
    content.threadIdentifier = ""
    if content.title.isEmpty { content.title = fallbackSender }
    if content.body.isEmpty { content.body = fallbackText }
    return content
  }

  func content(
    for notification: PushNotification, base: UNNotificationContent
  ) -> (content: UNMutableNotificationContent, maybeIntent: INSendMessageIntent?) {
    let text = NotificationText(
      maybeConversationName: notification.maybeConversationName,
      maybeSenderName: notification.maybeSenderName,
      maybeText: notification.maybeText,
      maybeFileName: notification.maybeFileName,
      isDirect: notification.isDirect,
      fallbackSender: fallbackSender, fallbackText: fallbackText, fileText: fileText)
    let content = (base.mutableCopy() as? UNMutableNotificationContent) ?? UNMutableNotificationContent()
    content.title = text.title
    content.subtitle = text.maybeSubtitle ?? ""
    content.body = text.text
    content.threadIdentifier = notification.conversationId
    content.categoryIdentifier = NotificationCategories.messageCategory
    // Only what the core decrypted; the server-supplied userInfo is dropped.
    var info: [AnyHashable: Any] = [NotificationCategories.conversationKey: notification.conversationId]
    if let tag = NotificationAuthenticator().tag(for: notification.conversationId) {
      info[NotificationAuthenticator.tagKey] = tag
    }
    content.userInfo = info
    // Without a tag (no key yet) the actions would be refused, so don't offer them.
    if info[NotificationAuthenticator.tagKey] == nil { content.categoryIdentifier = "" }
    content.sound = notification.isSilent ? nil : .default
    if notification.isUrgent {
      content.interruptionLevel = .timeSensitive
    }
    return (content, intent(for: notification, text: text))
  }

  private func intent(for notification: PushNotification, text: NotificationText) -> INSendMessageIntent? {
    guard let senderId = notification.maybeSenderId else { return nil }
    let handle = INPersonHandle(value: senderId, type: .unknown)
    let sender = INPerson(
      personHandle: handle, nameComponents: nil, displayName: text.senderName, image: nil,
      contactIdentifier: nil, customIdentifier: senderId)
    let group = text.maybeConversationTitle.map { INSpeakableString(spokenPhrase: $0) }
    return INSendMessageIntent(
      recipients: nil, outgoingMessageType: .outgoingMessageText, content: text.text,
      speakableGroupName: group, conversationIdentifier: notification.conversationId,
      serviceName: nil, sender: sender, attachments: nil)
  }

  /// Donates the intent and lets the system restyle the notification as a communication
  /// notification; falls back to the plain content if the system refuses (no entitlement).
  func communicationStyled(
    _ content: UNMutableNotificationContent, intent: INSendMessageIntent?
  ) async -> UNNotificationContent {
    guard let intent else { return content }
    let interaction = INInteraction(intent: intent, response: nil)
    interaction.direction = .incoming
    do {
      try await interaction.donate()
      return try content.updating(from: intent)
    } catch {
      return content
    }
  }
}
