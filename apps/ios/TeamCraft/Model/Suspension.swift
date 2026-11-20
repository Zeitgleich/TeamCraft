import UIKit

/// When the app closes its local store for a suspension, and when it may open it again (#2179).
///
/// iOS kills a suspended app that holds a lock on a file in the App Group container (0xdead10cc),
/// and in WAL mode SQLite keeps one on the database's `-shm` file for as long as a connection is
/// open. So every stay in the background ends with the store closed: the scene going to the
/// background, each background job (a push, a background task, a notification action) and the
/// expiry of the background time. Background jobs may overlap; the store closes once the last one
/// ended, and only a job or the foreground opens it again, never a screen reading in the background.
struct SuspensionGate: Equatable {
  /// Background jobs under way, each with background time from iOS.
  private(set) var runningJobs = 0
  /// The store is closed (`TeamCraft.suspend`) and not yet opened again.
  private(set) var isStoreSuspended = false
  /// The background time ran out: the jobs still under way run on without it, and must not open
  /// the store again, since iOS suspends the app any moment. A new job brings new time.
  private(set) var isTimeExpired = false

  mutating func jobStarted() {
    runningJobs += 1
    isTimeExpired = false
  }

  mutating func timeExpired() {
    isTimeExpired = true
  }

  mutating func jobEnded() {
    runningJobs = max(0, runningJobs - 1)
  }

  /// Whether the store closes now: the app is in the background and nothing there needs it. A call
  /// keeps the app running (CallKit, the audio session) and needs the store; its end closes it.
  func shouldSuspend(isInBackground: Bool, hasActiveCall: Bool) -> Bool {
    isInBackground && runningJobs == 0 && !hasActiveCall
  }

  /// Whether a use of the core opens the closed store again: in the foreground, or for a background
  /// job with time left, whose end closes it again.
  func mayResume(isInBackground: Bool) -> Bool {
    !isInBackground || (runningJobs > 0 && !isTimeExpired)
  }

  mutating func storeSuspended() {
    isStoreSuspended = true
  }

  mutating func storeResumed() {
    isStoreSuspended = false
  }
}

/// Background time from iOS for one job (`beginBackgroundTask`). The expiry handler runs once the
/// time is up; the time ends once, by the job or by its expiry, whichever comes first.
@MainActor
final class BackgroundTime {
  private var identifier = UIBackgroundTaskIdentifier.invalid

  init(name: String, expired: @escaping @MainActor () -> Void) {
    identifier = UIApplication.shared.beginBackgroundTask(withName: name) { [weak self] in
      // iOS calls it on the main thread.
      MainActor.assumeIsolated {
        expired()
        self?.end()
      }
    }
  }

  func end() {
    guard identifier != .invalid else { return }
    UIApplication.shared.endBackgroundTask(identifier)
    identifier = .invalid
  }
}
