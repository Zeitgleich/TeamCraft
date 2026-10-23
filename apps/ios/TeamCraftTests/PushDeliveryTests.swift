import UserNotifications
import XCTest

@testable import TeamCraft

final class PushPresentationTests: XCTestCase {
  func testNewMessagesAlert() {
    XCTAssertEqual(PushPresentation.of(PushFlags(kind: .message), canFilter: false), .message)
    XCTAssertEqual(PushPresentation.of(PushFlags(kind: .message), canFilter: true), .message)
  }

  func testSuppressedMessagesAreQuietWithoutFilteringAndDroppedWithIt() {
    for suppression in [NotificationSuppression.muted, .doNotDisturb] {
      let flags = PushFlags(kind: .message, maybeSuppression: suppression)
      XCTAssertEqual(PushPresentation.of(flags, canFilter: false), .quietMessage)
      XCTAssertEqual(PushPresentation.of(flags, canFilter: true), .drop)
    }
  }

  func testDuplicatesAreQuietWithoutFilteringAndDroppedWithIt() {
    let flags = PushFlags(kind: .message, isDuplicate: true)
    XCTAssertEqual(PushPresentation.of(flags, canFilter: false), .quietMessage)
    XCTAssertEqual(PushPresentation.of(flags, canFilter: true), .drop)
  }

  func testUrgentRepeatsAlertAgainAndReplaceTheEarlierOne() {
    // The core never marks an urgent repeat as a duplicate.
    let presentation = PushPresentation.of(PushFlags(kind: .message, isUrgent: true), canFilter: false)
    XCTAssertEqual(presentation, .message)
    XCTAssertTrue(presentation.replacesSameMessage)
  }

  func testCallsGoToCallKitOnlyWithFiltering() {
    XCTAssertEqual(PushPresentation.of(PushFlags(kind: .call), canFilter: true), .callKit)
    XCTAssertEqual(PushPresentation.of(PushFlags(kind: .call), canFilter: false), .callAlert)
    XCTAssertEqual(PushPresentation.of(PushFlags(kind: .call, isDuplicate: true), canFilter: false), .quietGeneric)
    XCTAssertEqual(PushPresentation.of(PushFlags(kind: .call, maybeSuppression: .doNotDisturb), canFilter: true), .drop)
  }

  func testDismissAndUnknownShowNothing() {
    for kind in [PushNotificationKind.dismiss, .unknown] {
      XCTAssertEqual(PushPresentation.of(PushFlags(kind: kind), canFilter: true), .drop)
      XCTAssertEqual(PushPresentation.of(PushFlags(kind: kind), canFilter: false), .quietGeneric)
    }
  }

  func testTestPushesAlwaysShow() {
    XCTAssertEqual(PushPresentation.of(PushFlags(kind: .test), canFilter: true), .test)
    XCTAssertFalse(PushPresentation.test.replacesSameMessage)
  }
}

final class NotificationStyleTests: XCTestCase {
  func testAMessageAlertsWithSoundAndTheBadge() {
    let content = styled(.message, PushFlags(kind: .message, maybeBadge: 3))
    XCTAssertNotNil(content.sound)
    XCTAssertEqual(content.interruptionLevel, .active)
    XCTAssertEqual(content.badge?.intValue, 3)
    XCTAssertEqual(content.relevanceScore, 0.3)
  }

  func testWithoutABadgeTheBadgeIsLeftAlone() {
    XCTAssertNil(styled(.message, PushFlags(kind: .message)).badge)
  }

  func testUrgentIsTimeSensitiveAndRanksFirst() {
    let content = styled(.message, PushFlags(kind: .message, isUrgent: true))
    XCTAssertEqual(content.interruptionLevel, .timeSensitive)
    XCTAssertEqual(content.relevanceScore, 1)
  }

  func testMentionsRankAboveOtherMessages() {
    XCTAssertEqual(styled(.message, PushFlags(kind: .message, isMention: true)).relevanceScore, 0.8)
  }

  func testSilentMessagesHaveNoSound() {
    XCTAssertNil(styled(.message, PushFlags(kind: .message, isSilent: true)).sound)
  }

  func testQuietMessagesArePassiveAndSilentButKeepTheBadge() {
    let content = styled(.quietMessage, PushFlags(kind: .message, maybeSuppression: .muted, maybeBadge: 2))
    XCTAssertNil(content.sound)
    XCTAssertEqual(content.interruptionLevel, .passive)
    XCTAssertEqual(content.relevanceScore, 0)
    XCTAssertEqual(content.badge?.intValue, 2)
  }

  func testCallAlertsRingAndAreTimeSensitive() {
    let content = styled(.callAlert, PushFlags(kind: .call))
    XCTAssertNotNil(content.sound)
    XCTAssertEqual(content.interruptionLevel, .timeSensitive)
  }

  func testTheGenericFallbackDropsEverythingButTheSealedWakeUpWhenAsked() {
    let original = UNMutableNotificationContent()
    original.userInfo = ["w": "AAEC", NotificationCategories.conversationKey: "7"]
    original.threadIdentifier = "7"
    let builder = NotificationContentBuilder(strings: strings)
    let kept = builder.generic(from: original, keepsSealed: true)
    XCTAssertEqual(kept.userInfo.count, 1)
    XCTAssertEqual(kept.userInfo["w"] as? String, "AAEC")
    XCTAssertEqual(kept.threadIdentifier, "")
    XCTAssertEqual(kept.title, "TeamCraft")
    XCTAssertEqual(kept.body, "New message")
    XCTAssertTrue(builder.generic(from: original).userInfo.isEmpty)
  }

  private var strings: NotificationStrings {
    NotificationStrings(
      appName: "TeamCraft", newMessage: "New message", fileText: { "File \($0)" }, callTitle: "Incoming call",
      callFrom: { "Call from \($0)" }, callUnknown: "Someone is calling", testBody: "Test", quietUpdate: "Updated")
  }

  private func styled(_ presentation: PushPresentation, _ flags: PushFlags) -> UNMutableNotificationContent {
    let content = UNMutableNotificationContent()
    NotificationContentBuilder.apply(presentation, flags: flags, to: content)
    return content
  }
}

final class NotificationMatcherTests: XCTestCase {
  func testReadingAConversationRemovesItsTopLevelNotificationsUpToTheReadMessage() {
    let delivered = [
      message("a", conversation: "1", id: "100"),
      message("b", conversation: "1", id: "200"),
      message("c", conversation: "1", id: "300"),
      message("d", conversation: "2", id: "150"),
      message("e", conversation: "1", id: "120", thread: "90"),
    ]
    let target = DismissTarget(conversationId: "1", maybeMessageId: "200")
    XCTAssertEqual(NotificationMatcher.identifiers(delivered, dismissing: target), ["a", "b"])
  }

  func testReadingAThreadRemovesOnlyThatThread() {
    let delivered = [
      message("a", conversation: "1", id: "100"),
      message("b", conversation: "1", id: "120", thread: "90"),
      message("c", conversation: "1", id: "130", thread: "95"),
    ]
    let target = DismissTarget(conversationId: "1", maybeThreadId: "90", maybeMessageId: "500")
    XCTAssertEqual(NotificationMatcher.identifiers(delivered, dismissing: target), ["b"])
  }

  func testMessageIdsCompareAsNumbersNotText() {
    XCTAssertTrue(NotificationMatcher.isAtOrBefore("99", "100"))
    XCTAssertFalse(NotificationMatcher.isAtOrBefore("1000", "999"))
    XCTAssertTrue(NotificationMatcher.isAtOrBefore("7206104748613181440", "7206104748613181441"))
  }

  func testMissingIdsCountAsRead() {
    XCTAssertTrue(NotificationMatcher.isAtOrBefore(nil, "5"))
    XCTAssertTrue(NotificationMatcher.isAtOrBefore("5", nil))
  }

  func testACallDismissRemovesOnlyThatCallsAlert() {
    let delivered = [
      call("a", conversation: "1", callId: "77"),
      call("b", conversation: "1", callId: "78"),
      message("c", conversation: "1", id: "100"),
    ]
    let target = DismissTarget(conversationId: "1", maybeCallId: "77")
    XCTAssertEqual(NotificationMatcher.identifiers(delivered, dismissing: target), ["a"])
  }

  func testAMessageDismissLeavesCallAlertsAndTestPushes() {
    let delivered = [
      call("a", conversation: "1", callId: "77"),
      DeliveredPush(identifier: "t", userInfo: [NotificationKeys.kind: NotificationKeys.testKind]),
    ]
    XCTAssertTrue(NotificationMatcher.identifiers(delivered, dismissing: DismissTarget(conversationId: "1")).isEmpty)
  }

  func testOlderNotificationsWithoutAKindCountAsMessages() {
    let old = DeliveredPush(identifier: "x", userInfo: [NotificationKeys.conversation: "1"])
    XCTAssertEqual(NotificationMatcher.identifiers([old], dismissing: DismissTarget(conversationId: "1")), ["x"])
  }

  func testARepeatReplacesTheSameMessageOnly() {
    let delivered = [message("a", conversation: "1", id: "100"), message("b", conversation: "1", id: "101")]
    XCTAssertEqual(NotificationMatcher.identifiers(delivered, sameMessageAs: "100", in: "1"), ["a"])
    XCTAssertTrue(NotificationMatcher.identifiers(delivered, sameMessageAs: "100", in: "2").isEmpty)
  }

  func testFullyReadConversationsLoseTheirTopLevelNotifications() {
    let delivered = [
      message("a", conversation: "1", id: "100"),
      message("b", conversation: "1", id: "101", thread: "90"),
      message("c", conversation: "2", id: "102"),
    ]
    XCTAssertEqual(NotificationMatcher.identifiers(delivered, readConversations: ["1"]), ["a"])
  }

  func testUserInfoRoundTrips() {
    let info = NotificationKeys.userInfo(
      kind: NotificationKeys.messageKind, conversationId: "1", maybeTag: "ff", maybeMessageId: "5",
      maybeThreadId: "4")
    let push = DeliveredPush(identifier: "id", userInfo: info)
    XCTAssertEqual(push.maybeConversationId, "1")
    XCTAssertEqual(push.maybeMessageId, "5")
    XCTAssertEqual(push.maybeThreadId, "4")
    XCTAssertNil(push.maybeCallId)
    XCTAssertTrue(push.isMessage)
    XCTAssertEqual(info[NotificationAuthenticator.tagKey] as? String, "ff")
  }

  private func message(_ identifier: String, conversation: String, id: String, thread: String? = nil) -> DeliveredPush {
    DeliveredPush(
      identifier: identifier,
      userInfo: NotificationKeys.userInfo(
        kind: NotificationKeys.messageKind, conversationId: conversation, maybeTag: nil, maybeMessageId: id,
        maybeThreadId: thread))
  }

  private func call(_ identifier: String, conversation: String, callId: String) -> DeliveredPush {
    DeliveredPush(
      identifier: identifier,
      userInfo: NotificationKeys.userInfo(
        kind: NotificationKeys.callKind, conversationId: conversation, maybeTag: nil, maybeCallId: callId))
  }
}

final class BadgeAndClockTests: XCTestCase {
  func testTheBadgeCountsConversationsWithMentionsOrUnreadDirectMessages() {
    let conversations = [
      UnreadSummary(isDirectMessage: true, unreadCount: 2, mentionCount: 0),
      UnreadSummary(isDirectMessage: false, unreadCount: 9, mentionCount: 0),
      UnreadSummary(isDirectMessage: false, unreadCount: 3, mentionCount: 1),
      UnreadSummary(isDirectMessage: true, unreadCount: 0, mentionCount: 0),
    ]
    XCTAssertEqual(BadgeCount.local(conversations), 2)
  }

  func testTheOffsetFollowsDaylightSavingTime() throws {
    let berlin = try XCTUnwrap(TimeZone(identifier: "Europe/Berlin"))
    // 2026-01-15 and 2026-07-15, noon UTC.
    XCTAssertEqual(PushClock.utcOffsetMinutes(timeZone: berlin, at: Date(timeIntervalSince1970: 1_768_478_400)), 60)
    XCTAssertEqual(PushClock.utcOffsetMinutes(timeZone: berlin, at: Date(timeIntervalSince1970: 1_784_116_800)), 120)
  }
}
