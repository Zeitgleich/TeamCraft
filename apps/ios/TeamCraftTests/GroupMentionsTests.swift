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

  func testSuggestionsWorkAtTheCaretAndReplaceOnlyThatWord() {
    let text = "hi @an and more"
    XCTAssertEqual(activeMentionQuery(text, caret: 6), "an")
    XCTAssertNil(activeMentionQuery(text, caret: 10))
    let done = completingMention(in: text, handle: "anna", caret: 6)
    XCTAssertEqual(done.text, "hi @anna and more")
    XCTAssertEqual(done.caret, "hi @anna ".utf16.count)
  }

  func testRankedPeopleComeFirstAndOthersKeepTheirOrder() {
    let people = [person("bob", "Bob"), person("bella", "Bella"), person("ben", "Ben")]
    XCTAssertEqual(personSuggestions(query: "b", people: people, priority: ["Ben", "bella"]).map(\.username), ["ben", "bella", "bob"])
    XCTAssertEqual(personSuggestions(query: "b", people: people).map(\.username), ["bob", "bella", "ben"])
  }

  func testPriorityIsRecentSendersThenRecentlyMentionedWithoutMe() {
    let people = [person("me", "Me"), person("anna", "Anna"), person("tom", "Tom"), person("eva", "Eva")]
    let messages = [
      Message(id: "1", conversationId: "c", authorId: "tom", sentAtMs: 0, body: "a"),
      Message(id: "2", conversationId: "c", authorId: "anna", sentAtMs: 1, body: "b"),
      Message(id: "3", conversationId: "c", authorId: "me", sentAtMs: 2, body: "c"),
    ]
    XCTAssertEqual(
      mentionPriority(messages: messages, myId: "me", people: people, recentlyMentioned: ["eva", "Anna"]),
      ["anna", "tom", "eva"])
  }
}
