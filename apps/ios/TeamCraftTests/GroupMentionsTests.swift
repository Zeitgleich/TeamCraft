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

  private func person(_ username: String, _ name: String, isBot: Bool? = nil, isDeleted: Bool? = nil) -> PeopleSnapshot.Person {
    PeopleSnapshot.Person(id: username, username: username, displayName: name, isDeleted: isDeleted, isBot: isBot)
  }

  func testPeopleAreSuggestedByUsernameOrNameWordAndNeverBotsOrDeleted() {
    let people = [
      person("anna", "Anna Meier"), person("tom", "Tom Anders"), person("hook", "Hook", isBot: true),
      person("gone", "Anna Gone", isDeleted: true),
    ]
    XCTAssertEqual(personSuggestions(query: "an", people: people).map(\.username), ["anna", "tom"])
    XCTAssertEqual(personSuggestions(query: "MEI", people: people).map(\.username), ["anna"])
    XCTAssertEqual(personSuggestions(query: "", people: people, limit: 1).map(\.username), ["anna"])
    XCTAssertTrue(personSuggestions(query: "hook", people: people).isEmpty)
  }

  func testPickingAPersonCompletesTheirUsername() {
    XCTAssertEqual(completeMention(in: "hi\n@an", handle: "anna"), "hi\n@anna ")
  }

  func testConfirmationsFollowTheSetting() {
    XCTAssertEqual(MentionConfirmation.needed(body: "@channel hi", memberCount: 30, notified: 0, confirmOver: 10), .channel(memberCount: 30))
    XCTAssertNil(MentionConfirmation.needed(body: "@channel hi", memberCount: 5, notified: 0, confirmOver: 10))
    XCTAssertNil(MentionConfirmation.needed(body: "@channel hi", memberCount: 30, notified: 0, confirmOver: 0))
    XCTAssertEqual(MentionConfirmation.needed(body: "@team", memberCount: 5, notified: 12, confirmOver: 10), .groups(notified: 12))
    XCTAssertNil(MentionConfirmation.needed(body: "@team", memberCount: 5, notified: 10, confirmOver: 10))
    XCTAssertNil(MentionConfirmation.needed(body: "@team", memberCount: 5, notified: 99, confirmOver: 0))
  }
}
