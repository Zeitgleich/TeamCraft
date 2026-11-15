import XCTest

@testable import TeamCraft

/// The call's clock lives with the call, not with its screen, so it goes on while the screen is put
/// away and brought back (#2066).
final class CallMinimiseTests: XCTestCase {
  func testTheClockStartsAtTheFirstConnection() {
    var call = ActiveCall(id: UUID(), conversationId: "1", displayName: "Ada", isOutgoing: true, phase: .connecting)
    XCTAssertNil(call.maybeConnectedAt)
    let first = Date(timeIntervalSince1970: 1_000)
    call.markConnected(at: first)
    XCTAssertEqual(call.phase, .connected)
    XCTAssertEqual(call.maybeConnectedAt, first)
  }

  func testAReconnectKeepsTheClock() {
    var call = ActiveCall(id: UUID(), conversationId: "1", displayName: "Ada", isOutgoing: false, phase: .connecting)
    let first = Date(timeIntervalSince1970: 1_000)
    call.markConnected(at: first)
    call.phase = .reconnecting
    call.markConnected(at: Date(timeIntervalSince1970: 2_000))
    XCTAssertEqual(call.maybeConnectedAt, first, "the time runs through reconnects")
  }

  func testTheTimeReadsAsOnTheCallScreen() {
    XCTAssertEqual(CallScreen.format(65), "1:05")
    XCTAssertEqual(CallScreen.format(3_725), "1:02:05")
    XCTAssertEqual(CallScreen.format(-3), "0:00")
  }
}
