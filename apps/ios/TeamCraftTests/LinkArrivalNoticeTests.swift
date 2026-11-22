import XCTest

@testable import TeamCraft

/// The notice of somebody this device let into a meeting without a click (#2258).
final class LinkArrivalNoticeTests: XCTestCase {
  func testTheNoticeNamesThePersonAndOffersRemoveWhereNamesShow() {
    let notice = LinkArrivalNotice(arrival(), showsNames: true)
    XCTAssertEqual(notice.categoryIdentifier, NotificationCategories.linkArrivalCategory)
    XCTAssertTrue(notice.title.contains("Gus"))
    XCTAssertFalse(notice.isQuiet)
  }

  func testBehindTheAppLockItSaysOnlyThatSomebodyJoined() {
    let notice = LinkArrivalNotice(arrival(), showsNames: false)
    XCTAssertEqual(notice.categoryIdentifier, "", "no Remove from the lock screen")
    XCTAssertFalse(notice.title.contains("Gus"))
    XCTAssertFalse(notice.body.contains("Planning"))
  }

  func testAFailedRemovalShowsAgainQuietly() {
    let notice = LinkArrivalNotice(arrival(), showsNames: true, isFailed: true)
    XCTAssertTrue(notice.isQuiet)
    XCTAssertEqual(notice.identifier, LinkArrivalNotice(arrival(), showsNames: true).identifier, "replaces the first")
  }

  func testRemoveFindsThePersonTheNoticeNamed() throws {
    let request = LinkArrivalNotice(arrival(), showsNames: true).request()
    XCTAssertEqual(LinkArrivalNotice.arrival(from: request.content.userInfo), arrival())
    XCTAssertNil(LinkArrivalNotice.arrival(from: [:]), "nothing to remove without the notice's data")
  }

  private func arrival() -> LinkArrival {
    LinkArrival(meetingId: "7", meetingTitle: "Planning", conversationId: "70", userId: "77", displayName: "Gus")
  }
}
