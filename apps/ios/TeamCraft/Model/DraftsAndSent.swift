import Foundation

/// One unsent text, in a conversation or one of its threads (teamcraft-client-api `drafts`, #1220).
struct DraftItem: Decodable, Equatable, Identifiable, Sendable {
  let conversation: String
  var maybeThread: String?
  let conversationName: String
  /// The conversation is the user's notes: `conversationName` is empty (#1385). Absent from older cores.
  var isNotesToSelf: Bool?
  var maybeThreadRoot: String?
  let body: String
  /// Unix milliseconds.
  let editedAt: Int64
  /// Files attached to it; a draft may hold files and no text. Absent without files.
  var fileCount: Int?

  var listName: String { conversationListName(conversationName, isNotesToSelf: isNotesToSelf == true) }
  var id: String { "\(conversation)/\(maybeThread ?? "")" }
  var editedDate: Date { Date(timeIntervalSince1970: TimeInterval(editedAt) / 1000) }
}

/// A file attached to a draft (#1220), as the composer shows it.
struct DraftFileItem: Decodable, Equatable, Identifiable, Sendable {
  /// Names it for `FileSource::Draft` and for removing it.
  let id: String
  let fileName: String
  let mediaType: String
  var sizeBytes: Int64?
  var thumbnail: [UInt8]?
  let state: DraftFileStateWire

  var thumbnailData: Data? { thumbnail.map { Data($0) } }
  var fileState: DraftFileState { DraftFileState(rawValue: state.state) ?? .unknown }
  /// Goes out with the message: not one that failed or expired.
  var isSendable: Bool { [.uploading, .ready, .onThisDevice].contains(fileState) }
}

/// `DraftFileState` of the core, tagged by `state` (`{"state":"ready"}`).
struct DraftFileStateWire: Decodable, Equatable, Sendable {
  let state: String
}

/// Where a draft's file stands; one this app doesn't know counts as failed.
enum DraftFileState: String, Sendable {
  /// Going up from this device.
  case uploading
  /// Up and in the draft on every device of the user.
  case ready
  /// Kept on this device until it is sent.
  case onThisDevice = "on_this_device"
  /// Can't go up: the user removes it.
  case failed
  /// Held by the server too long: gone, not sent. The user attaches it again.
  case expired
  case unknown

  /// The mark on a chip of a file that won't go out.
  var maybeStatusKey: String? {
    switch self {
    case .failed, .unknown: return "composer_draft_file_failed"
    case .expired: return "composer_draft_file_expired"
    case .uploading, .ready, .onThisDevice: return nil
    }
  }
}

/// The answer of the `draft_files` query; its `files` aren't a message's (`ChatAnswer.files`).
struct DraftFilesAnswer: Decodable, Sendable {
  var files: [DraftFileItem]
}

/// The draft's files a composer shows besides its own picks (#1220): attached on another device, or
/// before the composer last opened. Not its own, nor those it removed or sent while the core catches up.
func otherDraftFiles(_ items: [DraftFileItem], local: [PickedFile], gone: Set<String>) -> [DraftFileItem] {
  let localIds = Set(local.compactMap(\.maybeDraftId))
  return items.filter { !localIds.contains($0.id) && !gone.contains($0.id) }
}

/// One message the user sent, as much of it as the list shows.
struct SentItem: Decodable, Equatable, Identifiable, Sendable {
  let conversation: String
  let conversationName: String
  /// The conversation is the user's notes: `conversationName` is empty (#1385). Absent from older cores.
  var isNotesToSelf: Bool?
  let message: String
  var maybeThread: String?
  let excerpt: String
  var fileCount: Int?
  /// Unix milliseconds.
  let sentAt: Int64

  var listName: String { conversationListName(conversationName, isNotesToSelf: isNotesToSelf == true) }
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
