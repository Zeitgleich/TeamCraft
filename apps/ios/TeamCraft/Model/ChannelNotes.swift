import Foundation
import SwiftUI

/// A channel's shared notes, read-only on the phone (issue #1456, e2ee.md §4.11). The core builds the
/// page from the updates this device holds (`canvas_page`); the app only words and draws it.
enum ChannelNotes {
  /// Pages of older history read per opening, as on desktop and web ("A device catching up").
  static let maxHistoryPages = 20

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
  /// once more by the shared rule before they become tappable.
  static func attributed(_ runs: [CanvasRunItem]) -> AttributedString {
    var whole = AttributedString()
    for run in runs {
      var piece = AttributedString(run.text)
      var intent: InlinePresentationIntent = []
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
}
