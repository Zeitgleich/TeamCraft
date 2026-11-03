import Foundation

/// The part of a conversation a feed shows while the reader pages back (issue #1058). The core
/// reads the newest messages down to the oldest one shown, so a reread after a change keeps the rows
/// paged to and new messages don't push old ones out; `older()` adds the next older page. Reads run
/// one at a time: a reread that started before a page back can't drop that page again.
@MainActor
final class PagedFeed {
  typealias Read = @MainActor (_ maybeOldest: MessageItem?, _ older: UInt32) async -> MessagePage?

  private let firstPage: UInt32
  private let pageBack: UInt32
  private let read: Read
  private var maybeOldest: MessageItem?
  private var maybeLastRead: Task<MessagePage?, Never>?

  init(firstPage: UInt32 = 200, pageBack: UInt32 = 100, read: @escaping Read) {
    self.firstPage = firstPage
    self.pageBack = pageBack
    self.read = read
  }

  /// The messages shown so far, read again (the newest `firstPage` before the first read).
  func reread() async -> MessagePage? {
    await queue { [self] in maybeOldest == nil ? firstPage : 0 }
  }

  /// The messages shown so far and the next older page.
  func older() async -> MessagePage? {
    await queue { [self] in pageBack }
  }

  private func queue(_ older: @escaping @MainActor () -> UInt32) async -> MessagePage? {
    let maybePrevious = maybeLastRead
    let next = Task { [self] () -> MessagePage? in
      _ = await maybePrevious?.value
      // A failed read keeps the anchor; the next change or scroll asks again.
      guard let page = await read(maybeOldest, older()) else { return nil }
      maybeOldest = page.messages.first
      return page
    }
    maybeLastRead = next
    return await next.value
  }
}
