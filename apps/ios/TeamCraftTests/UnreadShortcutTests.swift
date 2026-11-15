import XCTest

@testable import TeamCraft

final class UnreadShortcutTests: XCTestCase {
  private let order: [(String, Int)] = [("a", 0), ("b", 2), ("c", 0), ("d", 1)]

  func testFollowsTheCurrentConversationAndWraps() {
    XCTAssertEqual(UnreadShortcut.next(in: order, after: "a"), "b")
    XCTAssertEqual(UnreadShortcut.next(in: order, after: "b"), "d")
    XCTAssertEqual(UnreadShortcut.next(in: order, after: "d"), "b")
  }

  func testStartsAtTheTopWithoutAnOpenConversation() {
    XCTAssertEqual(UnreadShortcut.next(in: order, after: nil), "b")
  }

  func testIsNilWithoutOtherUnread() {
    XCTAssertNil(UnreadShortcut.next(in: [("a", 3)], after: "a"))
    XCTAssertNil(UnreadShortcut.next(in: [(String, Int)](), after: nil))
  }
}
