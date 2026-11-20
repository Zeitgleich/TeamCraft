import XCTest

@testable import TeamCraft

/// #2179: the store closes once the app is in the background with nothing using it, and only the
/// foreground or a background job with time left opens it again.
final class SuspensionGateTests: XCTestCase {
  func testTheStoreClosesInTheBackgroundOnceTheLastJobEnded() {
    var gate = SuspensionGate()
    XCTAssertFalse(gate.shouldSuspend(isInBackground: false, hasActiveCall: false))
    gate.jobStarted()
    gate.jobStarted()
    XCTAssertFalse(gate.shouldSuspend(isInBackground: true, hasActiveCall: false))
    gate.jobEnded()
    XCTAssertFalse(gate.shouldSuspend(isInBackground: true, hasActiveCall: false))
    gate.jobEnded()
    XCTAssertTrue(gate.shouldSuspend(isInBackground: true, hasActiveCall: false))
    // An extra end never counts below zero.
    gate.jobEnded()
    gate.jobStarted()
    XCTAssertFalse(gate.shouldSuspend(isInBackground: true, hasActiveCall: false))
  }

  func testACallKeepsTheStoreOpen() {
    let gate = SuspensionGate()
    XCTAssertFalse(gate.shouldSuspend(isInBackground: true, hasActiveCall: true))
  }

  func testOnlyTheForegroundOrAJobOpensTheStoreAgain() {
    var gate = SuspensionGate()
    gate.storeSuspended()
    XCTAssertTrue(gate.isStoreSuspended)
    // A screen reading in the background.
    XCTAssertFalse(gate.mayResume(isInBackground: true))
    XCTAssertTrue(gate.mayResume(isInBackground: false))
    gate.jobStarted()
    XCTAssertTrue(gate.mayResume(isInBackground: true))
    gate.storeResumed()
    XCTAssertFalse(gate.isStoreSuspended)
  }

  func testAJobWhoseTimeRanOutNeverOpensTheStoreAgain() {
    var gate = SuspensionGate()
    gate.jobStarted()
    gate.timeExpired()
    gate.storeSuspended()
    XCTAssertFalse(gate.mayResume(isInBackground: true))
    XCTAssertTrue(gate.mayResume(isInBackground: false))
    // A new job brings new time.
    gate.jobStarted()
    XCTAssertTrue(gate.mayResume(isInBackground: true))
  }
}
