import Foundation

/// The conversations the share extension offers (issue #1037), as the app saw them last. The
/// extension links no core, so it can't ask `share_targets()` itself: the app writes this list into
/// the App Group, and the extension draws its picker from it. Names only, no messages; not written
/// while the app lock is on (the lock hides names everywhere outside the app), deleted at sign-out.
struct ShareTargetsSnapshot: Codable, Equatable, Sendable {
  /// The account that wrote it (`SharedContainer.activeAccount`, the same value the share inbox
  /// is claimed with). The extension shows it only while the inbox belongs to the same person.
  let owner: String
  /// Notes first, then by last activity, as `share_targets()` orders them.
  let rows: [ShareTargetRow]

  /// Most rows kept. The extension's memory is small, and a picker is about recent conversations;
  /// anything older is one tap away in the app ("Choose in TeamCraft").
  static let maxRows = 300

  /// The snapshot of `rows` in the core's order: all notes, then the most recent up to `maxRows`.
  static func make(owner: String, rows: [ShareTargetRow]) -> ShareTargetsSnapshot {
    let notes = rows.filter(\.isNotes)
    let others = rows.filter { !$0.isNotes }.prefix(max(0, maxRows - notes.count))
    return ShareTargetsSnapshot(owner: owner, rows: Array(notes + others))
  }

  /// The rows the extension may show while the inbox belongs to `maybeInboxOwner`: none when
  /// somebody else wrote the list (an account switch the app hasn't seen yet) or nobody is known.
  func rows(forInboxOwner maybeInboxOwner: String?) -> [ShareTargetRow] {
    guard let inboxOwner = maybeInboxOwner, inboxOwner == owner else { return [] }
    return rows
  }
}

/// One conversation of the picker.
struct ShareTargetRow: Codable, Equatable, Identifiable, Sendable {
  enum Kind: String, Codable, Sendable {
    case notes, channel, direct
  }

  let conversationId: String
  /// As the chat list names it ("Notes" in the user's language for the notes to self).
  let name: String
  let kind: Kind
  /// The space of a channel, as a second line; `nil` for direct messages and the notes.
  let maybeSpaceName: String?
  let maybeLastActivityMs: Int64?

  var id: String { conversationId }
  var isNotes: Bool { kind == .notes }

  /// The SF Symbol of the row: a bookmark for the notes, as in the chat list (#1036).
  var symbol: String {
    switch kind {
    case .notes: return "bookmark"
    case .channel: return "number"
    case .direct: return "person"
    }
  }
}

/// The picker's sections from the core's order: the notes and the most recently active on top, the
/// rest by name; while searching, every match once by name. The rule of the app's own picker
/// (`ShareSections`), on the snapshot's rows.
struct ShareTargetSections: Equatable {
  static let recentCount = 5
  let top: [ShareTargetRow]
  let others: [ShareTargetRow]

  init(rows: [ShareTargetRow], filter: String) {
    let needle = filter.trimmingCharacters(in: .whitespaces)
    let byName = rows.sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    if !needle.isEmpty {
      top = []
      others = byName.filter {
        $0.name.localizedCaseInsensitiveContains(needle)
          || ($0.maybeSpaceName?.localizedCaseInsensitiveContains(needle) ?? false)
      }
      return
    }
    let notes = rows.filter(\.isNotes)
    let recent = rows.filter { !$0.isNotes && $0.maybeLastActivityMs != nil }.prefix(Self.recentCount)
    let shown = notes + recent
    top = Array(shown)
    others = byName.filter { row in !shown.contains { $0.id == row.id } }
  }
}

/// What the extension adds to a copied share before the app takes it: the picked conversation and
/// the caption, which goes first in the composer's text.
enum ShareHandoff {
  /// Most characters of a caption kept, like the 64 KiB of text Android takes from a share.
  static let maxCaptionLength = 16_000

  static func apply(destination maybeDestination: String?, caption: String, to share: StagedShare) -> StagedShare {
    var updated = share
    updated.maybeConversationId = maybeDestination
    let trimmed = String(caption.trimmingCharacters(in: .whitespacesAndNewlines).prefix(maxCaptionLength))
    if !trimmed.isEmpty {
      updated.text = share.text.isEmpty ? trimmed : trimmed + "\n" + share.text
    }
    return updated
  }
}

/// The snapshot file in the App Group. Readable after the first unlock like the share inbox (a
/// share can be made from another app's flow); not in backups.
struct ShareTargetsStore {
  private static let fileName = "share-targets.json"
  let maybeURL: URL?
  private let fileManager: FileManager

  init(url: URL?, fileManager: FileManager = .default) {
    self.maybeURL = url
    self.fileManager = fileManager
  }

  /// The file in the App Group; without it (an unsigned simulator build) nothing is written or read.
  static func shared(environment: AppEnvironment = .current, fileManager: FileManager = .default) -> ShareTargetsStore {
    let maybeGroup = fileManager.containerURL(forSecurityApplicationGroupIdentifier: environment.appGroupId)
    return ShareTargetsStore(url: maybeGroup?.appendingPathComponent(Self.fileName), fileManager: fileManager)
  }

  /// `nil` when there is none (signed out, app lock on, never written) or it can't be read.
  func read() -> ShareTargetsSnapshot? {
    guard let url = maybeURL, let data = try? Data(contentsOf: url) else { return nil }
    return try? JSONDecoder().decode(ShareTargetsSnapshot.self, from: data)
  }

  func write(_ snapshot: ShareTargetsSnapshot) throws {
    guard var url = maybeURL else { return }
    try fileManager.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
    let data = try JSONEncoder().encode(snapshot)
    try data.write(to: url, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
    var values = URLResourceValues()
    values.isExcludedFromBackup = true
    try? url.setResourceValues(values)
  }

  func delete() {
    guard let url = maybeURL else { return }
    try? fileManager.removeItem(at: url)
  }
}

/// What the app does with a waiting share that names its conversation (picked in the extension).
enum ShareDestinationRoute: Equatable {
  /// Straight into that conversation's composer.
  case open(String)
  /// The conversation is gone or can't be posted in any more: the app's picker asks again.
  case pick
  /// The conversations haven't arrived yet (first catch-up after a cold start): wait a little.
  case wait

  /// `targetIds` are the conversations the user may post in now; an empty list right after the
  /// start means "not known yet" until `hasWaitedEnough`, then the picker decides.
  static func decide(destination: String, targetIds: Set<String>, hasWaitedEnough: Bool) -> ShareDestinationRoute {
    if targetIds.contains(destination) { return .open(destination) }
    if targetIds.isEmpty && !hasWaitedEnough { return .wait }
    return .pick
  }
}
