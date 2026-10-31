import XCTest

@testable import TeamCraft

@MainActor
final class PagedFeedTests: XCTestCase {
  private struct Read: Equatable {
    let maybeOldestId: String?
    let older: UInt32
  }

  func testTheFirstReadAsksForTheNewestPage() async {
    var reads: [Read] = []
    let feed = PagedFeed(firstPage: 200, pageBack: 100) { maybeOldest, older in
      reads.append(Read(maybeOldestId: maybeOldest?.id, older: older))
      return self.page("5")
    }
    _ = await feed.reread()
    XCTAssertEqual(reads, [Read(maybeOldestId: nil, older: 200)])
  }

  func testRereadsKeepTheOldestRowShown() async {
    var reads: [Read] = []
    let feed = PagedFeed(firstPage: 200, pageBack: 100) { maybeOldest, older in
      reads.append(Read(maybeOldestId: maybeOldest?.id, older: older))
      return self.page("5", "6")
    }
    _ = await feed.reread()
    _ = await feed.reread()
    XCTAssertEqual(reads, [Read(maybeOldestId: nil, older: 200), Read(maybeOldestId: "5", older: 0)])
  }

  func testPagingBackStartsFromTheOldestRowAndMovesIt() async {
    var reads: [Read] = []
    var oldest = "5"
    let feed = PagedFeed(firstPage: 200, pageBack: 100) { maybeOldest, older in
      reads.append(Read(maybeOldestId: maybeOldest?.id, older: older))
      defer { oldest = "1" }
      return self.page(oldest)
    }
    _ = await feed.reread()
    _ = await feed.older()
    _ = await feed.reread()
    XCTAssertEqual(
      reads,
      [Read(maybeOldestId: nil, older: 200), Read(maybeOldestId: "5", older: 100), Read(maybeOldestId: "1", older: 0)])
  }

  func testAFailedReadKeepsTheAnchor() async {
    var reads: [Read] = []
    var fails = false
    let feed = PagedFeed(firstPage: 200, pageBack: 100) { maybeOldest, older in
      reads.append(Read(maybeOldestId: maybeOldest?.id, older: older))
      return fails ? nil : self.page("5")
    }
    _ = await feed.reread()
    fails = true
    _ = await feed.older()
    fails = false
    _ = await feed.reread()
    XCTAssertEqual(
      reads,
      [Read(maybeOldestId: nil, older: 200), Read(maybeOldestId: "5", older: 100), Read(maybeOldestId: "5", older: 0)])
  }

  private func page(_ ids: String...) -> MessagePage {
    MessagePage(messages: ids.map(message), hasOlder: true)
  }

  private func message(_ id: String) -> MessageItem {
    MessageItem(
      id: id, conversationId: "c", authorId: "a", authorName: "Name a", sentAtMs: Int64(id) ?? 0, body: "text \(id)",
      isEdited: false, isPending: false, replyCount: 0, maybeThreadId: nil, reactions: [])
  }
}
