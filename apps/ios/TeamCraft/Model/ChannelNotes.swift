import Foundation
import SwiftUI

/// A channel's shared notes, read-only on the phone (issue #1456, e2ee.md §4.11). The core builds the
/// page from the updates this device holds (`canvas_page`); the app only words and draws it.
enum ChannelNotes {
  /// Pages of older history read per opening, as on desktop and web ("A device catching up").
  static let maxHistoryPages = 20
  /// How often the shown page asks who else has it open: an entry runs out without an event, and the
  /// core says "open" on its refresh ("Who has the page open").
  static let presenceCheck = Duration.seconds(10)
  /// Avatars in the presence line at most; the text names everyone.
  static let maxPresenceAvatars = 5

  /// "Anna has this page open · Ben is editing"; empty when nobody else is on the page.
  static func presenceText(_ presence: CanvasPresenceItem) -> String {
    var parts: [String] = []
    if !presence.openBy.isEmpty {
      let names = presence.openBy.map(\.displayName).joined(separator: ", ")
      parts.append(
        presence.openBy.count == 1
          ? String(localized: "channel_notes_open_by_one \(names)")
          : String(localized: "channel_notes_open_by_other \(names)"))
    }
    if !presence.editing.isEmpty {
      let names = presence.editing.map(\.displayName).joined(separator: ", ")
      parts.append(
        presence.editing.count == 1
          ? String(localized: "channel_notes_editing_one \(names)")
          : String(localized: "channel_notes_editing_other \(names)"))
    }
    return parts.joined(separator: " · ")
  }

  /// The page's line of state: unreadable, loading or missing earlier changes; `nil` when all is here.
  static func statusKey(_ page: CanvasPageItem, history: OlderHistory) -> String? {
    if page.isUnreadable { return "channel_notes_unreadable" }
    switch history {
    case .loading: return "channel_notes_loading_history"
    case .failed: return "channel_notes_history_failed"
    case .available, .complete: break
    }
    return page.isIncomplete ? "channel_notes_incomplete" : nil
  }

  /// A block's text with its formatting. Links are web addresses only (the core's rule), checked
  /// once more by the shared rule before they become tappable. A mention (#2361) is bold in the
  /// accent colour, one of `maybeMeId` highlighted as in messages.
  static func attributed(_ runs: [CanvasRunItem], maybeMeId: String? = nil) -> AttributedString {
    var whole = AttributedString()
    for run in runs {
      var piece = AttributedString(run.text)
      var intent: InlinePresentationIntent = []
      if let mentioned = run.maybeMention {
        intent.insert(.stronglyEmphasized)
        if mentioned == maybeMeId {
          piece.foregroundColor = Theme.mentionText
          piece.backgroundColor = Theme.mentionBackground
        } else {
          piece.foregroundColor = Theme.accent
        }
      }
      if run.isBold { intent.insert(.stronglyEmphasized) }
      if run.isItalic { intent.insert(.emphasized) }
      if run.isStrike { intent.insert(.strikethrough) }
      if run.isCode { intent.insert(.code) }
      if !intent.isEmpty { piece.inlinePresentationIntent = intent }
      if let link = run.maybeLink, isOpenableLink(url: link), let url = URL(string: link) {
        piece.link = url
      }
      whole += piece
    }
    return whole
  }

  /// Where the first block that mentions `userId` is on the page, if one does (#2361).
  static func firstMention(of userId: String, in page: CanvasPageItem) -> String? {
    page.blocks.first { block in block.runs.contains { $0.maybeMention == userId } }?.id
  }

  /// The block's text without formatting (a code block, a file's name).
  static func plain(_ runs: [CanvasRunItem]) -> String {
    runs.map(\.text).joined()
  }

  /// The file a file block names, if this device holds the message it was sent with.
  static func file(of block: CanvasBlockItem, in files: [FileInfo]) -> FileInfo? {
    guard let ref = block.maybeFile else { return nil }
    return files.first { $0.fileId == ref.fileId && $0.message == ref.messageId }
  }
}

extension AppModel {
  /// A page of the channel's notes: the first one (`nil`) or a further one; `nil` when the core
  /// can't answer.
  func canvasPage(_ conversationId: String, pageId: String?) async -> CanvasPageItem? {
    try? await core().canvasPage(conversationId: conversationId, maybePageId: pageId)
  }

  /// Who else has the page open and who edited it lately; says this device has it open when due
  /// (e2ee.md §4.11, "Who has the page open"). `isShared` is "Show when I'm typing".
  func canvasPresence(_ conversationId: String, pageId: String?, isShared: Bool) async -> CanvasPresenceItem? {
    try? await core().canvasPresence(conversationId: conversationId, maybePageId: pageId, isShared: isShared)
  }

  /// The page is no longer shown: the others drop this device at once instead of after 50 s.
  func leaveCanvasPage(_ conversationId: String, pageId: String?, isShared: Bool) async {
    // Nothing to tell the user: without a connection the others drop the phone after 50 s anyway.
    _ = try? await core().leaveCanvasPage(conversationId: conversationId, maybePageId: pageId, isShared: isShared)
  }
}
