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

  func testOneSectionPerDay() {
    let sections = feedSections(
      [message("1", "a", dayOne), message("2", "a", dayOne + 60 * minute), message("3", "a", dayOne + 1440 * minute)],
      calendar: utc)
    XCTAssertEqual(sections.map { $0.entries.map(\.message.id) }, [["1", "2"], ["3"]])
  }

  func testFollowUpsOfTheSameAuthorAreGrouped() {
    let entries = feedSections(
      [
        message("1", "a", dayOne), message("2", "a", dayOne + minute), message("3", "b", dayOne + 2 * minute),
        message("4", "b", dayOne + 2 * minute + groupWindowMs + 1), message("5", "b", dayOne + 3 * minute + groupWindowMs, body: ""),
      ], calendar: utc
    ).flatMap(\.entries)
    XCTAssertEqual(entries.map(\.isContinuation), [false, true, false, false, false])
    // The group's last bubble carries the avatar.
    XCTAssertEqual(entries.map(\.isGroupEnd), [false, true, true, true, true])
  }

  func testANewDayStartsANewGroup() {
    let entries = feedSections(
      [message("1", "a", dayOne + 959 * minute), message("2", "a", dayOne + 961 * minute)], calendar: utc
    ).flatMap(\.entries)
    XCTAssertEqual(entries.map(\.isContinuation), [false, false])
    XCTAssertEqual(entries.map(\.isGroupEnd), [true, true])
  }

  func testAHoleSitsAfterTheMessageBeforeItAndBreaksAGroup() {
    let messages = [message("1", "a", dayOne), message("2", "a", dayOne + minute), message("3", "a", dayOne + 2 * minute)]
    let gaps = [
      HistoryGapItem(after: "0", index: 0, status: .loading), HistoryGapItem(after: "1", index: 1, status: .available),
      HistoryGapItem(after: "9", index: 4, status: .available),
    ]
    let placed = PlacedGaps(gaps, among: messages)
    XCTAssertEqual(placed.above.map(\.after), ["0"])
    let entries = feedSections(messages, gaps: placed, calendar: utc).flatMap(\.entries)
    XCTAssertEqual(entries.map { $0.gapsAfter.map(\.after) }, [["1"], [], []], "beyond the page is left out")
    XCTAssertEqual(entries.map(\.isContinuation), [false, false, true])
    XCTAssertEqual(entries.map(\.isGroupEnd), [true, false, true])
  }

  func testAHoleNearTheScreenIsReadFromTheReadersSide() {
    // Rows 50 to 60 are on screen.
    let below = HistoryGapItem(after: "1", index: 70, status: .available)
    XCTAssertEqual(gapToFill([below], firstVisible: 50, lastVisible: 60, ahead: 40)?.edge, .older)
    let above = HistoryGapItem(after: "2", index: 20, status: .available)
    XCTAssertEqual(gapToFill([above], firstVisible: 50, lastVisible: 60, ahead: 40)?.edge, .newer)
    XCTAssertEqual(gapToFill([above], firstVisible: 20, lastVisible: 30, ahead: 40)?.edge, .newer, "right above the screen")
    XCTAssertNil(gapToFill([below], firstVisible: 0, lastVisible: 10, ahead: 40), "too far")
    for status in [HistoryGapStatus.loading, .failed] {
      let waiting = HistoryGapItem(after: "1", index: 55, status: status)
      XCTAssertNil(gapToFill([waiting], firstVisible: 50, lastVisible: 60, ahead: 40))
    }
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

  func testDirectMessagesByRecency() {
    func dm(_ id: String, _ name: String) -> ConversationItem {
      ConversationItem(
        id: id, maybeSpaceId: nil, maybeSpaceName: nil, kind: .direct, name: name, topic: "", unreadCount: 0,
        mentionCount: 0, isFavorite: false, isMuted: false, canPost: true)
    }
    let sections = directSections(
      [dm("1", "Alex"), dm("2", "Bo"), dm("3", "Cy")],
      lastActivity: ["3": Date(timeIntervalSince1970: 30), "1": Date(timeIntervalSince1970: 10)])
    XCTAssertEqual(sections.first?.conversations.map(\.name), ["Cy", "Alex", "Bo"])
  }

  func testReplyAndMarkUnreadSerialize() throws {
    let request = SendRequest(conversationId: "1", maybeThreadId: nil, body: "hi", maybeReplyToId: "9")
    XCTAssertEqual(request.object["maybe_reply_to"] as? String, "9")
    XCTAssertTrue(request.object["maybe_reply_attachment"] is NSNull)
    let onePicture = SendRequest(
      conversationId: "1", maybeThreadId: nil, body: "hi", maybeReplyToId: "9", maybeReplyAttachmentId: "4")
    XCTAssertEqual(onePicture.object["maybe_reply_attachment"] as? String, "4")
    XCTAssertTrue(SendRequest(conversationId: "1", body: "hi").object["maybe_reply_to"] is NSNull)
    let unread = try JSONSerialization.jsonObject(
      with: Data(ChatAction.markUnread(conversationId: "1", fromMessageId: "9").json.utf8)) as? [String: String]
    XCTAssertEqual(unread, ["action": "mark_unread", "conversation": "1", "from": "9"])
  }

  func testFailedSendActionsAndExtras() throws {
    let retry = try JSONSerialization.jsonObject(
      with: Data(ChatAction.retrySend(conversationId: "1", messageId: "9").json.utf8)) as? [String: String]
    XCTAssertEqual(retry, ["action": "retry_send", "conversation": "1", "message": "9"])
    let discard = try JSONSerialization.jsonObject(
      with: Data(ChatAction.discardUnsent(conversationId: "1", messageId: "9").json.utf8)) as? [String: String]
    XCTAssertEqual(discard, ["action": "discard_unsent", "conversation": "1", "message": "9"])
    let failed = Wire.decode(MessageExtras.self, from: #"{"message":"9","is_send_failed":true}"#)
    XCTAssertEqual(failed?.isSendFailed, true)
    XCTAssertNil(Wire.decode(MessageExtras.self, from: #"{"message":"9"}"#)?.isSendFailed)
  }

  func testTypingIsAnnouncedAtMostEveryFewSeconds() {
    let start = Date(timeIntervalSince1970: 1000)
    XCTAssertTrue(shouldNotifyTyping("h", now: start, lastNotified: nil))
    XCTAssertFalse(shouldNotifyTyping("he", now: start.addingTimeInterval(typingNotifyInterval - 0.1), lastNotified: start))
    XCTAssertTrue(shouldNotifyTyping("hel", now: start.addingTimeInterval(typingNotifyInterval), lastNotified: start))
    XCTAssertFalse(shouldNotifyTyping("  ", now: start, lastNotified: nil))
    XCTAssertNil(typingText([]))
    XCTAssertNotNil(typingText(["Alex", "Bo", "Cy"]))
    let typing = try? JSONSerialization.jsonObject(
      with: Data(ChatAction.typing(conversationId: "1").json.utf8)) as? [String: String]
    XCTAssertEqual(typing, ["action": "typing", "conversation": "1"])
  }

  func testTypingInAThreadCarriesTheThread() {
    let thread = try? JSONSerialization.jsonObject(
      with: Data(ChatAction.typing(conversationId: "1", maybeThreadId: "9").json.utf8)) as? [String: String]
    XCTAssertEqual(thread, ["action": "typing", "conversation": "1", "maybe_thread": "9"])
    XCTAssertEqual(ChatQuery.typingNow.json, #"{"kind":"typing_now"}"#)
  }

  func testTypingNowAnswerParsesPlaces() {
    let answer = Wire.decode(
      ChatAnswer.self,
      from: #"{"answer":"typing","typing":[{"conversation":"7","users":["1"]},{"conversation":"7","maybe_thread":"9","users":["2"]}]}"#)
    XCTAssertEqual(
      answer?.typing,
      [
        TypingPlace(conversation: "7", maybeThread: nil, users: ["1"]),
        TypingPlace(conversation: "7", maybeThread: "9", users: ["2"]),
      ])
  }

  func testTypingIsReadPerPlace() {
    let places = [
      TypingNow(conversation: "7", maybeThread: nil, names: ["Alex"]),
      TypingNow(conversation: "7", maybeThread: "9", names: ["Bo", "Cy"]),
    ]
    XCTAssertEqual(typingNames(in: places, conversation: "7", thread: nil), ["Alex"])
    XCTAssertEqual(typingNames(in: places, conversation: "7", thread: "9"), ["Bo", "Cy"])
    XCTAssertEqual(typingNames(in: places, conversation: "7", thread: "10"), [])
    XCTAssertEqual(typingNames(in: places, conversation: "8", thread: nil), [])
    // A direct message says "typing…"; a group names who types.
    XCTAssertNotNil(typingRowText(["Alex"], isDirect: true))
    XCTAssertNil(typingRowText([], isDirect: true))
  }

  func testTypingDotsPulseOneAfterTheOtherAndRest() {
    let rest = TypingDots.restAlpha / 2
    // Resting at the start of a cycle, the first dot peaks before the second.
    XCTAssertEqual(TypingDots.alpha(at: 1.2, dot: 0), rest, accuracy: 0.001)
    XCTAssertGreaterThan(TypingDots.alpha(at: 1.2 + 0.15, dot: 0), TypingDots.alpha(at: 1.2 + 0.15, dot: 1))
    for dot in 0..<TypingDots.count {
      for step in 0..<100 {
        let alpha = TypingDots.alpha(at: Double(step) * 0.037, dot: dot)
        XCTAssertGreaterThanOrEqual(alpha, rest - 0.0001)
        XCTAssertLessThanOrEqual(alpha, 1.0001)
      }
    }
  }

  func testMessageActionsSerialize() throws {
    func object(_ action: ChatAction) throws -> [String: Any]? {
      try JSONSerialization.jsonObject(with: Data(action.json.utf8)) as? [String: Any]
    }
    let forward = try object(.forward(fromConversationId: "1", messageId: "9", toConversationId: "2"))
    XCTAssertEqual(forward?["action"] as? String, "forward_message")
    XCTAssertEqual(forward?["to"] as? String, "2")
    XCTAssertNil(forward?["attachment"])
    let forwardOne = try object(
      .forwardAttachment(fromConversationId: "1", messageId: "9", fileId: "4", toConversationId: "2"))
    XCTAssertEqual(forwardOne?["action"] as? String, "forward_message")
    XCTAssertEqual(forwardOne?["attachment"] as? String, "4")
    XCTAssertEqual(try object(.setPinned(conversationId: "1", messageId: "9", isPinned: true))?["is_pinned"] as? Bool, true)
    XCTAssertEqual(try object(.setSaved(conversationId: "1", messageId: "9", isSaved: false))?["is_saved"] as? Bool, false)
    let extras = Wire.decode(
      MessageExtras.self,
      from: #"{"message":"9","is_pinned":true,"maybe_reply":{"author":"5","excerpt":"hi"},"edits":[{"body":"old","replaced_at":1}]}"#)
    XCTAssertEqual(extras?.isPinned, true)
    XCTAssertEqual(extras?.maybeReply?.excerpt, "hi")
    XCTAssertNil(extras?.maybeReply?.attachment)
    let quote = Wire.decode(
      ReplyPreview.self,
      from: #"{"maybe_message":"9","author":"5","excerpt":"c.jpg","attachment":{"file_id":"4","file_name":"c.jpg","thumbnail_png":[1],"can_open":true}}"#)
    XCTAssertEqual(quote?.attachment?.fileId, "4")
    XCTAssertEqual(quote?.attachment?.canOpen, true)
    XCTAssertEqual(quote?.attachment?.thumbnailPng, [1])
    XCTAssertEqual(extras?.edits?.first?.body, "old")
  }

  func testEmojiQueryAndCompletion() {
    XCTAssertEqual(activeEmojiQuery("great :thu"), "thu")
    XCTAssertEqual(activeEmojiQuery(":+1"), "+1")
    XCTAssertNil(activeEmojiQuery("nice :)"))
    XCTAssertNil(activeEmojiQuery("done :tada: "))
    XCTAssertNil(activeEmojiQuery(":tada:"))
    XCTAssertNil(activeEmojiQuery("time 10:30"))
    XCTAssertEqual(completeEmoji(in: "great :thu", with: "👍"), "great 👍 ")
    XCTAssertEqual(completeEmoji(in: ":thu", with: "👍"), "👍 ")
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

  func testOnlyDaysOutsideTheCurrentYearShowTheirYear() {
    let now = utcDate(2026, 10, 22)
    XCTAssertFalse(DayPill.showsYear(utcDate(2026, 1, 1), now: now, calendar: utc))
    XCTAssertTrue(DayPill.showsYear(utcDate(2025, 12, 31), now: now, calendar: utc))
    XCTAssertTrue(DayPill.showsYear(utcDate(2027, 1, 1), now: now, calendar: utc))
    // New Year's Eve: the same day gains its year once the next year has begun.
    let eve = utcDate(2026, 12, 31, hour: 23)
    XCTAssertFalse(DayPill.showsYear(eve, now: eve, calendar: utc))
    XCTAssertTrue(DayPill.showsYear(eve, now: utcDate(2027, 1, 1), calendar: utc))
  }

  private func utcDate(_ year: Int, _ month: Int, _ day: Int, hour: Int = 12) -> Date {
    utc.date(from: DateComponents(year: year, month: month, day: day, hour: hour))!
  }
}
