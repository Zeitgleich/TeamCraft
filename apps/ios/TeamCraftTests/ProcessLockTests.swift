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

  func testAcquiringTwiceKeepsTheHold() {
    let url = FileManager.default.temporaryDirectory.appendingPathComponent("lock-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: url) }
    let lock = ProcessLock(fileURL: url)
    XCTAssertTrue(lock.acquire(timeout: 0.1))
    XCTAssertTrue(lock.acquire(timeout: 0.1))
    XCTAssertTrue(lock.isHeld)
  }
}
