import Foundation

/// What the home screen widget shows (issue #2076): the unread conversations as the app saw them
/// last, already cut down to what the privacy settings allow. Only what may show is written, so a
/// stricter setting never leaves names or text in the file. Shared by the app (which writes it) and
/// the widget extension (which reads it, and links no core).
struct WidgetSnapshot: Codable, Equatable, Sendable {
  let privacy: WidgetPrivacy
  /// Conversations with unread messages, also those not listed (hidden names, over the limit).
  let unreadConversations: Int
  /// Empty unless `privacy` shows names.
  let rows: [WidgetRow]
}

/// How much the widget may show (`HomeWidget.privacy`).
enum WidgetPrivacy: String, Codable, Sendable {
  /// The app lock is on: nothing personal, not even the number of conversations.
  case locked
  /// Notifications show nothing: only how many conversations wait.
  case countOnly
  /// The default: names and unread counts.
  case names
  /// Names, counts and the newest message, when notifications show content and the user asked.
  case previews
}

/// One unread conversation.
struct WidgetRow: Codable, Equatable, Identifiable, Sendable {
  let conversationId: String
  let name: String
  /// The count as the chat list labels it ("99+").
  let countLabel: String
  /// The newest message, one line with its sender; only with `WidgetPrivacy.previews`.
  let maybePreview: String?

  var id: String { conversationId }
}

/// Where the widget's taps lead, the rules of what it shows, and its kind.
enum HomeWidget {
  /// The widget's kind, for `WidgetCenter.reloadTimelines(ofKind:)`.
  static let kind = "de.zeitgleich.teamcraft.unread"
  /// Most rows kept; a home screen shows a handful.
  static let maxRows = 20

  private static let scheme = "teamcraft"
  private static let host = "widget"

  /// The widget's privacy: the app lock hides everything personal; otherwise the notification
  /// setting decides, and previews need the widget's own opt-in on top of notifications that show
  /// content (docs/notifications.md, "Home screen widget").
  static func privacy(showsNames: Bool, showsContent: Bool, isAppLockOn: Bool, isPreviewWanted: Bool) -> WidgetPrivacy {
    if isAppLockOn { return .locked }
    if !showsNames { return .countOnly }
    return showsContent && isPreviewWanted ? .previews : .names
  }

  /// The snapshot of `rows` (all unread, in the chat list's order) under `privacy`.
  static func snapshot(privacy: WidgetPrivacy, rows: [WidgetRow]) -> WidgetSnapshot {
    switch privacy {
    case .locked:
      return WidgetSnapshot(privacy: privacy, unreadConversations: 0, rows: [])
    case .countOnly:
      return WidgetSnapshot(privacy: privacy, unreadConversations: rows.count, rows: [])
    case .names:
      let named = rows.prefix(maxRows).map {
        WidgetRow(conversationId: $0.conversationId, name: $0.name, countLabel: $0.countLabel, maybePreview: nil)
      }
      return WidgetSnapshot(privacy: privacy, unreadConversations: rows.count, rows: named)
    case .previews:
      return WidgetSnapshot(privacy: privacy, unreadConversations: rows.count, rows: Array(rows.prefix(maxRows)))
    }
  }

  /// Opens a conversation the way its notification does (`AppModel.openFromWidget`).
  static func conversationURL(_ conversationId: String) -> URL {
    var components = URLComponents()
    components.scheme = scheme
    components.host = host
    components.path = "/conversation/\(conversationId)"
    // Invariant: scheme, host and an ID of digits always make a valid URL.
    return components.url!
  }

  // Invariant (both): fixed strings that are valid URLs.
  /// "New message", like the app icon's quick action (#2061).
  static let newMessageURL = URL(string: "teamcraft://widget/new-message")!

  /// Opens the app where it was.
  static let appURL = URL(string: "teamcraft://widget/open")!

  /// Where a widget URL leads; `nil` for any other URL (an app link).
  static func route(of url: URL) -> WidgetRoute? {
    guard url.scheme == scheme, url.host == host else { return nil }
    let parts = url.pathComponents.filter { $0 != "/" }
    switch parts.first {
    case "conversation":
      // Snowflake IDs: positive 64-bit numbers. Anything else never reaches navigation.
      guard parts.count == 2, let id = parts.last, (1...19).contains(id.count), id.allSatisfy(\.isASCIIDigit) else {
        return .app
      }
      return .conversation(id)
    case "new-message": return .newMessage
    default: return .app
    }
  }
}

/// A widget tap.
enum WidgetRoute: Equatable, Sendable {
  case conversation(String)
  case newMessage
  case app
}

/// The snapshot file in the App Group. Readable after the first unlock, like the store the
/// notification extension reads, so the widget can draw while the phone is locked in StandBy; not
/// in backups. Deleted at sign-out.
struct WidgetSnapshotStore {
  private static let fileName = "widget.json"
  private let fileManager: FileManager
  private let appGroupId: String

  init(appGroupId: String = AppEnvironment.current.appGroupId, fileManager: FileManager = .default) {
    self.appGroupId = appGroupId
    self.fileManager = fileManager
  }

  /// `nil` when there is none (signed out, never written) or it can't be read.
  func read() -> WidgetSnapshot? {
    guard let url = fileURL, let data = try? Data(contentsOf: url) else { return nil }
    return try? JSONDecoder().decode(WidgetSnapshot.self, from: data)
  }

  func write(_ snapshot: WidgetSnapshot) throws {
    guard var url = fileURL else { return }
    let data = try JSONEncoder().encode(snapshot)
    try data.write(to: url, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
    var values = URLResourceValues()
    values.isExcludedFromBackup = true
    try? url.setResourceValues(values)
  }

  func delete() {
    guard let url = fileURL else { return }
    try? fileManager.removeItem(at: url)
  }

  /// Only in the App Group: without it (an unsigned simulator build) the widget can't read it anyway.
  private var fileURL: URL? {
    fileManager.containerURL(forSecurityApplicationGroupIdentifier: appGroupId)?
      .appendingPathComponent(Self.fileName)
  }
}

extension Character {
  fileprivate var isASCIIDigit: Bool { ("0"..."9").contains(self) }
}
