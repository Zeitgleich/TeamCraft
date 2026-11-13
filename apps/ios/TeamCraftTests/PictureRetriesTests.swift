import XCTest

@testable import TeamCraft

/// Pictures that failed during an outage are asked for again in shared rounds (#1617).
@MainActor
final class PictureRetriesTests: XCTestCase {
  func testEverythingFailingAtOnceWaitsForOneRound() {
    let rounds = FakeRounds()
    let timers = ManualTimers()
    let retries = PictureRetries(rounds: rounds, schedule: timers.schedule)
    for _ in 0..<20 { retries.failed() }
    XCTAssertEqual(timers.pending.count, 1, "one timer for all pictures, none per picture")
    XCTAssertEqual(timers.pending.first?.waitMs, 5_000)
    XCTAssertEqual(retries.round, 0)
    timers.fireAll()
    XCTAssertEqual(retries.round, 1)
    XCTAssertEqual(rounds.roundsStarted, 1)
  }

  func testNothingRunsWhileNoPictureWaits() {
    let rounds = FakeRounds()
    let timers = ManualTimers()
    let retries = PictureRetries(rounds: rounds, schedule: timers.schedule)
    retries.outageEnded()
    retries.succeeded()
    XCTAssertTrue(timers.pending.isEmpty)
    XCTAssertEqual(retries.round, 0)
    XCTAssertEqual(rounds.restarts, 0)
  }

  func testTheEndOfAnOutageRunsTheRoundAtOnceAndStartsOver() {
    let rounds = FakeRounds()
    let timers = ManualTimers()
    let retries = PictureRetries(rounds: rounds, schedule: timers.schedule)
    retries.failed()
    retries.outageEnded()
    XCTAssertEqual(retries.round, 1)
    XCTAssertEqual(rounds.restarts, 1)
    XCTAssertEqual(timers.cancelled, 1, "the planned round is dropped")
    timers.fireAll()
    XCTAssertEqual(retries.round, 1, "a cancelled timer runs no second round")
    retries.outageEnded()
    XCTAssertEqual(retries.round, 1, "nothing waits any more")
  }

  func testTheServersWaitIsPassedOn() {
    let rounds = FakeRounds()
    let timers = ManualTimers()
    let retries = PictureRetries(rounds: rounds, schedule: timers.schedule)
    retries.failed(maybeServerWaitMs: 30_000)
    XCTAssertEqual(rounds.serverWaits, [30_000])
  }

  func testTheCoresRoundsBackOffWithJitter() {
    let rounds = CoreRetryRounds()
    let wait = try! XCTUnwrap(rounds.failed(maybeServerWaitMs: nil))
    XCTAssertTrue((2_500...5_000).contains(wait), "\(wait) ms is the first wait with jitter")
    XCTAssertNil(rounds.failed(maybeServerWaitMs: nil), "part of the planned round")
    rounds.roundStarted()
    let second = try! XCTUnwrap(rounds.failed(maybeServerWaitMs: nil))
    XCTAssertTrue((5_000...10_000).contains(second), "\(second) ms doubles")
    rounds.roundStarted()
    for _ in 0..<12 {
      _ = rounds.failed(maybeServerWaitMs: nil)
      rounds.roundStarted()
    }
    let capped = try! XCTUnwrap(rounds.failed(maybeServerWaitMs: nil))
    XCTAssertLessThanOrEqual(capped, 5 * 60 * 1_000, "capped at five minutes")
    rounds.restart()
    let fresh = try! XCTUnwrap(rounds.failed(maybeServerWaitMs: nil))
    XCTAssertLessThanOrEqual(fresh, 5_000, "the backoff started over")
  }

  func testOnlyTroubleThatPassesIsAskedAgain() {
    XCTAssertTrue(PictureRetries.isTransient(FakeCoreError.Network(message: "503")))
    XCTAssertFalse(PictureRetries.isTransient(FakeCoreError.Core(message: "not found")))
    XCTAssertFalse(PictureRetries.isTransient(FakeCoreError.SignedOut))
    XCTAssertTrue(PictureRetries.isTransient(fileFailure: "network"))
    XCTAssertTrue(PictureRetries.isTransient(fileFailure: "storage_unavailable"))
    for refused in ["missing", "corrupted", "too_large", "local_file", "intercepted", nil] {
      XCTAssertFalse(PictureRetries.isTransient(fileFailure: refused), "\(refused ?? "nil")")
    }
  }

  func testAFailedPictureIsAskedAgainOncePerRound() {
    var marks = RetryMarks<String>()
    XCTAssertTrue(marks.failed("a", round: 3), "the first failure is reported")
    XCTAssertFalse(marks.failed("a", round: 3), "reported once")
    XCTAssertFalse(marks.takeDue("a", round: 3), "not before the round ran")
    XCTAssertTrue(marks.takeDue("a", round: 4))
    XCTAssertFalse(marks.takeDue("a", round: 4), "asked once per round")
    XCTAssertFalse(marks.takeDue("b", round: 9), "never failed")
    XCTAssertTrue(marks.failed("a", round: 4))
    marks.forget("a")
    XCTAssertFalse(marks.isMarked("a"))
  }
}

private enum FakeCoreError: Error {
  case Network(message: String)
  case Core(message: String)
  case SignedOut
}

private final class FakeRounds: RetryRounds {
  private var isWaiting = false
  var roundsStarted = 0
  var restarts = 0
  var serverWaits: [UInt64] = []

  func failed(maybeServerWaitMs: UInt64?) -> UInt64? {
    if let wait = maybeServerWaitMs { serverWaits.append(wait) }
    guard !isWaiting else { return nil }
    isWaiting = true
    return maybeServerWaitMs ?? 5_000
  }

  func roundStarted() {
    isWaiting = false
    roundsStarted += 1
  }

  func succeeded() {}

  func restart() {
    isWaiting = false
    restarts += 1
  }
}

@MainActor
private final class ManualTimers {
  struct Timer {
    let waitMs: UInt64
    let fire: @MainActor () -> Void
    var isCancelled = false
  }

  private var timers: [Timer] = []
  var cancelled = 0

  var pending: [Timer] { timers.filter { !$0.isCancelled } }

  var schedule: PictureRetries.Schedule {
    { [unowned self] waitMs, fire in
      timers.append(Timer(waitMs: waitMs, fire: fire))
      let index = timers.count - 1
      return { [unowned self] in
        if !timers[index].isCancelled { cancelled += 1 }
        timers[index].isCancelled = true
      }
    }
  }

  func fireAll() {
    let due = pending
    timers = timers.map { var timer = $0; timer.isCancelled = true; return timer }
    for timer in due { timer.fire() }
  }
}
