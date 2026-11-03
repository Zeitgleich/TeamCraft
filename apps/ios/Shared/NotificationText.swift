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

  /// - `kindText`: the label of a photo, video or audio message (its file name is not shown).
  /// - `fromSenderText`: the fallback when nothing of the message is shown but the sender is known
  ///   ("New message from Alex"); without it `fallbackText` is used.
  init(
    maybeConversationName: String?, maybeSenderName: String?, maybeText: String?, maybeFileName: String?,
    maybeAttachmentKind: NotificationAttachmentKind? = nil,
    isDirect: Bool, fallbackSender: String, fallbackText: String, fileText: (String) -> String,
    kindText: (NotificationAttachmentKind) -> String = { _ in "" },
    fromSenderText: ((String) -> String)? = nil
  ) {
    func nonBlank(_ value: String?) -> String? {
      guard let value, !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return nil }
      return value
    }
    senderName = nonBlank(maybeSenderName) ?? maybeConversationName ?? fallbackSender
    if let text = nonBlank(maybeText) {
      self.text = text
    } else if let kind = maybeAttachmentKind, kind != .file, let label = nonBlank(kindText(kind)) {
      self.text = label
    } else if let file = nonBlank(maybeFileName) {
      self.text = fileText(file)
    } else if let sender = nonBlank(maybeSenderName), let fromSenderText {
      self.text = fromSenderText(sender)
    } else {
      self.text = fallbackText
    }
    maybeConversationTitle = isDirect ? nil : nonBlank(maybeConversationName)
  }
}
