import XCTest

@testable import TeamCraft

final class BrowseTests: XCTestCase {
  private let snapshotJSON = """
    {
      "me": {"id": "1", "username": "me", "display_name": "Me", "is_bot": false},
      "users": [
        {"id": "2", "username": "alice", "display_name": "Alice", "is_bot": false},
        {"id": "3", "username": "bob", "display_name": "bob", "is_bot": false},
        {"id": "4", "username": "hook", "display_name": "Hook", "is_bot": true}
      ],
      "spaces": [{"id": "9", "name": "Project A", "hue": 10}],
      "conversations": [
        {"id": "10", "kind": "Group", "name": "Trio", "member_ids": ["1", "2", "3"]},
        {"id": "11", "kind": "Direct", "name": "Alice", "member_ids": ["1", "2"]}
      ]
    }
    """

  func testPeopleLeaveOutMeAndBotsSortedByName() throws {
    let snapshot = try XCTUnwrap(Wire.decode(BrowseSnapshot.self, from: snapshotJSON))
    XCTAssertEqual(snapshot.people.map(\.username), ["alice", "bob"])
  }

  func testDirectConversationIsFoundButNotAGroup() throws {
    let snapshot = try XCTUnwrap(Wire.decode(BrowseSnapshot.self, from: snapshotJSON))
    XCTAssertEqual(snapshot.directConversation(with: "2"), "11")
    XCTAssertNil(snapshot.directConversation(with: "3"))
  }

  func testBrowseActionsOnTheWire() {
    XCTAssertEqual(ChatAction.openDm(userId: "2").json, #"{"action":"open_dm","user":"2"}"#)
    XCTAssertEqual(
      ChatAction.joinChannel(conversationId: "10").json, #"{"action":"join_channel","conversation":"10"}"#)
    XCTAssertEqual(ChatQuery.browseChannels(spaceId: "9").json, #"{"kind":"browse_channels","space":"9"}"#)
  }
}
