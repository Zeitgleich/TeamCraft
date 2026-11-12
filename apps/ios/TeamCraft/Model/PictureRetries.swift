import Foundation
import Observation

/// The core's retry arithmetic (`PictureRetryRounds`), as `PictureRetries` uses it; a fake in tests.
protocol RetryRounds: AnyObject {
  /// `nil`: part of the planned round; else the wait in ms before the round runs.
  func failed(maybeServerWaitMs: UInt64?) -> UInt64?
  func roundStarted()
  func succeeded()
  func restart()
}

/// The rounds of the core's policy: 5 seconds doubling up to 5 minutes, with jitter (issue #743).
final class CoreRetryRounds: RetryRounds {
  private let rounds = PictureRetryRounds()

  func failed(maybeServerWaitMs: UInt64?) -> UInt64? { rounds.failed(maybeServerWaitMs: maybeServerWaitMs) }
  func roundStarted() { rounds.roundStarted() }
  func succeeded() { rounds.succeeded() }
  func restart() { rounds.restart() }
}

/// When pictures that failed during an outage are asked for again (issue #1617), as on Android, the
/// web and the desktop: one pause for all of them in place of a timer per picture, with the core's
/// capped exponential backoff. A picture that failed for a reason that passes calls `failed`; the
/// screens read `round` and ask again when it moves. The end of an outage (the device back online,
/// the server connection back) runs the round at once and starts the backoff over. Nothing runs
/// while no picture waits: no polling. What is on screen meanwhile (the cached picture, the
/// initials, the thumbnail) stays. Refusals (not found, forbidden, a file that doesn't decrypt) are
/// not asked for again by themselves.
@MainActor
@Observable
final class PictureRetries {
  /// Runs `fire` after `waitMs`; the answer cancels it.
  typealias Schedule = @MainActor (_ waitMs: UInt64, _ fire: @escaping @MainActor () -> Void) -> () -> Void

  /// Counts the rounds that ran: a picture that failed is asked for again when this moves.
  private(set) var round = 0

  @ObservationIgnored private let rounds: RetryRounds
  @ObservationIgnored private let schedule: Schedule
  @ObservationIgnored private var maybeCancelTimer: (() -> Void)?
  @ObservationIgnored private var isWaiting = false

  init(rounds: RetryRounds = CoreRetryRounds(), schedule: @escaping Schedule = PictureRetries.afterWait) {
    self.rounds = rounds
    self.schedule = schedule
  }

  /// A picture did not load for a reason that passes; `maybeServerWaitMs` is what the server asked
  /// to wait, if it said. Plans the next round unless one is planned.
  func failed(maybeServerWaitMs: UInt64? = nil) {
    isWaiting = true
    guard let wait = rounds.failed(maybeServerWaitMs: maybeServerWaitMs) else { return }
    maybeCancelTimer?()
    maybeCancelTimer = schedule(wait) { [weak self] in self?.startRound() }
  }

  /// A picture came through: the next failure starts from the shortest wait again.
  func succeeded() {
    rounds.succeeded()
  }

  /// The device's network or the server connection came back: what waits asks now, and the backoff
  /// starts over. Nothing happens while no picture waits.
  func outageEnded() {
    guard isWaiting else { return }
    maybeCancelTimer?()
    maybeCancelTimer = nil
    isWaiting = false
    rounds.restart()
    round += 1
  }

  private func startRound() {
    maybeCancelTimer = nil
    isWaiting = false
    rounds.roundStarted()
    round += 1
  }

  /// Trouble that passes: no connection, a storage outage, 5xx, 429 (the core answers `Network`
  /// for those). Signed out, not found, forbidden and the rest are final for now.
  nonisolated static func isTransient(_ error: Error) -> Bool {
    CoreFailure(error) == .network
  }

  /// Whether a file's download failed for a reason that passes (`FileTransfer.reason`): the network
  /// or the storage's 503. Missing, corrupted (it doesn't decrypt), too large, a local file error or
  /// an intercepted connection don't get better by asking again.
  nonisolated static func isTransient(fileFailure maybeReason: String?) -> Bool {
    maybeReason == "network" || maybeReason == "storage_unavailable"
  }

  /// The real timer: a task that sleeps.
  static func afterWait(_ waitMs: UInt64, _ fire: @escaping @MainActor () -> Void) -> () -> Void {
    let task = Task { @MainActor in
      try? await Task.sleep(nanoseconds: waitMs.multipliedReportingOverflow(by: 1_000_000).partialValue)
      guard !Task.isCancelled else { return }
      fire()
    }
    return { task.cancel() }
  }
}

/// Which failed pictures of a screen are asked for again: each one that failed for a reason that
/// passes is marked with the round it failed in, and is due once a later round ran.
struct RetryMarks<Key: Hashable> {
  private var failedIn: [Key: Int] = [:]

  /// `key` failed in `round`; `true` when it wasn't marked yet (then report it to `PictureRetries`).
  mutating func failed(_ key: Key, round: Int) -> Bool {
    guard failedIn[key] == nil else { return false }
    failedIn[key] = round
    return true
  }

  /// Whether `key` is asked for again now; a due mark goes, so it is asked once per round.
  mutating func takeDue(_ key: Key, round: Int) -> Bool {
    guard let failed = failedIn[key], round != failed else { return false }
    failedIn[key] = nil
    return true
  }

  /// `key` came through, left the screen, or failed for good.
  mutating func forget(_ key: Key) {
    failedIn[key] = nil
  }

  func isMarked(_ key: Key) -> Bool { failedIn[key] != nil }
}
