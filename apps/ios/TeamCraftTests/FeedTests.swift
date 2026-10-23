import XCTest

@testable import TeamCraft

final class FeedTests: XCTestCase {
  private let minute: Int64 = 60_000
  /// 2026-10-21 08:00 UTC.
  private let dayOne: Int64 = 1_792_569_600_000

  private var utc: Calendar {
    var calendar = Calendar(identifier: .gregorian)
    calendar.timeZone = TimeZone(identifier: "UTC")!
    return calendar
  }

  private func message(_ id: String, _ author: String, _ sentAtMs: Int64, body: String = "text") -> MessageItem {
    MessageItem(
      id: id, conversationId: "c", authorId: author, authorName: "Name \(author)", sentAtMs: sentAtMs, body: body,
      isEdited: false, isPending: false, replyCount: 0, maybeThreadId: nil, reactions: [])
  }

  func testDateChipBeforeEachDay() {
    let items = feedItems(
      [message("1", "a", dayOne), message("2", "a", dayOne + 60 * minute), message("3", "a", dayOne + 1440 * minute)],
      calendar: utc)
    XCTAssertEqual(items.count, 5)
    guard case .day = items[0], case .day = items[3] else { return XCTFail("date chips missing: \(items)") }
  }

  func testFollowUpsOfTheSameAuthorAreGrouped() {
    let items = feedItems(
      [
        message("1", "a", dayOne), message("2", "a", dayOne + minute), message("3", "b", dayOne + 2 * minute),
        message("4", "b", dayOne + 2 * minute + groupWindowMs + 1), message("5", "b", dayOne + 3 * minute + groupWindowMs, body: ""),
      ], calendar: utc)
    let continuations = items.compactMap { item -> Bool? in
      if case .entry(_, let isContinuation) = item { return isContinuation }
      return nil
    }
    XCTAssertEqual(continuations, [false, true, false, false, false])
  }

  func testPreviewNamesSenderOnlyWhereNeeded() {
    XCTAssertEqual(chatPreview(message("1", "a", dayOne), myId: "me", isOneToOne: false).sender, .other("Name a"))
    XCTAssertEqual(chatPreview(message("1", "a", dayOne), myId: "me", isOneToOne: true).sender, .none)
    XCTAssertEqual(chatPreview(message("1", "me", dayOne), myId: "me", isOneToOne: true).sender, .me)
    let event = chatPreview(message("1", "a", dayOne, body: ""), myId: "me", isOneToOne: false)
    XCTAssertEqual(event.content, .noText)
    XCTAssertEqual(event.sender, .none)
  }

  func testOneLineFoldsWhitespaceAndLimitsLength() {
    XCTAssertEqual(oneLine("  Line one\n\nline   two "), "Line one line two")
    XCTAssertEqual(oneLine(String(repeating: "x", count: 500)).count, 160)
  }

  func testLinksAreOnlyWebLinks() {
    let text = linkedText("See https://example.com/a, or mailto:someone@example.com")
    let links = text.runs.compactMap(\.link)
    XCTAssertEqual(links.map(\.absoluteString), ["https://example.com/a"])
  }

  func testDirectMessagesByRecency() {
    func dm(_ id: String, _ name: String) -> ConversationItem {
      ConversationItem(
        id: id, maybeSpaceId: nil, maybeSpaceName: nil, kind: .direct, name: name, topic: "", unreadCount: 0,
        mentionCount: 0, isFavorite: false, isMuted: false, canPost: true)
    }
    let sections = conversationSections(
      [dm("1", "Alex"), dm("2", "Bo"), dm("3", "Cy")],
      lastActivity: ["3": Date(timeIntervalSince1970: 30), "1": Date(timeIntervalSince1970: 10)])
    XCTAssertEqual(sections.first?.conversations.map(\.name), ["Cy", "Alex", "Bo"])
  }

  func testReplyAndMarkUnreadSerialize() throws {
    let request = SendRequest(conversationId: "1", maybeThreadId: nil, body: "hi", maybeReplyToId: "9")
    XCTAssertEqual(request.object["maybe_reply_to"] as? String, "9")
    XCTAssertTrue(SendRequest(conversationId: "1", body: "hi").object["maybe_reply_to"] is NSNull)
    let unread = try JSONSerialization.jsonObject(
      with: Data(ChatAction.markUnread(conversationId: "1", fromMessageId: "9").json.utf8)) as? [String: String]
    XCTAssertEqual(unread, ["action": "mark_unread", "conversation": "1", "from": "9"])
  }

  func testAppLockClock() {
    var clock = AppLockClock()
    let start = ContinuousClock.now
    XCTAssertFalse(clock.shouldLock(at: start, isEnabled: true, lockAfter: .immediately), "no background phase")
    clock.didEnterBackground(at: start)
    XCTAssertFalse(clock.shouldLock(at: start.advanced(by: .seconds(59)), isEnabled: true, lockAfter: .oneMinute))
    clock.didEnterBackground(at: start)
    XCTAssertTrue(clock.shouldLock(at: start.advanced(by: .seconds(60)), isEnabled: true, lockAfter: .oneMinute))
    XCTAssertFalse(clock.shouldLock(at: start.advanced(by: .seconds(60)), isEnabled: true, lockAfter: .oneMinute), "used up")
    clock.didEnterBackground(at: start)
    XCTAssertFalse(clock.shouldLock(at: start.advanced(by: .seconds(3600)), isEnabled: false, lockAfter: .immediately))
    // Going backwards (impossible on this clock) locks rather than not.
    clock.didEnterBackground(at: start.advanced(by: .seconds(100)))
    XCTAssertTrue(clock.shouldLock(at: start, isEnabled: true, lockAfter: .fifteenMinutes))
  }
}
