import XCTest

@testable import TeamCraft

/// The screen goes off at the ear only in a call on the receiver, never while a call only rings.
final class CallProximityTests: XCTestCase {
  func testACallOnTheReceiverTurnsTheScreenOffAtTheEar() {
    XCTAssertTrue(CallProximity.isScreenOffAtEar(CallProximityState(isRingingIn: false), isOnReceiver: true))
  }

  func testTheLoudspeakerOrAHeadsetKeepsTheScreenOn() {
    XCTAssertFalse(CallProximity.isScreenOffAtEar(CallProximityState(isRingingIn: false), isOnReceiver: false))
  }

  func testARingingIncomingCallKeepsTheScreenOn() {
    XCTAssertFalse(CallProximity.isScreenOffAtEar(CallProximityState(isRingingIn: true), isOnReceiver: true))
  }

  func testNoCallKeepsTheScreenOn() {
    XCTAssertFalse(CallProximity.isScreenOffAtEar(nil, isOnReceiver: true))
  }
}
