import XCTest

@testable import TeamCraft

/// The core's change callbacks reach the main actor once per burst (a catch-up after a long time
/// offline reports thousands), and never one too few.
@MainActor
final class ChangeCoalescerTests: XCTestCase {
  func testABurstRunsTheHandlerOnce() {
    let scheduler = ManualScheduler()
    var runs = 0
    let coalescer = ChangeCoalescer(schedule: scheduler.schedule) { runs += 1 }
    for _ in 0..<1_000 { coalescer.signal() }
    XCTAssertEqual(scheduler.pending, 1)
    scheduler.runAll()
    XCTAssertEqual(runs, 1)
  }

  func testAChangeAfterTheRunSchedulesTheNextOne() {
    let scheduler = ManualScheduler()
    var runs = 0
    let coalescer = ChangeCoalescer(schedule: scheduler.schedule) { runs += 1 }
    coalescer.signal()
    scheduler.runAll()
    coalescer.signal()
    coalescer.signal()
    XCTAssertEqual(scheduler.pending, 1)
    scheduler.runAll()
    XCTAssertEqual(runs, 2)
  }

  func testAChangeWhileTheHandlerRunsIsNotLost() {
    let scheduler = ManualScheduler()
    var runs = 0
    var maybeCoalescer: ChangeCoalescer?
    let coalescer = ChangeCoalescer(schedule: scheduler.schedule) {
      runs += 1
      if runs == 1 { maybeCoalescer?.signal() }
    }
    maybeCoalescer = coalescer
    coalescer.signal()
    scheduler.runAll()
    XCTAssertEqual(runs, 2)
  }
}

/// Keeps scheduled work until the test runs it, on the main thread.
private final class ManualScheduler: @unchecked Sendable {
  private var work: [@Sendable () -> Void] = []

  var pending: Int { work.count }

  var schedule: @Sendable (@escaping @Sendable () -> Void) -> Void {
    { [self] item in work.append(item) }
  }

  /// Runs what is scheduled, and what that schedules, until nothing is left.
  func runAll() {
    while !work.isEmpty {
      let next = work.removeFirst()
      next()
    }
  }
}
