import Foundation

/// One unsent text, in a conversation or one of its threads (teamcraft-client-api `drafts`, #1220).
struct DraftItem: Decodable, Equatable, Identifiable, Sendable {
  let conversation: String
  var maybeThread: String?
  let conversationName: String
  var maybeThreadRoot: String?
  let body: String
  /// Unix milliseconds.
  let editedAt: Int64

  var id: String { "\(conversation)/\(maybeThread ?? "")" }
  var editedDate: Date { Date(timeIntervalSince1970: TimeInterval(editedAt) / 1000) }
}

/// One message the user sent, as much of it as the list shows.
struct SentItem: Decodable, Equatable, Identifiable, Sendable {
  let conversation: String
  let conversationName: String
  let message: String
  var maybeThread: String?
  let excerpt: String
  var fileCount: Int?
  /// Unix milliseconds.
  let sentAt: Int64

  var id: String { message }
  var sentDate: Date { Date(timeIntervalSince1970: TimeInterval(sentAt) / 1000) }
}

/// Where "Drafts & sent" sends the user: the conversation's pane opens the thread itself, and with
/// `isWriting` the composer of that place takes the focus.
struct OpenedPlace: Equatable, Sendable {
  let conversationId: String
  var maybeThreadId: String?
  var isWriting = false
}
