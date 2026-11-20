import XCTest

@testable import TeamCraft

/// #2179: kills by the system reach the crash reports with their code, never the reason's text.
final class SystemKillTests: XCTestCase {
  func testALockHeldWhileSuspendedIsNamed() throws {
    let kill = try XCTUnwrap(
      SystemKill(signal: 9, terminationReason: "Namespace RUNNINGBOARD, Code 0xDEAD10CC, held a lock on /private/var/x"))
    XCTAssertEqual(kill.maybeNamespace, "RUNNINGBOARD")
    XCTAssertEqual(kill.maybeCode, "0xdead10cc")
    XCTAssertEqual(kill.summary, "RUNNINGBOARD 0xdead10cc: held a file lock in the shared container while suspended")
  }

  func testOnlyKillsAreForwarded() {
    // sentry-cocoa reports the crashes it can catch itself.
    XCTAssertNil(SystemKill(signal: 11, terminationReason: nil))
    XCTAssertNil(SystemKill(signal: nil, terminationReason: "Namespace RUNNINGBOARD, Code 0xdead10cc"))
  }

  func testAReasonWithoutCodeStillReports() throws {
    let kill = try XCTUnwrap(SystemKill(signal: 9, terminationReason: nil))
    XCTAssertEqual(kill.summary, "unknown no code: killed by the system")
  }
}
