import XCTest

@testable import TeamCraft

/// The message list's pure parts (issue #2180): rows and sections from a page, the rows on screen,
/// which cells change, and the scroll arithmetic that makes a jump land.
final class MessageListTests: XCTestCase {
  private let minute: Int64 = 60_000
  /// 2026-10-21 08:00 UTC.
  private let dayOne: Int64 = 1_792_569_600_000
  private let dayMs: Int64 = 86_400_000

  // MARK: Feed

  func testOneSectionPerDayAfterTheTop() {
    let feed = messageFeed(
      [message("1", dayOne), message("2", dayOne + minute), message("3", dayOne + dayMs)], gaps: [], revision: 1,
      calendar: utc)
    let sections = feed.sections(top: MessageListTop())
    XCTAssertEqual(sections.map(\.id).first, .top)
    XCTAssertEqual(sections.dropFirst().map(\.items), [[.message("1"), .message("2")], [.message("3")]])
    XCTAssertEqual(feed.ids, ["1", "2", "3"])
    XCTAssertEqual(feed.positions["3"], 2)
    XCTAssertEqual(feed.revision, 1)
  }

  func testTopRowsComeBeforeTheHolesAboveTheFirstMessage() {
    let feed = messageFeed(
      [message("1", dayOne)], gaps: [HistoryGap(after: "0", index: 0, status: .available)], revision: 1,
      calendar: utc)
    let top = MessageListTop(showsSkeleton: false, older: .failed)
    XCTAssertEqual(feed.sections(top: top).first?.items, [.olderFailed, .gap(after: "0")])
    XCTAssertEqual(MessageListTop(showsSkeleton: true, older: .loading).items, [.skeleton, .olderLoading])
  }

  func testAHoleFollowsTheMessageBeforeIt() {
    let feed = messageFeed(
      [message("1", dayOne), message("2", dayOne + minute)],
      gaps: [HistoryGap(after: "1", index: 1, status: .loading)], revision: 1, calendar: utc)
    XCTAssertEqual(feed.days.first?.items, [.message("1"), .gap(after: "1"), .message("2")])
    XCTAssertEqual(feed.position(of: .gap(after: "1")), 0)
  }

  func testRepeatedRowsAndDaysNeverReachTheDataSourceTwice() {
    // A message sent with a wrong clock lies between two of another day; an ID read twice.
    let feed = messageFeed(
      [message("1", dayOne), message("2", dayOne + dayMs), message("3", dayOne + minute), message("1", dayOne)],
      gaps: [], revision: 1, calendar: utc)
    let sections = feed.sections(top: MessageListTop())
    XCTAssertEqual(Set(sections.map(\.id)).count, sections.count)
    let items = sections.flatMap(\.items)
    XCTAssertEqual(Set(items).count, items.count)
    XCTAssertEqual(feed.ids, ["1", "2", "3"])
  }

  func testAnArrivalIsANewestRowAddedWithNothingElseMoved() {
    let old = messageFeed([message("1", dayOne)], gaps: [], revision: 1, calendar: utc)
    let arrived = messageFeed([message("1", dayOne), message("2", dayOne + minute)], gaps: [], revision: 2, calendar: utc)
    let paged = messageFeed([message("0", dayOne - minute), message("1", dayOne)], gaps: [], revision: 2, calendar: utc)
    XCTAssertTrue(arrived.isArrival(after: old))
    XCTAssertFalse(paged.isArrival(after: old))
    XCTAssertFalse(old.isArrival(after: MessageFeed()))
  }

  func testTheLiveWindowIsClampedToThePage() {
    let feed = messageFeed((0..<10).map { message("\($0)", dayOne + Int64($0) * minute) }, gaps: [], revision: 1, calendar: utc)
    var visible = VisibleRows()
    XCTAssertEqual(Array(feed.ids(around: visible, margin: 2)), ["7", "8", "9"])
    visible = VisibleRows([.message("4"), .message("5")], in: feed)
    XCTAssertEqual(Array(feed.ids(around: visible, margin: 2)), ["2", "3", "4", "5", "6", "7"])
    XCTAssertEqual(Array(MessageFeed().ids(around: visible, margin: 2)), [])
  }

  // MARK: Rows on screen

  func testRowsOnScreenFollowTheirMessagesIntoANewerPage() {
    let first = messageFeed((10..<20).map { message("\($0)", dayOne + Int64($0) * minute) }, gaps: [], revision: 1, calendar: utc)
    let rows = VisibleRows([.message("10"), .message("11")], in: first)
    XCTAssertEqual(rows.maybeFirst, 0)
    XCTAssertTrue(rows.isNearOldest(in: first, ahead: 5))
    // A page of 10 older messages came above: the same rows are 10 further down now.
    let paged = messageFeed((0..<20).map { message("\($0)", dayOne + Int64($0) * minute) }, gaps: [], revision: 2, calendar: utc)
    XCTAssertEqual(rows.range(in: paged), 10...11)
    XCTAssertFalse(rows.isNearOldest(in: paged, ahead: 5))
    XCTAssertFalse(VisibleRows().isNearOldest(in: paged, ahead: 5))
  }

  // MARK: Changed rows

  func testOnlyRowsWhoseLookChangedAreConfiguredAgain() {
    let feed = messageFeed([message("1", dayOne), message("2", dayOne + minute)], gaps: [], revision: 1, calendar: utc)
    let old = (feed: feed, marks: MessageRowMarks(maybeHighlighted: "1"))
    XCTAssertEqual(MessageRowMarks.changed(["1", "2"], from: old, to: old), [])
    XCTAssertEqual(
      MessageRowMarks.changed(["1", "2"], from: old, to: (feed, MessageRowMarks(picked: ["2"]))), ["1", "2"])
    XCTAssertEqual(
      MessageRowMarks.changed(["1", "2"], from: old, to: (feed, MessageRowMarks(maybeHighlighted: "1", maybeFirstUnread: "2"))),
      ["2"])
    let edited = messageFeed([message("1", dayOne), message("2", dayOne + minute, body: "edited")], gaps: [], revision: 2, calendar: utc)
    XCTAssertEqual(MessageRowMarks.changed(["1", "2"], from: old, to: (edited, old.marks)), ["2"])
  }

  // MARK: Scrolling

  func testTheBottomRestsTheLastRowAboveTheInset() {
    XCTAssertEqual(MessageListScroll.bottomOffset(contentHeight: 2_000, viewHeight: 800, insetTop: 0, insetBottom: 100), 1_300)
    // Less than a screen of rows: they start at the top.
    XCTAssertEqual(MessageListScroll.bottomOffset(contentHeight: 200, viewHeight: 800, insetTop: 10, insetBottom: 100), -10)
    XCTAssertTrue(MessageListScroll.isAtBottom(offset: 1_280, bottomOffset: 1_300))
    XCTAssertFalse(MessageListScroll.isAtBottom(offset: 1_200, bottomOffset: 1_300))
  }

  func testARowScrolledToIsClampedToTheContent() {
    let top = MessageListScroll.offset(
      showing: 1_000, 1_100, at: .top, viewHeight: 800, insetTop: 0, insetBottom: 100, bottomOffset: 5_000)
    XCTAssertEqual(top, 1_000 - MessageListScroll.topMargin)
    let center = MessageListScroll.offset(
      showing: 1_000, 1_100, at: .center, viewHeight: 800, insetTop: 0, insetBottom: 100, bottomOffset: 5_000)
    XCTAssertEqual(center, 1_050 - 350)
    // Near the end the row can't come higher than the bottom allows; near the start not lower.
    XCTAssertEqual(
      MessageListScroll.offset(showing: 4_900, 5_000, at: .top, viewHeight: 800, insetTop: 0, insetBottom: 100, bottomOffset: 4_300),
      4_300)
    XCTAssertEqual(
      MessageListScroll.offset(showing: 10, 50, at: .center, viewHeight: 800, insetTop: 0, insetBottom: 100, bottomOffset: 4_300),
      0)
  }

  func testAPageAboveKeepsTheRowTheReaderLooksAt() {
    // The row was 40 pt below the top at offset 500; 3 000 pt of older rows came above it.
    XCTAssertEqual(MessageListScroll.keepingAnchor(offset: 500, anchorBefore: 540, anchorAfter: 3_540), 3_500)
  }

  func testOnlyARowAboveTheReaderMovesTheScrollPosition() {
    XCTAssertEqual(MessageListScroll.adjustment(rowMinY: 100, oldHeight: 72, newHeight: 200, visibleTop: 500), 128)
    // Straddling the top edge: its growth pushes what the reader sees too.
    XCTAssertEqual(MessageListScroll.adjustment(rowMinY: 480, oldHeight: 72, newHeight: 40, visibleTop: 500), -32)
    XCTAssertEqual(MessageListScroll.adjustment(rowMinY: 600, oldHeight: 72, newHeight: 200, visibleTop: 500), 0)
  }

  // MARK: Frames

  func testFrameStatsCountStalls() {
    var stats = FrameStats()
    for frame in [16.7, 16.7, 120, 51, 33] { stats.add(frameMs: frame) }
    XCTAssertEqual(stats.frames, 5)
    XCTAssertEqual(stats.slowFrames, 2)
    XCTAssertEqual(stats.longestFrameMs, 120)
  }

  // MARK: Notified message (#1065, #2374)

  func testTheNotifiedMessageShowsInItsPlaceUntilTheStoreHasIt() {
    let stored = [message("1", 100), message("3", 300)]
    let shown = withNotified(stored, notified(id: "2", sentAtMs: 200))
    XCTAssertEqual(shown.map(\.id), ["1", "2", "3"])
    XCTAssertEqual(shown[1].body, "lunch?")
    XCTAssertEqual(shown[1].authorName, "Sender")
    // A read before the catch-up (the client still opening) brings nothing: the row stays.
    XCTAssertEqual(withNotified([], notified(id: "2", sentAtMs: 200)).map(\.id), ["2"])
    // Once the store has it, the stored message takes its place.
    XCTAssertEqual(withNotified(stored + [message("2", 200)], notified(id: "2", sentAtMs: 200)).count, 3)
  }

  func testAThreadReplyOrAnUnreadableMessageAddsNoRow() {
    let stored = [message("1", 100)]
    XCTAssertEqual(withNotified(stored, notified(id: "2", sentAtMs: 200, threadId: "1")).map(\.id), ["1"])
    XCTAssertEqual(withNotified(stored, notified(id: "2", sentAtMs: 200, text: nil)).map(\.id), ["1"])
    XCTAssertEqual(withNotified(stored, nil).map(\.id), ["1"])
  }

  // MARK: Helpers

  private func notified(
    id: String, sentAtMs: Int64, threadId: String? = nil, text: String? = "lunch?"
  ) -> NotifiedConversation {
    NotifiedConversation(
      conversationId: "c", maybeName: "planning", isDirect: false,
      maybeMessage: NotifiedMessage(
        messageId: id, maybeSenderId: "s", maybeSenderName: "Sender", isSenderSource: false, maybeText: text,
        maybeFileName: nil, maybeThreadId: threadId, sentAtMs: sentAtMs))
  }

  private var utc: Calendar {
    var calendar = Calendar(identifier: .gregorian)
    calendar.timeZone = TimeZone(identifier: "UTC")!
    return calendar
  }

  private func message(_ id: String, _ sentAtMs: Int64, body: String = "text") -> Message {
    Message(id: id, conversationId: "c", authorId: "a", sentAtMs: sentAtMs, body: body, authorName: "Name a")
  }
}
