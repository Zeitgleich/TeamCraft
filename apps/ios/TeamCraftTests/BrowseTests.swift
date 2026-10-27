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
    XCTAssertEqual(snapshot.directConversation(with: ["2"]), "11")
    XCTAssertNil(snapshot.directConversation(with: ["3"]))
  }

  func testGroupIsFoundForExactlyItsPeople() throws {
    let snapshot = try XCTUnwrap(Wire.decode(BrowseSnapshot.self, from: snapshotJSON))
    XCTAssertEqual(snapshot.directConversation(with: ["3", "2", "1"]), "10")
    XCTAssertNil(snapshot.directConversation(with: ["2", "3", "4"]))
  }

  func testAddablePeopleLeaveOutTheConversationsMembers() throws {
    let snapshot = try XCTUnwrap(Wire.decode(BrowseSnapshot.self, from: snapshotJSON))
    XCTAssertEqual(snapshot.addablePeople(to: "11", matching: "").map(\.id), ["3"])
    XCTAssertEqual(snapshot.addablePeople(to: "10", matching: "").map(\.id), [])
    XCTAssertEqual(snapshot.addablePeople(to: "11", matching: "BO").map(\.id), ["3"])
    XCTAssertEqual(snapshot.addablePeople(to: "11", matching: "alice").map(\.id), [])
  }

  func testCallAdditionKeepsThePeopleInTheCallOutOfTheAdded() throws {
    let snapshot = try XCTUnwrap(Wire.decode(BrowseSnapshot.self, from: snapshotJSON))
    let addition = snapshot.callAddition(inCallUsers: ["2"], picked: ["3", "2", "3"])
    XCTAssertEqual(addition, CallAddition(inCall: ["1", "2"], added: ["3"]))
  }

  func testChannelCreationAndBrowsingFollowTheCore() throws {
    let snapshot = try XCTUnwrap(Wire.decode(BrowseSnapshot.self, from: snapshotJSON))
    XCTAssertTrue(snapshot.canBrowseChannels)
    XCTAssertEqual(snapshot.spaces?.first?.offersChannelCreation, true, "an older core doesn't say")
    let guest = """
      {
        "me": {"id": "1", "username": "ext", "display_name": "Ext", "is_guest": true},
        "spaces": [
          {"id": "9", "name": "Project A", "can_create_channels": false},
          {"id": "8", "name": "Project B", "can_create_channels": true}
        ]
      }
      """
    let decoded = try XCTUnwrap(Wire.decode(BrowseSnapshot.self, from: guest))
    XCTAssertFalse(decoded.canBrowseChannels)
    XCTAssertEqual(decoded.spaces?.map(\.offersChannelCreation), [false, true])
  }

  func testBrowseActionsOnTheWire() {
    XCTAssertEqual(ChatAction.openDm(userId: "2").json, #"{"action":"open_dm","user":"2"}"#)
    XCTAssertEqual(
      ChatAction.openGroupDm(userIds: ["2", "3"]).json, #"{"action":"open_group_dm","users":["2","3"]}"#)
    XCTAssertEqual(
      ChatAction.joinChannel(conversationId: "10").json, #"{"action":"join_channel","conversation":"10"}"#)
    XCTAssertEqual(ChatQuery.browseChannels(spaceId: "9").json, #"{"kind":"browse_channels","space":"9"}"#)
  }
}
