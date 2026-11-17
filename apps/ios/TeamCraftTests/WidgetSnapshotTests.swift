import XCTest

@testable import TeamCraft

final class WidgetSnapshotTests: XCTestCase {
  func testTheAppLockHidesEverythingWhateverTheOtherSettings() {
    XCTAssertEqual(
      HomeWidget.privacy(showsNames: true, showsContent: true, isAppLockOn: true, isPreviewWanted: true), .locked)
    let locked = HomeWidget.snapshot(privacy: .locked, rows: rows(3))
    XCTAssertTrue(locked.rows.isEmpty)
    XCTAssertEqual(locked.unreadConversations, 0)
  }

  func testNotificationsThatShowNothingLeaveOnlyTheCount() {
    XCTAssertEqual(
      HomeWidget.privacy(showsNames: false, showsContent: false, isAppLockOn: false, isPreviewWanted: true), .countOnly)
    let snapshot = HomeWidget.snapshot(privacy: .countOnly, rows: rows(3))
    XCTAssertTrue(snapshot.rows.isEmpty)
    XCTAssertEqual(snapshot.unreadConversations, 3)
  }

  func testNamesAndCountsByDefaultWithoutPreviews() {
    XCTAssertEqual(
      HomeWidget.privacy(showsNames: true, showsContent: true, isAppLockOn: false, isPreviewWanted: false), .names)
    XCTAssertEqual(
      HomeWidget.privacy(showsNames: true, showsContent: false, isAppLockOn: false, isPreviewWanted: true), .names)
    let snapshot = HomeWidget.snapshot(privacy: .names, rows: rows(2))
    XCTAssertEqual(snapshot.rows.map(\.name), ["Conversation 0", "Conversation 1"])
    XCTAssertTrue(snapshot.rows.allSatisfy { $0.maybePreview == nil })
  }

  func testPreviewsOnlyWithContentNotificationsAndTheOptIn() {
    XCTAssertEqual(
      HomeWidget.privacy(showsNames: true, showsContent: true, isAppLockOn: false, isPreviewWanted: true), .previews)
    XCTAssertEqual(HomeWidget.snapshot(privacy: .previews, rows: rows(2)).rows[1].maybePreview, "Preview 1")
  }

  func testALongListKeepsItsCountButNotEveryRow() {
    let snapshot = HomeWidget.snapshot(privacy: .names, rows: rows(HomeWidget.maxRows + 5))
    XCTAssertEqual(snapshot.rows.count, HomeWidget.maxRows)
    XCTAssertEqual(snapshot.unreadConversations, HomeWidget.maxRows + 5)
  }

  func testWidgetURLsRoundTripAndRejectForeignIds() {
    XCTAssertEqual(HomeWidget.route(of: HomeWidget.conversationURL("123456789")), .conversation("123456789"))
    XCTAssertEqual(HomeWidget.route(of: HomeWidget.newMessageURL), .newMessage)
    XCTAssertEqual(HomeWidget.route(of: HomeWidget.appURL), .app)
    XCTAssertEqual(HomeWidget.route(of: URL(string: "teamcraft://widget/conversation/abc")!), .app)
    XCTAssertEqual(HomeWidget.route(of: URL(string: "teamcraft://widget/conversation/1/2")!), .app)
    // An app link is not the widget's.
    XCTAssertNil(HomeWidget.route(of: URL(string: "teamcraft://open/example.com/channel/1")!))
  }

  private func rows(_ count: Int) -> [WidgetRow] {
    (0..<count).map { index in
      WidgetRow(
        conversationId: "\(100 + index)", name: "Conversation \(index)", countLabel: "1",
        maybePreview: "Preview \(index)")
    }
  }
}
