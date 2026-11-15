import XCTest

@testable import TeamCraft

final class MessageSelectionTests: XCTestCase {
  func testStartingPicksTheMessageAndTurnsTheModeOn() {
    var selection = MessageSelection()
    XCTAssertFalse(selection.isActive)
    selection.start(with: "10")
    XCTAssertTrue(selection.isActive)
    XCTAssertTrue(selection.isSelected("10"))
    XCTAssertEqual(selection.count, 1)
  }

  func testTogglingTwiceDropsTheMessageButKeepsTheMode() {
    var selection = MessageSelection()
    selection.start(with: "10")
    selection.toggle("11")
    selection.toggle("11")
    XCTAssertFalse(selection.isSelected("11"))
    selection.toggle("10")
    XCTAssertEqual(selection.count, 0)
    XCTAssertTrue(selection.isActive)
  }

  func testTogglingOutsideTheModeDoesNothing() {
    var selection = MessageSelection()
    selection.toggle("10")
    XCTAssertEqual(selection.count, 0)
    XCTAssertFalse(selection.isActive)
  }

  func testClearingLeavesTheMode() {
    var selection = MessageSelection()
    selection.start(with: "10")
    selection.clear()
    XCTAssertFalse(selection.isActive)
    XCTAssertEqual(selection.count, 0)
  }

  func testDeletedMessagesDropOut() {
    var selection = MessageSelection()
    selection.start(with: "1")
    selection.toggle("2")
    selection.forget { $0 == "2" }
    XCTAssertEqual(selection.ids, ["1"])
  }

  func testPickedMessagesComeOldestFirstWithoutSystemLinesOrPending() {
    var selection = MessageSelection()
    selection.start(with: "3")
    for id in ["1", "2", "4"] { selection.toggle(id) }
    let feed = [
      message("3", author: "a", sentAtMs: 300),
      message("1", author: "a", sentAtMs: 100),
      message("2", author: "b", sentAtMs: 200, event: .renamed(name: "x")),
      message("5", author: "a", sentAtMs: 500),
    ]
    XCTAssertEqual(selection.ordered(in: feed).map(\.id), ["1", "3"])
  }

  func testDeleteNeedsEveryMessageToBeTheUsersOwnAndPostingOn() {
    let own = message("1", author: "me", sentAtMs: 1)
    let other = message("2", author: "you", sentAtMs: 2)
    XCTAssertTrue(MessageSelection.canDeleteAll([own], me: "me", canPost: true))
    XCTAssertFalse(MessageSelection.canDeleteAll([own, other], me: "me", canPost: true))
    XCTAssertFalse(MessageSelection.canDeleteAll([own], me: "me", canPost: false))
    XCTAssertFalse(MessageSelection.canDeleteAll([], me: "me", canPost: true))
  }

  func testCopiedTextNamesSenderAndTimePerMessage() {
    let first = message("1", author: "a", sentAtMs: 100, name: "Ada")
    let second = message("2", author: "b", sentAtMs: 200, name: "Bo")
    let text = MessageSelection.copyText([first, second]) { "t\($0)" }
    XCTAssertEqual(text, "Ada, t100\nmessage 1\n\nBo, t200\nmessage 2")
  }

  private func message(
    _ id: String, author: String, sentAtMs: Int64, name: String = "", event: Message.SystemEvent? = nil
  ) -> Message {
    Message(
      id: id, conversationId: "c", authorId: author, sentAtMs: sentAtMs, body: "message \(id)", authorName: name,
      maybeSystemEvent: event)
  }
}
