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
  case sendMessage(SendRequest)
  case downloadFile(conversationId: String, messageId: String)
  /// Unread again from `fromMessageId` on (account data, synced to the user's devices).
  case markUnread(conversationId: String, fromMessageId: String)
  case forward(fromConversationId: String, messageId: String, toConversationId: String)
  case setPinned(conversationId: String, messageId: String, isPinned: Bool)
  case setSaved(conversationId: String, messageId: String, isSaved: Bool)
  /// The user types (send while the text changes; the core throttles).
  case typing(conversationId: String)
  /// An own message the server rejected: send it again, or drop it.
  case retrySend(conversationId: String, messageId: String)
  case discardUnsent(conversationId: String, messageId: String)

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
    case .sendMessage(let request):
      object = ["action": "send_message", "message": request.object]
    case .downloadFile(let conversation, let message):
      object = ["action": "download_file", "conversation": conversation, "message": message]
    case .markUnread(let conversation, let message):
      object = ["action": "mark_unread", "conversation": conversation, "from": message]
    case .forward(let from, let message, let to):
      object = ["action": "forward_message", "from": from, "message": message, "to": to, "comment": ""]
    case .setPinned(let conversation, let message, let isPinned):
      object = ["action": "set_pinned", "conversation": conversation, "message": message, "is_pinned": isPinned]
    case .setSaved(let conversation, let message, let isSaved):
      object = ["action": "set_saved", "conversation": conversation, "message": message, "is_saved": isSaved]
    case .typing(let conversation):
      object = ["action": "typing", "conversation": conversation]
    case .retrySend(let conversation, let message):
      object = ["action": "retry_send", "conversation": conversation, "message": message]
    case .discardUnsent(let conversation, let message):
      object = ["action": "discard_unsent", "conversation": conversation, "message": message]
    }
    return jsonText(object)
  }
}

/// A message with files or a reply (`OutgoingMessage` of the core). Plain text goes through
/// `TeamCraft.sendMessage` instead.
struct SendRequest: Equatable {
  var conversationId: String
  var maybeThreadId: String?
  var body: String
  var files: [PickedFile] = []
  var maybeReplyToId: String?

  var object: [String: Any] {
    [
      "conversation": conversationId,
      "maybe_thread": maybeThreadId.map { $0 as Any } ?? NSNull(),
      "body": body,
      "maybe_reply_to": maybeReplyToId.map { $0 as Any } ?? NSNull(),
      "priority": "normal",
      "wants_acknowledgement": false,
      "is_silent": false,
      "also_send_to_channel": false,
      "files": files.map { file in
        [
          "file_name": file.fileName, "media_type": file.mediaType,
          "source": ["kind": "path", "path": file.path],
        ] as [String: Any]
      },
    ]
  }
}

/// A file picked for sending, already copied to a path the core can read.
struct PickedFile: Equatable, Identifiable {
  let fileName: String
  let mediaType: String
  let path: String

  var id: String { path }
}

/// The `ChatQuery` JSON messages (tagged by `kind`) that `TeamCraft.query(queryJson:)` answers.
enum ChatQuery {
  case messageExtras(conversationId: String, messageIds: [String])
  case conversationFiles(conversationId: String)
  case maxUploadBytes
  case resolvePermalink(link: String)
  case typingUsers(conversationId: String)
  case permalink(conversationId: String, messageId: String)

  var json: String {
    switch self {
    case .messageExtras(let conversation, let messages):
      return jsonText(["kind": "message_extras", "conversation": conversation, "messages": messages])
    case .conversationFiles(let conversation):
      return jsonText(["kind": "conversation_files", "conversation": conversation])
    case .maxUploadBytes:
      return jsonText(["kind": "max_upload_bytes"])
    case .resolvePermalink(let link):
      return jsonText(["kind": "resolve_permalink", "link": link])
    case .typingUsers(let conversation):
      return jsonText(["kind": "typing_users", "conversation": conversation])
    case .permalink(let conversation, let message):
      return jsonText(["kind": "permalink", "conversation": conversation, "message": message])
    }
  }
}

/// Compact JSON with sorted keys.
func jsonText(_ object: [String: Any]) -> String {
  // Invariant: callers pass only strings, numbers, bools, nulls and arrays or dictionaries of them,
  // which always serialize.
  let data = (try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])) ?? Data("{}".utf8)
  return String(decoding: data, as: UTF8.self)
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
