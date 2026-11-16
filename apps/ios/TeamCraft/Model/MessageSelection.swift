import Foundation

/// The messages picked in selection mode (issue #2070). The rules are those of the web client's
/// `selection.rs`: forward, copy and delete act on the same, oldest-first list, and delete is only
/// offered when every picked message may be deleted by this user.
struct MessageSelection: Equatable {
  /// Selection mode is on from `start(with:)` until `clear()`, even with nothing picked.
  private(set) var isActive = false
  private(set) var ids: Set<String> = []

  var count: Int { ids.count }

  /// Starts selection mode with `id` picked.
  mutating func start(with id: String) {
    isActive = true
    ids = [id]
  }

  func isSelected(_ id: String) -> Bool { ids.contains(id) }

  /// Picks the message, or drops it when it is picked. Mode stays on with nothing picked: leaving
  /// is Cancel's job, not an emptied list's.
  mutating func toggle(_ id: String) {
    guard isActive else { return }
    if ids.contains(id) { ids.remove(id) } else { ids.insert(id) }
  }

  /// Leaves selection mode.
  mutating func clear() {
    isActive = false
    ids = []
  }

  /// Drops the picked messages that `isGone` names (deleted meanwhile).
  mutating func forget(where isGone: (String) -> Bool) {
    ids = ids.filter { !isGone($0) }
  }

  /// Whether a message can be picked: one a person wrote that the server holds and this device
  /// can read, not a system line.
  static func isSelectable(_ message: Message) -> Bool {
    message.maybePlaceholder == nil && !message.isPending && message.maybeSystemEvent == nil
      && message.maybeCall == nil
  }

  /// The picked messages of `feed`, oldest first: the order they are forwarded and copied in.
  func ordered(in feed: [Message]) -> [Message] {
    feed.filter { ids.contains($0.id) && Self.isSelectable($0) }
      .sorted { ($0.sentAtMs, $0.id) < ($1.sentAtMs, $1.id) }
  }

  /// Whether `me` may delete every one of `messages`: only their own, imported ones excepted, and
  /// only where posting is on (the rule of a single message's Delete in the menu).
  static func canDeleteAll(_ messages: [Message], me: String, canPost: Bool) -> Bool {
    canPost && !messages.isEmpty
      && messages.allSatisfy { $0.authorId == me && $0.maybeImported == nil && !$0.isPending }
  }

  /// What Copy puts on the pasteboard: per message the sender and time on a line, then the text,
  /// with a blank line between messages.
  static func copyText(_ messages: [Message], timeText: (Int64) -> String) -> String {
    messages
      .map { "\($0.shownAuthorName), \(timeText($0.sentAtMs))\n\($0.body)" }
      .joined(separator: "\n\n")
  }
}
