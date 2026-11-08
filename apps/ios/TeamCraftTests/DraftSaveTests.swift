import XCTest

@testable import TeamCraft

final class DraftSaveTests: XCTestCase {
  func testTextTypedJustBeforeLeavingIsStoredOnLeaving() {
    var save = DraftSave()
    save.loaded("")
    save.edit("hel", conversationId: "7", threadId: nil)
    XCTAssertEqual(save.takePending(), DraftSave.Pending(conversationId: "7", maybeThreadId: nil, text: "hel"))
  }

  func testTheDraftJustReadIsNotStoredAgain() {
    var save = DraftSave()
    save.loaded("stored")
    save.edit("stored", conversationId: "7", threadId: nil)
    XCTAssertNil(save.takePending())
  }

  func testNothingWaitsAfterTheTextWasStored() {
    var save = DraftSave()
    save.loaded("")
    save.edit("a", conversationId: "7", threadId: nil)
    XCTAssertNotNil(save.takePending())
    XCTAssertNil(save.takePending())
    XCTAssertNil(save.takeDue(for: "a"))
  }

  func testThePauseOfAnOlderEditStoresNothingWhenANewerOneWaits() {
    var save = DraftSave()
    save.loaded("")
    save.edit("a", conversationId: "7", threadId: nil)
    save.edit("ab", conversationId: "7", threadId: nil)
    XCTAssertNil(save.takeDue(for: "a"))
    XCTAssertEqual(save.takeDue(for: "ab")?.text, "ab")
  }

  func testTypingBackToTheReadTextStoresNothing() {
    var save = DraftSave()
    save.loaded("x")
    save.edit("xy", conversationId: "7", threadId: nil)
    save.edit("x", conversationId: "7", threadId: nil)
    // The text changed meanwhile, so the stored one is no longer known to match: storing is safe.
    XCTAssertEqual(save.takePending()?.text, "x")
  }

  func testTheSameTextAfterClearingIsStoredAgain() {
    var save = DraftSave()
    save.loaded("")
    save.edit("hi", conversationId: "7", threadId: nil)
    _ = save.takePending()
    save.edit("", conversationId: "7", threadId: nil)  // sent: the core cleared the draft
    save.edit("hi", conversationId: "7", threadId: nil)
    XCTAssertEqual(save.takePending()?.text, "hi")
  }

  func testAWaitingTextKeepsItsPlaceWhenTheConversationChanges() {
    var save = DraftSave()
    save.loaded("")
    save.edit("late", conversationId: "7", threadId: "9")
    let pending = save.takePending()
    save.loaded("other")
    XCTAssertEqual(pending, DraftSave.Pending(conversationId: "7", maybeThreadId: "9", text: "late"))
    XCTAssertNil(save.takePending())
  }
}
