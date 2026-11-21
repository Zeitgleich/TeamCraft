import Foundation

/// Who in this process holds the cross-process lock (docs/e2ee.md §7). The sync loop and short jobs
/// (a push, sealing, a prefetch) share the one `flock`; it is released when the last of them lets
/// go, so no job frees the lock under the sync loop and no job keeps it after it ended. One take
/// runs at a time: another taker waits for it and then joins, so the process never opens two
/// descriptors for it.
@MainActor
final class LockHolds {
  private let lock: any SharedLock
  private var holders: Set<LockHolder> = []
  /// The one take of the lock under way.
  private var maybePending: Task<Bool, Never>?

  init(lock: any SharedLock) {
    self.lock = lock
  }

  func contains(_ holder: LockHolder) -> Bool { holders.contains(holder) }

  /// Takes the lock for `holder`, or joins this process's hold of it; `false`: another process
  /// kept it for `timeout`. The wait for the lock runs off the main actor.
  func take(for holder: LockHolder, timeout: TimeInterval) async -> Bool {
    while let pending = maybePending {
      _ = await pending.value
      // Whoever resumes first after the take ended clears it. Awaiting a finished task doesn't
      // suspend: left to the taker, which can't resume while a waiter loops on the main actor,
      // the loop spun forever and the watchdog killed the app (the 1.0.39 launch hang).
      if maybePending == pending { maybePending = nil }
    }
    if lock.isHeld {
      holders.insert(holder)
      return true
    }
    let lock = lock
    // blocking-ok: on a detached task, off the main thread.
    let pending = Task.detached { lock.acquire(timeout: timeout) }
    maybePending = pending
    let isTaken = await pending.value
    if maybePending == pending { maybePending = nil }
    // A drop (suspension, background expiry) may have let it go while this waited to resume.
    guard isTaken, lock.isHeld else { return false }
    holders.insert(holder)
    return true
  }

  /// `holder` is done; the lock goes once nobody in this process holds it.
  func release(for holder: LockHolder) {
    holders.remove(holder)
    if holders.isEmpty { lock.release() }
  }

  /// The lock goes now, whoever holds it (0xdead10cc): the store closes for a suspension, or the
  /// background time ran out.
  func dropAll() {
    holders.removeAll()
    lock.release()
  }
}

/// A holder of the cross-process lock inside the app: the sync loop, or one short job.
enum LockHolder: Hashable {
  case sync
  case job(UUID)
}

/// The cross-process lock as `LockHolds` uses it: `ProcessLock`, or a fake in tests.
protocol SharedLock: AnyObject, Sendable {
  var isHeld: Bool { get }
  /// Blocks until taken or `timeout` passed; never called on the main actor.
  func acquire(timeout: TimeInterval) -> Bool
  func release()
}

extension ProcessLock: SharedLock {}
