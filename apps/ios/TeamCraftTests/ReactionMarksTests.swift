import XCTest

@testable import TeamCraft

final class ReactionMarksTests: XCTestCase {
  func testOnlyWaitingMessagesOnScreenAreSeenAndOnce() {
    var marks = ReactionMarks()
    let unseen = UnseenReactions(count: 3, messages: ["1", "2"])
    XCTAssertEqual(marks.seen(onScreen: ["2", "3"], unseen: unseen), ["2"])
    XCTAssertEqual(marks.seen(onScreen: ["2", "3"], unseen: unseen), [])
  }

  func testAMessageWaitingAgainIsSeenAgain() {
    var marks = ReactionMarks()
    XCTAssertEqual(marks.seen(onScreen: ["1"], unseen: UnseenReactions(count: 1, messages: ["1"])), ["1"])
    XCTAssertEqual(marks.seen(onScreen: ["1"], unseen: UnseenReactions()), [])
    XCTAssertEqual(marks.seen(onScreen: ["1"], unseen: UnseenReactions(count: 1, messages: ["1"])), ["1"])
  }

  func testAJumpMarksTheTargetOnce() {
    var marks = ReactionMarks()
    XCTAssertTrue(marks.jumped(to: "1"))
    XCTAssertFalse(marks.jumped(to: "1"))
    XCTAssertEqual(marks.seen(onScreen: ["1"], unseen: UnseenReactions(count: 1, messages: ["1"])), [])
  }
}
