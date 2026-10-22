import Foundation

/// The `ChatAction` JSON messages of teamcraft-client-api that the app sends through
/// `TeamCraft.act(actionJson:)` (the same ones the desktop UI sends). Built as dictionaries:
/// the core's serde types are the schema, and ids travel as decimal strings.
enum ChatAction {
  case toggleReaction(conversationId: String, messageId: String, emoji: String)
  case editMessage(conversationId: String, messageId: String, body: String)
  case deleteMessage(conversationId: String, messageId: String)
  case setCustomStatus(CustomStatusValue?)
  case setActivity(isIdle: Bool)

  /// The wire form.
  var json: String {
    var object: [String: Any]
    switch self {
    case .toggleReaction(let conversation, let message, let emoji):
      object = [
        "action": "toggle_reaction", "conversation": conversation, "message": message, "emoji": emoji,
      ]
    case .editMessage(let conversation, let message, let body):
      object = ["action": "edit_message", "conversation": conversation, "message": message, "body": body]
    case .deleteMessage(let conversation, let message):
      object = ["action": "delete_message", "conversation": conversation, "message": message]
    case .setCustomStatus(let maybeStatus):
      object = ["action": "set_custom_status"]
      if let status = maybeStatus {
        object["maybe_status"] = ["emoji": status.emoji, "text": status.text, "expiry": status.expiry.rawValue]
      } else {
        object["maybe_status"] = NSNull()
      }
    case .setActivity(let isIdle):
      object = ["action": "set_activity", "is_idle": isIdle]
    }
    // Invariant: the dictionaries above hold only strings, bools and nulls, which always serialize.
    let data = (try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])) ?? Data("{}".utf8)
    return String(decoding: data, as: UTF8.self)
  }
}

/// A custom status next to the user's name ("🌴 Vacation").
struct CustomStatusValue: Equatable {
  var emoji: String
  var text: String
  var expiry: StatusExpiryValue
}

/// When a custom status clears itself (the core's `StatusExpiry` variant names).
enum StatusExpiryValue: String, CaseIterable, Identifiable {
  case never = "Never"
  case thirtyMinutes = "ThirtyMinutes"
  case oneHour = "OneHour"
  case today = "Today"
  case thisWeek = "ThisWeek"

  var id: String { rawValue }
}
