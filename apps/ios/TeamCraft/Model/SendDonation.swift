import Intents

/// Tells the system the user wrote to a conversation (issue #2059), so the share sheet suggests the
/// chats they write to, not only those that write to them (`NotificationContentBuilder` donates the
/// incoming side). No message text: the system must not keep decrypted content.
enum SendDonation {
  /// The intent of a sent message to `conversationId` shown as `title`; for a direct message the
  /// other person is its one recipient.
  static func intent(conversationId: String, title: String, isDirect: Bool) -> INSendMessageIntent {
    let recipients: [INPerson]? = isDirect
      ? [
        INPerson(
          personHandle: INPersonHandle(value: conversationId, type: .unknown), nameComponents: nil,
          displayName: title, image: nil, contactIdentifier: nil, customIdentifier: conversationId)
      ] : nil
    return INSendMessageIntent(
      recipients: recipients, outgoingMessageType: .outgoingMessageText, content: nil,
      speakableGroupName: isDirect ? nil : INSpeakableString(spokenPhrase: title),
      conversationIdentifier: conversationId, serviceName: nil, sender: nil, attachments: nil)
  }

  /// Donates the intent; a refusal (no entitlement, Siri off) is not the user's concern.
  static func donate(conversationId: String, title: String, isDirect: Bool) {
    guard !title.isEmpty else { return }
    let interaction = INInteraction(
      intent: intent(conversationId: conversationId, title: title, isDirect: isDirect), response: nil)
    interaction.direction = .outgoing
    interaction.donate { _ in }
  }
}
