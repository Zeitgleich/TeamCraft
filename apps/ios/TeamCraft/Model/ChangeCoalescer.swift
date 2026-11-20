import Foundation

/// Collapses the core's change callbacks into one main-actor update at a time. The core calls
/// `on_change` from its threads after every gateway event and for every conversation it caught up;
/// after a long time offline that is thousands in a few seconds. One main-actor job each, every one
/// re-reading the lists, kept the main thread busy until the watchdog killed the app at launch, only
/// with a network (offline nothing arrives). Now a burst waits `interval` and runs the handler once;
/// a change during the handler schedules one more run, so nothing is missed.
final class ChangeCoalescer: @unchecked Sendable {
  /// About two frames: a burst becomes one update, a single change still shows at once.
  static let interval: Duration = .milliseconds(50)

  private let lock = NSLock()
  private var isScheduled = false
  private let schedule: @Sendable (@escaping @Sendable () -> Void) -> Void
  /// The handler, called where `schedule` runs its closure: on the main actor.
  private let handler: @Sendable () -> Void

  /// `schedule` runs its closure later (tests run it by hand); `handler` is called on the main actor.
  init(
    schedule: (@Sendable (@escaping @Sendable () -> Void) -> Void)? = nil,
    handler: @escaping @MainActor () -> Void
  ) {
    let run: @Sendable () -> Void = { MainActor.assumeIsolated { handler() } }
    self.schedule =
      schedule ?? { work in
        Task { @MainActor in
          try? await Task.sleep(for: Self.interval)
          work()
        }
      }
    self.handler = run
  }

  /// From any thread: the store changed.
  func signal() {
    let isFirst: Bool = lock.withLock {
      guard !isScheduled else { return false }
      isScheduled = true
      return true
    }
    guard isFirst else { return }
    schedule { [self] in
      // Cleared before the handler reads: a change while it runs schedules the next run.
      lock.withLock { isScheduled = false }
      handler()
    }
  }
}
