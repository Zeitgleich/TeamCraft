import XCTest

@testable import TeamCraft

/// The app's holds of the cross-process lock. The 1.0.39 launch hang: a taker waiting for a take
/// that had already ended looped on the main actor forever. Several takers at once must all get
/// through, with one `flock` taken.
@MainActor
final class LockHoldsTests: XCTestCase {
  func testTakersWaitingForATakeThatEndedAllJoinIt() async {
    let lock = FakeLock(isFree: true)
    lock.holdAcquire()
    let holds = LockHolds(lock: lock)
    let takers = (0..<8).map { _ in
      Task { await holds.take(for: .job(UUID()), timeout: 5) }
    }
    // Every taker is on its way before the take ends: the first one takes, the others wait for it.
    await lock.untilAcquireCalls(1)
    for _ in 0..<20 { await Task.yield() }
    lock.finishAcquire()
    var results: [Bool] = []
    for taker in takers { results.append(await taker.value) }
    XCTAssertEqual(results, Array(repeating: true, count: 8))
    XCTAssertEqual(lock.acquireCalls, 1)
  }

  func testATakerAfterAnEndedTakeJoinsWithoutTakingAgain() async {
    let lock = FakeLock(isFree: true)
    let holds = LockHolds(lock: lock)
    let first = await holds.take(for: .sync, timeout: 5)
    let second = await holds.take(for: .job(UUID()), timeout: 5)
    XCTAssertTrue(first)
    XCTAssertTrue(second)
    XCTAssertEqual(lock.acquireCalls, 1)
  }

  func testTakersOfALockHeldElsewhereEachTryAndGiveUp() async {
    let lock = FakeLock(isFree: false)
    let holds = LockHolds(lock: lock)
    let takers = (0..<4).map { _ in
      Task { await holds.take(for: .job(UUID()), timeout: 0) }
    }
    var results: [Bool] = []
    for taker in takers { results.append(await taker.value) }
    XCTAssertEqual(results, Array(repeating: false, count: 4))
    XCTAssertEqual(lock.acquireCalls, 4)
  }

  func testTheLockGoesWithTheLastHolder() async {
    let lock = FakeLock(isFree: true)
    let holds = LockHolds(lock: lock)
    let job = LockHolder.job(UUID())
    _ = await holds.take(for: .sync, timeout: 5)
    _ = await holds.take(for: job, timeout: 5)
    holds.release(for: job)
    XCTAssertTrue(lock.isHeld)
    holds.release(for: .sync)
    XCTAssertFalse(lock.isHeld)
  }
}

/// A lock whose take can be held back until the test lets it end.
private final class FakeLock: SharedLock, @unchecked Sendable {
  private let state = NSLock()
  private let isFree: Bool
  private var held = false
  private var calls = 0
  private var gate: DispatchSemaphore?

  init(isFree: Bool) {
    self.isFree = isFree
  }

  var isHeld: Bool { state.withLock { held } }
  var acquireCalls: Int { state.withLock { calls } }

  /// The next take blocks until `finishAcquire`.
  func holdAcquire() { state.withLock { gate = DispatchSemaphore(value: 0) } }
  func finishAcquire() { state.withLock { gate }?.signal() }

  /// Yields until `count` takes started (each starts on another thread).
  func untilAcquireCalls(_ count: Int) async {
    while acquireCalls < count { await Task.yield() }
  }

  func acquire(timeout: TimeInterval) -> Bool {
    let maybeGate: DispatchSemaphore? = state.withLock {
      calls += 1
      return gate
    }
    maybeGate?.wait()
    return state.withLock {
      held = isFree
      return isFree
    }
  }

  func release() { state.withLock { held = false } }
}
