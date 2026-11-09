import XCTest

@testable import TeamCraft

final class GroupMentionsTests: XCTestCase {
  private let groups = [
    MentionableGroupItem(handle: "accounting", name: "Accounting Team", kind: .custom, notifiedCount: 3, outsideCount: 0),
    MentionableGroupItem(handle: "admins", name: "Admins", kind: .admins, notifiedCount: 2, outsideCount: 1),
  ]

  func testTheTypedMentionIsFoundAtTheEndOnly() {
    XCTAssertEqual(activeMentionQuery("hello @acc"), "acc")
    XCTAssertNil(activeMentionQuery("hello acc"))
    XCTAssertNil(activeMentionQuery("mail@example.com"))
    XCTAssertEqual(activeMentionQuery("@"), "")
  }

  func testGroupsAreSuggestedByHandleOrShownName() {
    XCTAssertEqual(groupSuggestions(query: "acc", groups: groups, adminsName: "Administratoren").map(\.handle), ["accounting"])
    XCTAssertEqual(groupSuggestions(query: "team", groups: groups, adminsName: "Administratoren").map(\.handle), ["accounting"])
    XCTAssertEqual(groupSuggestions(query: "administ", groups: groups, adminsName: "Administratoren").map(\.handle), ["admins"])
    XCTAssertEqual(shownGroupName(groups[1], adminsName: "Administratoren"), "Administratoren")
  }

  func testPickingAGroupCompletesItsHandle() {
    XCTAssertEqual(completeMention(in: "ping @acc", handle: "accounting"), "ping @accounting ")
    XCTAssertEqual(completeMention(in: "@ad", handle: "admins"), "@admins ")
  }
}
