import XCTest

@testable import TeamCraft

/// The member's own alert of a noted provider account that isn't theirs (issue #1880).
final class OwnRosterSnapshotTests: XCTestCase {
  func testTheMismatchDecodesFromTheRosterStatus() throws {
    let json = #"""
      {"security":{"owners":{"version":3,"owners":[{"user":"1"}],"recommended_minimum":2,"warning_lead_ms":1,
        "is_own_account_mismatched":true,"signing_problem":{"issue":"contradictory","users":["2"]}}}}
      """#
    XCTAssertTrue(try XCTUnwrap(Wire.decode(OwnRosterSnapshot.self, from: json)).isOwnAccountMismatched)
  }

  func testNoRosterOrNoFlagMeansNoAlert() throws {
    let fine = #"{"security":{"owners":{"version":3,"owners":[],"recommended_minimum":2,"warning_lead_ms":1}}}"#
    XCTAssertFalse(try XCTUnwrap(Wire.decode(OwnRosterSnapshot.self, from: fine)).isOwnAccountMismatched)
    XCTAssertFalse(try XCTUnwrap(Wire.decode(OwnRosterSnapshot.self, from: #"{"security":{}}"#)).isOwnAccountMismatched)
    XCTAssertFalse(try XCTUnwrap(Wire.decode(OwnRosterSnapshot.self, from: "{}")).isOwnAccountMismatched)
  }
}
