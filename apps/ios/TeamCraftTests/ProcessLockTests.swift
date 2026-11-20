import XCTest

@testable import TeamCraft

final class ProcessLockTests: XCTestCase {
  func testOnlyOneHolderAtATime() async throws {
    let url = FileManager.default.temporaryDirectory.appendingPathComponent("lock-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: url) }
    let first = ProcessLock(fileURL: url)
    let second = ProcessLock(fileURL: url)
    let isFirstHeld = await take(first, timeout: 0.2)
    XCTAssertTrue(isFirstHeld)
    let isSecondHeld = await take(second, timeout: 0.2)
    XCTAssertFalse(isSecondHeld, "the lock is held")
    first.release()
    let isSecondHeldNow = await take(second, timeout: 0.2)
    XCTAssertTrue(isSecondHeldNow, "free again after release")
  }

  /// Two takers in one process at once, with a release in between: the process ends up with one hold
  /// on one descriptor, and a release frees it for another process.
  func testRacingTakersKeepOneHold() async {
    let url = FileManager.default.temporaryDirectory.appendingPathComponent("lock-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: url) }
    let lock = ProcessLock(fileURL: url)
    let other = ProcessLock(fileURL: url)
    let isOtherHeld = await take(other, timeout: 0.1)
    XCTAssertTrue(isOtherHeld)
    let group = DispatchGroup()
    let results = Outcomes()
    for index in 0..<2 {
      group.enter()
      DispatchQueue.global().async {
        results.set(index, lock.acquire(timeout: 2))
        group.leave()
      }
    }
    try? await Task.sleep(for: .milliseconds(200))
    other.release()
    await withCheckedContinuation { done in group.notify(queue: .global()) { done.resume() } }
    XCTAssertEqual(results.values, [true, true])
    XCTAssertTrue(lock.isHeld)
    lock.release()
    XCTAssertFalse(lock.isHeld)
    let isOtherHeldAgain = await take(other, timeout: 0.2)
    XCTAssertTrue(isOtherHeldAgain, "no second descriptor kept the lock")
    other.release()
  }

  func testAcquiringTwiceKeepsTheHold() async {
    let url = FileManager.default.temporaryDirectory.appendingPathComponent("lock-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: url) }
    let lock = ProcessLock(fileURL: url)
    let isHeld = await take(lock, timeout: 0.1)
    XCTAssertTrue(isHeld)
    let isHeldAgain = await take(lock, timeout: 0.1)
    XCTAssertTrue(isHeldAgain)
    XCTAssertTrue(lock.isHeld)
  }

  /// The lock waits, so the tests take it off the main thread like the app (#2184, `UiThreadGuard`).
  private func take(_ lock: ProcessLock, timeout: TimeInterval) async -> Bool {
    await offMain { lock.acquire(timeout: timeout) }
  }
}

/// What each racing thread got.
private final class Outcomes: @unchecked Sendable {
  private let guardLock = NSLock()
  private var stored = [false, false]

  var values: [Bool] { guardLock.withLock { stored } }

  func set(_ index: Int, _ value: Bool) {
    guardLock.withLock { stored[index] = value }
  }
}
