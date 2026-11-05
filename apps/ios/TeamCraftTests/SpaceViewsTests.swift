import XCTest

@testable import TeamCraft

final class SpaceViewsTests: XCTestCase {
  private func item(_ id: String, space: (String, String)?, imageVersion: String? = nil) -> ConversationItem {
    ConversationItem(
      id: id, maybeSpaceId: space?.0, maybeSpaceName: space?.1, kind: .public, name: "general", topic: "",
      unreadCount: 0, mentionCount: 0, isFavorite: false, isMuted: false, canPost: true, isNotesToSelf: false,
      maybeSpaceImageVersion: imageVersion)
  }

  func testAChoiceCarriesThePictureVersion() {
    let choices = spaceChoices([item("1", space: ("1", "Project A"), imageVersion: "v2")])
    XCTAssertEqual(choices.map(\.maybeImageVersion), ["v2"])
  }

  func testOneChoicePerSpaceByNameAndNoneForDirectMessages() {
    let choices = spaceChoices([
      item("1", space: ("2", "Project B")),
      item("2", space: ("1", "Project A")),
      item("3", space: ("2", "Project B")),
      item("4", space: nil),
    ])
    XCTAssertEqual(choices, [SpaceChoice(id: "1", name: "Project A"), SpaceChoice(id: "2", name: "Project B")])
  }

  func testASpaceListsOnlyTheMeetingsScheduledInIt() throws {
    let json = """
      {"meetings":[
        {"id":"1","title":"Sync","starts_at":1,"ends_at":2,"status":"scheduled","conversation_id":"3","space_id":"9"},
        {"id":"2","title":"Old","starts_at":1,"ends_at":2,"status":"scheduled","conversation_id":"4"}
      ]}
      """
    let list = try XCTUnwrap(Wire.decode(MeetingList.self, from: json))
    XCTAssertEqual(meetingsOfSpace(list.meetings, spaceId: "9").map(\.id), ["1"])
    XCTAssertNil(list.meetings[1].spaceId)
  }

  func testRosterAnswersDecodeAndThePeopleAreByNameWithoutBots() throws {
    let json = """
      {"answer":"space_members","roster":{"state":"loaded","members":[
        {"user":"1","display_name":"bea","is_guest":true,"role":"member"},
        {"user":"2","display_name":"Ada","role":"member"},
        {"user":"3","display_name":"Bot","is_bot":true,"role":"member"}
      ]}}
      """
    let roster = try XCTUnwrap(Wire.decode(ChatAnswer.self, from: json)?.roster)
    XCTAssertEqual(roster.people.map(\.displayName), ["Ada", "bea"])
    let hidden = try XCTUnwrap(Wire.decode(ChatAnswer.self, from: #"{"answer":"space_members","roster":{"state":"hidden"}}"#))
    XCTAssertEqual(hidden.roster?.state, "hidden")
  }

  func testCallAndSavedAnswersDecode() throws {
    let calls = #"{"answer":"call_history","calls":[{"id":"7","conversation":"3","direction":"missed","started_at":5,"participants":[]}]}"#
    XCTAssertEqual(try XCTUnwrap(Wire.decode(ChatAnswer.self, from: calls)?.calls).first?.direction, "missed")
    let saved = #"{"answer":"saved_items","items":[{"message":{"id":"1","conversation":"3","body":"Hello"},"conversation_name":"general","saved_at":5}]}"#
    XCTAssertEqual(try XCTUnwrap(Wire.decode(ChatAnswer.self, from: saved)?.items).first?.conversationName, "general")
  }
}
