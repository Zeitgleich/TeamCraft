import Foundation

/// The part of a conversation a feed shows while the reader pages back (issue #1058). The core
/// reads the newest messages down to the oldest one shown, so a reread after a change keeps the rows
/// paged to and new messages don't push old ones out; `older()` adds the next older page. Reads run
/// one at a time: a reread that started before a page back can't drop that page again.
@MainActor
final class PagedFeed {
  typealias Read = @MainActor (_ maybeOldest: Message?, _ older: UInt32) async -> MessagePage?

  private let firstPage: UInt32
  private let pageBack: UInt32
  private let read: Read
  private var maybeOldest: Message?
  private var maybeLastRead: Task<MessagePage?, Never>?
  /// Waits between tries of a failed read (tests don't wait).
  private let sleep: @Sendable (Duration) async throws -> Void

  init(
    firstPage: UInt32 = 60, pageBack: UInt32 = 60,
    sleep: @escaping @Sendable (Duration) async throws -> Void = { try await Task.sleep(for: $0) },
    read: @escaping Read
  ) {
    self.firstPage = firstPage
    self.pageBack = pageBack
    self.sleep = sleep
    self.read = read
  }

  /// The messages shown so far, read again (the newest `firstPage` before the first read). A failed
  /// read is tried again up to `retries` times, a little later each time (the store may be opening
  /// again after the background just now, #2206); not once the calling task is cancelled.
  func reread(retries: Int = 0) async -> MessagePage? {
    for attempt in 0...max(retries, 0) {
      if attempt > 0 {
        guard (try? await sleep(.milliseconds(250 * attempt))) != nil else { return nil }
      }
      if let page = await queue({ [self] in maybeOldest == nil ? firstPage : 0 }) { return page }
      if Task.isCancelled { return nil }
    }
    return nil
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
