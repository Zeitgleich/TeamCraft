import UIKit
import XCTest

@testable import TeamCraft

final class QuickActionsTests: XCTestCase {
  func testARecentConversationShortcutCarriesItsIdAndNameOnly() throws {
    let item = RecentConversationShortcut(conversationId: "42", title: "Project A").shortcutItem
    XCTAssertEqual(item.localizedTitle, "Project A")
    XCTAssertNil(item.localizedSubtitle)
    XCTAssertEqual(item.userInfo?.count, 1)
    let back = try XCTUnwrap(RecentConversationShortcut(item: item))
    XCTAssertEqual(back, RecentConversationShortcut(conversationId: "42", title: "Project A"))
  }

  func testTheFixedActionsAreNotConversations() {
    for action in QuickAction.allCases {
      XCTAssertNil(RecentConversationShortcut(item: action.shortcutItem))
    }
    let conversation = RecentConversationShortcut(conversationId: "1", title: "A").shortcutItem
    XCTAssertNil(QuickAction(item: conversation))
  }

  func testAnItemWithoutAnIdIsNotAConversation() {
    let item = UIApplicationShortcutItem(type: RecentConversationShortcut.type, localizedTitle: "A")
    XCTAssertNil(RecentConversationShortcut(item: item))
  }
}
