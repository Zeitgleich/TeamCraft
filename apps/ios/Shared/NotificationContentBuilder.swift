import Foundation
import Intents
import UserNotifications

/// Turns what the core could read of a push into the notification the user sees: title, text,
/// thread grouping, actions, sound and interruption level, badge and, where the system supports it,
/// a communication notification (sender name in the system's messaging style via
/// `INSendMessageIntent`). Used by the extension and by the app for pushes it opens in the
/// foreground.
struct NotificationContentBuilder {
  let strings: NotificationStrings

  /// What is shown while nothing could be decrypted (or the extension ran out of time). With
  /// `keepsSealed` the sealed wake-up stays in `userInfo`, so the app (which holds the store while it
  /// is in the foreground) can open it when the system asks it to present the notification.
  func generic(from original: UNNotificationContent, keepsSealed: Bool = false) -> UNNotificationContent {
    guard let content = original.mutableCopy() as? UNMutableNotificationContent else { return original }
    // Nothing was decrypted, so nothing in the payload is trusted: no conversation, no actions.
    let maybeSealed = original.userInfo[PushPayload.sealedKey]
    content.userInfo = [:]
    if keepsSealed, let maybeSealed { content.userInfo = [PushPayload.sealedKey: maybeSealed] }
    content.categoryIdentifier = ""
    content.threadIdentifier = ""
    if content.title.isEmpty { content.title = strings.appName }
    if content.body.isEmpty { content.body = strings.newMessage }
    return content
  }

  /// A message notification (`presentation` is `.message` or `.quietMessage`).
  func message(
    for notification: PushNotification, presentation: PushPresentation, base: UNNotificationContent
  ) -> (content: UNMutableNotificationContent, maybeIntent: INSendMessageIntent?) {
    let tag = NotificationAuthenticator().tag(for: notification.conversationId)
    let text = NotificationText(
      maybeConversationName: notification.maybeConversationName,
      maybeSenderName: notification.maybeSenderName,
      maybeText: notification.maybeText,
      maybeFileName: notification.maybeFileName,
      maybeAttachmentKind: notification.maybeAttachmentKind,
      isDirect: notification.isDirect,
      fallbackSender: strings.appName, fallbackText: strings.newMessage, fileText: strings.fileText,
      kindText: strings.attachmentText, fromSenderText: strings.newMessageFrom)
    let content = Self.mutable(base)
    content.title = text.title
    content.subtitle = text.maybeSubtitle ?? ""
    content.body = text.text
    content.threadIdentifier = notification.conversationId
    content.targetContentIdentifier = notification.conversationId
    // Only what the core decrypted; the server-supplied userInfo is dropped.
    content.userInfo = NotificationKeys.userInfo(
      kind: NotificationKeys.messageKind, conversationId: notification.conversationId, maybeTag: tag,
      maybeMessageId: notification.maybeMessageId, maybeThreadId: notification.maybeThreadId)
    // Reply and mark-read need the tag (no key yet: they would be refused) and a conversation the
    // core verified (the wake-up's conversation alone is the server's word).
    content.categoryIdentifier =
      tag != nil && notification.isConversationVerified ? NotificationCategories.messageCategory : ""
    Self.apply(presentation, flags: PushFlags(notification), to: content)
    let maybeIntent = presentation == .message ? intent(for: notification, text: text) : nil
    return (content, maybeIntent)
  }

  /// An incoming call without CallKit (no filtering entitlement): a time-sensitive alert that rings
  /// once, with answer and decline.
  func callAlert(for notification: PushNotification, base: UNNotificationContent) -> UNMutableNotificationContent {
    let tag = NotificationAuthenticator().tag(for: notification.conversationId)
    let caller = Self.nonBlank(notification.maybeSenderName) ?? Self.nonBlank(notification.maybeConversationName)
    let content = Self.mutable(base)
    content.title = strings.callTitle
    content.subtitle = ""
    content.body = caller.map(strings.callFrom) ?? strings.callUnknown
    content.threadIdentifier = Self.callThread(notification.conversationId)
    content.targetContentIdentifier = notification.conversationId
    content.userInfo = NotificationKeys.userInfo(
      kind: NotificationKeys.callKind, conversationId: notification.conversationId, maybeTag: tag,
      maybeCallId: notification.maybeCallId)
    content.categoryIdentifier =
      tag != nil && notification.isConversationVerified ? NotificationCategories.callCategory : ""
    Self.apply(.callAlert, flags: PushFlags(notification), to: content)
    return content
  }

  /// The diagnostics' test push arrived and was opened end to end.
  func test(for notification: PushNotification, base: UNNotificationContent) -> UNMutableNotificationContent {
    let content = Self.mutable(base)
    content.title = strings.appName
    content.subtitle = ""
    content.body = strings.testBody
    content.threadIdentifier = NotificationKeys.testKind
    content.categoryIdentifier = ""
    content.userInfo = [NotificationKeys.kind: NotificationKeys.testKind]
    Self.apply(.test, flags: PushFlags(notification), to: content)
    return content
  }

  /// Something iOS must show although there is nothing to say (no filtering entitlement).
  func quietGeneric(for flags: PushFlags, base: UNNotificationContent) -> UNMutableNotificationContent {
    let content = Self.mutable(base)
    content.title = strings.appName
    content.subtitle = ""
    content.body = strings.quietUpdate
    content.threadIdentifier = "other"
    content.categoryIdentifier = ""
    content.userInfo = [:]
    Self.apply(.quietGeneric, flags: flags, to: content)
    return content
  }

  /// Sound, interruption level, ranking and badge of a presentation. Pure, unit-tested.
  static func apply(_ presentation: PushPresentation, flags: PushFlags, to content: UNMutableNotificationContent) {
    switch presentation {
    case .message:
      content.sound = flags.isSilent ? nil : .default
      content.interruptionLevel = flags.isUrgent ? .timeSensitive : .active
      // The core's mention rules (@channel, first name, keywords) decide `isMention`: mentions rank
      // first in the notification summary and the stack.
      content.relevanceScore = relevance(isMention: flags.isMention, isUrgent: flags.isUrgent)
    case .callAlert:
      content.sound = .defaultRingtone
      content.interruptionLevel = .timeSensitive
      content.relevanceScore = 1
    case .test:
      content.sound = .default
      content.interruptionLevel = .active
      content.relevanceScore = 0.1
    case .quietMessage, .quietGeneric, .callKit, .drop:
      content.sound = nil
      content.interruptionLevel = .passive
      content.relevanceScore = 0
    }
    if let badge = flags.maybeBadge { content.badge = NSNumber(value: badge) }
  }

  /// Ranking in the notification summary: urgent first, then mentions, then the rest.
  static func relevance(isMention: Bool, isUrgent: Bool) -> Double {
    if isUrgent { return 1 }
    return isMention ? 0.8 : 0.3
  }

  /// Call alerts are grouped apart from the conversation's messages, so reading the conversation
  /// doesn't remove a ringing call.
  static func callThread(_ conversationId: String) -> String {
    "call-\(conversationId)"
  }

  /// Donates the intent and lets the system restyle the notification as a communication
  /// notification; falls back to the plain content if the system refuses (no entitlement). The
  /// presentation's sound and interruption level are set again on the result.
  func communicationStyled(
    _ content: UNMutableNotificationContent, intent: INSendMessageIntent?, presentation: PushPresentation,
    flags: PushFlags
  ) async -> UNNotificationContent {
    guard let intent else { return content }
    let interaction = INInteraction(intent: intent, response: nil)
    interaction.direction = .incoming
    do {
      try await interaction.donate()
      let updated = try content.updating(from: intent)
      guard let styled = updated.mutableCopy() as? UNMutableNotificationContent else { return updated }
      Self.apply(presentation, flags: flags, to: styled)
      return styled
    } catch {
      return content
    }
  }

  private func intent(for notification: PushNotification, text: NotificationText) -> INSendMessageIntent? {
    guard let senderId = notification.maybeSenderId else { return nil }
    let handle = INPersonHandle(value: senderId, type: .unknown)
    let sender = INPerson(
      personHandle: handle, nameComponents: nil, displayName: text.senderName, image: nil,
      contactIdentifier: nil, customIdentifier: senderId)
    let group = text.maybeConversationTitle.map { INSpeakableString(spokenPhrase: $0) }
    // No message text: the intent is donated to the system (Siri suggestions, search), which must
    // not keep decrypted content. The notification shows the text from its own body.
    return INSendMessageIntent(
      recipients: nil, outgoingMessageType: .outgoingMessageText, content: nil,
      speakableGroupName: group, conversationIdentifier: notification.conversationId,
      serviceName: nil, sender: sender, attachments: nil)
  }

  private static func mutable(_ base: UNNotificationContent) -> UNMutableNotificationContent {
    (base.mutableCopy() as? UNMutableNotificationContent) ?? UNMutableNotificationContent()
  }

  private static func nonBlank(_ value: String?) -> String? {
    guard let value, !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return nil }
    return value
  }
}

/// The translated texts of notifications. Both the app's and the extension's String Catalog carry
/// these keys (whichever process builds the notification looks them up in its own bundle).
struct NotificationStrings {
  let appName: String
  let newMessage: String
  let fileText: (String) -> String
  let attachmentText: (NotificationAttachmentKind) -> String
  let newMessageFrom: (String) -> String
  let callTitle: String
  let callFrom: (String) -> String
  let callUnknown: String
  let testBody: String
  let quietUpdate: String

  static var localized: NotificationStrings {
    NotificationStrings(
      appName: String(localized: "app_name"),
      newMessage: String(localized: "notification_new_message"),
      fileText: { String(localized: "notification_file \($0)") },
      attachmentText: { kind in
        switch kind {
        case .image: String(localized: "notification_photo")
        case .video: String(localized: "notification_video")
        case .audio: String(localized: "notification_audio")
        case .file: String(localized: "notification_new_message")
        }
      },
      newMessageFrom: { String(localized: "notification_new_message_from \($0)") },
      callTitle: String(localized: "notification_call_title"),
      callFrom: { String(localized: "notification_call_from \($0)") },
      callUnknown: String(localized: "notification_call_unknown"),
      testBody: String(localized: "notification_test_body"),
      quietUpdate: String(localized: "notification_quiet_update"))
  }
}
