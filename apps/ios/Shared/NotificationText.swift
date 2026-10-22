import Foundation

/// Title and lines of a message notification, from what the push preview could read. Pure, so the
/// choices are unit-tested; the localized fallbacks come in as parameters (same rules as the
/// Android app's `NotificationText`).
struct NotificationText: Equatable {
  /// The conversation's name for channels and group DMs; `nil` for one-to-one DMs.
  let maybeConversationTitle: String?
  let senderName: String
  let text: String

  /// The notification's title: the conversation for channels, the sender for DMs.
  var title: String { maybeConversationTitle ?? senderName }
  /// A subtitle naming the sender when the title is the conversation.
  var maybeSubtitle: String? { maybeConversationTitle == nil ? nil : senderName }

  init(
    maybeConversationName: String?, maybeSenderName: String?, maybeText: String?, maybeFileName: String?,
    isDirect: Bool, fallbackSender: String, fallbackText: String, fileText: (String) -> String
  ) {
    func nonBlank(_ value: String?) -> String? {
      guard let value, !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return nil }
      return value
    }
    senderName = nonBlank(maybeSenderName) ?? maybeConversationName ?? fallbackSender
    if let text = nonBlank(maybeText) {
      self.text = text
    } else if let file = nonBlank(maybeFileName) {
      self.text = fileText(file)
    } else {
      self.text = fallbackText
    }
    maybeConversationTitle = isDirect ? nil : nonBlank(maybeConversationName)
  }
}
