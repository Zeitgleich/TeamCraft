import XCTest

@testable import TeamCraft

final class ProcessLockTests: XCTestCase {
  func testOnlyOneHolderAtATime() throws {
    let url = FileManager.default.temporaryDirectory.appendingPathComponent("lock-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: url) }
    let first = ProcessLock(fileURL: url)
    let second = ProcessLock(fileURL: url)
    XCTAssertTrue(first.acquire(timeout: 0.2))
    XCTAssertFalse(second.acquire(timeout: 0.2), "the lock is held")
    first.release()
    XCTAssertTrue(second.acquire(timeout: 0.2), "free again after release")
  }

  /// Two takers in one process at once, with a release in between: the process ends up with one hold
  /// on one descriptor, and a release frees it for another process.
  func testRacingTakersKeepOneHold() {
    let url = FileManager.default.temporaryDirectory.appendingPathComponent("lock-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: url) }
    let lock = ProcessLock(fileURL: url)
    let other = ProcessLock(fileURL: url)
    XCTAssertTrue(other.acquire(timeout: 0.1))
    let group = DispatchGroup()
    let results = Outcomes()
    for index in 0..<2 {
      group.enter()
      DispatchQueue.global().async {
        results.set(index, lock.acquire(timeout: 2))
        group.leave()
      }
    }
    Thread.sleep(forTimeInterval: 0.2)
    other.release()
    XCTAssertEqual(group.wait(timeout: .now() + 5), .success)
    XCTAssertEqual(results.values, [true, true])
    XCTAssertTrue(lock.isHeld)
    lock.release()
    XCTAssertFalse(lock.isHeld)
    XCTAssertTrue(other.acquire(timeout: 0.2), "no second descriptor kept the lock")
    other.release()
  }

  func testAcquiringTwiceKeepsTheHold() {
    let url = FileManager.default.temporaryDirectory.appendingPathComponent("lock-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: url) }
    let lock = ProcessLock(fileURL: url)
    XCTAssertTrue(lock.acquire(timeout: 0.1))
    XCTAssertTrue(lock.acquire(timeout: 0.1))
    XCTAssertTrue(lock.isHeld)
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
