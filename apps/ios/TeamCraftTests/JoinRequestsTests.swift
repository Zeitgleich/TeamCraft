import XCTest

@testable import TeamCraft

/// Admins' requests to join a private channel on the phone (admin.md §2.4.1, issue #799).
final class JoinRequestsTests: XCTestCase {
  func testTheBrowserOffersWhatTheCoreDecided() {
    XCTAssertEqual(BrowseJoin(wire: "open"), .open)
    XCTAssertEqual(BrowseJoin(wire: "as_admin"), .asAdmin)
    XCTAssertEqual(BrowseJoin(wire: "needs_approval"), .needsApproval)
    XCTAssertEqual(BrowseJoin(wire: "requested"), .requested)
    XCTAssertEqual(BrowseJoin(wire: nil), .open, "an older core lists public channels only")
    XCTAssertNil(BrowseJoin(wire: "by_invitation"), "nothing to press for a way in this version doesn't know")
  }

  func testBrowsableChannelsCarryTheirWayIn() throws {
    let asked = try XCTUnwrap(
      Wire.decode(
        BrowsableChannel.self,
        from: #"{"conversation":"8","name":"board","is_member":false,"join":"requested"}"#))
    XCTAssertEqual(asked.maybeJoin, .requested)
    let older = try XCTUnwrap(
      Wire.decode(BrowsableChannel.self, from: #"{"conversation":"7","name":"general","is_member":false}"#))
    XCTAssertEqual(older.maybeJoin, .open)
  }

  func testOnlyAnActionThatChangedNothingIsWorded() {
    for step in [JoinStep.ask, .withdraw, .approve, .decline] {
      XCTAssertNil(JoinRequests.note(after: step, outcome: .done))
    }
    XCTAssertEqual(JoinRequests.note(after: .approve, outcome: .gone), .gone)
    XCTAssertEqual(JoinRequests.note(after: .decline, outcome: .gone), .gone)
    XCTAssertEqual(JoinRequests.note(after: .approve, outcome: .notAnAdmin), .requesterNotAdmin)
    XCTAssertEqual(JoinRequests.note(after: .ask, outcome: .notAnAdmin), .notAnAdminYourself)
    XCTAssertEqual(JoinRequests.note(after: .ask, outcome: .tooSoon), .tooSoon)
    XCTAssertEqual(JoinRequests.note(after: .approve, outcome: .notApprovable), .notApprovable)
  }

  func testAChannelShowsOnlyItsOwnRequestsEachKeyedByAdminAndChannel() {
    let requests = [request("7", "2"), request("8", "2"), request("7", "3")]
    XCTAssertEqual(JoinRequests.requests(in: "7", of: requests), [request("7", "2"), request("7", "3")])
    XCTAssertEqual(JoinRequests.requests(in: "9", of: requests), [])
    XCTAssertEqual(JoinRequests.key(request("7", "2")), "7:2")
  }

  func testTheNotificationOffersApproveOnlyForAnAdminOfTheFreshRoster() {
    let approvable = JoinRequestNotice(request("7", "2"), showsNames: true)
    XCTAssertEqual(approvable.identifier, "admin-join:7:2")
    XCTAssertEqual(approvable.categoryIdentifier, NotificationCategories.joinRequestCategory)
    XCTAssertTrue(approvable.title.contains("Admin 2 (@admin.2)"), "the username tells two Admins apart")
    XCTAssertTrue(approvable.title.contains("#planning"))
    XCTAssertFalse(approvable.isQuiet)
    let unknown = JoinRequestNotice(request("7", "3", standing: .notAnAdmin), showsNames: true)
    XCTAssertEqual(unknown.categoryIdentifier, NotificationCategories.joinRequestDeclineOnlyCategory)
  }

  func testWithoutNamesTheDecisionIsMadeInTheApp() {
    let hidden = JoinRequestNotice(request("7", "2"), showsNames: false)
    XCTAssertEqual(hidden.categoryIdentifier, "")
    XCTAssertFalse(hidden.title.contains("Admin 2"))
    XCTAssertFalse(hidden.body.contains("planning"))
  }

  func testAFailedDecisionFromTheNotificationIsShownAgainQuietly() {
    let again = JoinRequestNotice(request("7", "2"), showsNames: true, maybeNote: .failed(.network))
    XCTAssertTrue(again.isQuiet)
    XCTAssertEqual(again.body, JoinNote.failed(.network).notificationText)
    XCTAssertEqual(again.categoryIdentifier, NotificationCategories.joinRequestCategory)
  }

  private func request(_ conversation: String, _ user: String, standing: JoinRequestStanding = .approvable)
    -> JoinRequestItem
  {
    JoinRequestItem(
      conversationId: conversation, conversationName: "planning", userId: user, userName: "Admin \(user)",
      maybeUsername: "admin.\(user)", userLabel: "Admin \(user) (@admin.\(user))",
      requestedAtMs: 10, expiresAtMs: 20, standing: standing,
      notificationTag: "admin-join:\(conversation):\(user)")
  }
}
