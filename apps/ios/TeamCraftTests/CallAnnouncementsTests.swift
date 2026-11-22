import XCTest

@testable import TeamCraft

final class CallAnnouncementsTests: XCTestCase {
  func testAChangeNamesWhoCameAndWhoWent() {
    let change = RosterChange(before: ["a", "b"], now: ["b", "c"])
    XCTAssertEqual(change.joined, ["c"])
    XCTAssertEqual(change.left, ["a"])
  }

  func testNoChangeIsEmpty() {
    XCTAssertTrue(RosterChange(before: ["a"], now: ["a"]).isEmpty)
  }

  func testAFlickerInsideOneWindowSaysNothing() {
    let gone = RosterChange(before: ["a", "b"], now: ["b"])
    let back = RosterChange(before: ["b"], now: ["a", "b"])
    XCTAssertTrue(gone.merged(with: back).isEmpty)
  }

  func testABurstAddsUp() {
    let first = RosterChange(before: [], now: ["a"])
    let second = RosterChange(before: ["a"], now: ["a", "b", "c"])
    XCTAssertEqual(first.merged(with: second).joined, ["a", "b", "c"])
  }
}
