import Foundation
import LocalAuthentication
import Observation

/// How long the app may be in the background before it asks to unlock again (same choices as
/// the Android app).
enum LockAfter: String, CaseIterable, Identifiable {
  case immediately
  case oneMinute
  case fiveMinutes
  case fifteenMinutes

  var id: String { rawValue }

  var seconds: TimeInterval {
    switch self {
    case .immediately: return 0
    case .oneMinute: return 60
    case .fiveMinutes: return 5 * 60
    case .fifteenMinutes: return 15 * 60
    }
  }
}

/// Decides when the app is locked: at launch, and when it comes back after more than
/// `lockAfter` in the background. Measured on the continuous clock (it keeps counting while the
/// device sleeps and can't be set back like the wall clock). `now` is injectable for tests.
struct AppLockClock {
  private var maybeBackgroundedAt: ContinuousClock.Instant?

  mutating func didEnterBackground(at now: ContinuousClock.Instant) {
    maybeBackgroundedAt = now
  }

  /// Whether the app must lock now that it is active again; uses up the background phase.
  mutating func shouldLock(at now: ContinuousClock.Instant, isEnabled: Bool, lockAfter: LockAfter) -> Bool {
    guard let backgroundedAt = maybeBackgroundedAt else { return false }
    maybeBackgroundedAt = nil
    let elapsed = backgroundedAt.duration(to: now)
    // A negative span can't happen on this clock; if it did, locking is the safe answer.
    return isEnabled && (elapsed < .zero || elapsed >= .seconds(lockAfter.seconds))
  }
}

/// "App lock" (Face ID, Touch ID or the device passcode) and "hide content in the app switcher".
/// Device settings, off by default; the data itself is protected by the sealed store either way.
@Observable
final class AppLock {
  static let enabledKey = "app_lock_enabled"
  static let lockAfterKey = "app_lock_after"
  static let privacyCoverKey = "privacy_cover"
  static let defaultLockAfter = LockAfter.oneMinute

  private(set) var isLocked: Bool
  @ObservationIgnored private var clock = AppLockClock()
  @ObservationIgnored private let defaults: UserDefaults

  init(defaults: UserDefaults = .standard) {
    self.defaults = defaults
    isLocked = defaults.bool(forKey: Self.enabledKey)
    SharedContainer().isAppLockOn = isEnabled
  }

  var isEnabled: Bool { defaults.bool(forKey: Self.enabledKey) }

  /// The cover over the app while it is not active: on when asked for, and always with app lock
  /// (the app switcher must not show what the lock hides).
  var isCoverWanted: Bool {
    isEnabled || defaults.bool(forKey: Self.privacyCoverKey)
  }

  var lockAfter: LockAfter {
    LockAfter(rawValue: defaults.string(forKey: Self.lockAfterKey) ?? "") ?? Self.defaultLockAfter
  }

  /// Whether the device has Face ID, Touch ID or a passcode to check against.
  static var isAvailable: Bool {
    LAContext().canEvaluatePolicy(.deviceOwnerAuthentication, error: nil)
  }

  func didEnterBackground() {
    clock.didEnterBackground(at: ContinuousClock.now)
    // Locked already in the background when there is no delay, so the app comes back locked.
    if isEnabled && lockAfter == .immediately {
      isLocked = true
    }
  }

  func didBecomeActive() {
    if clock.shouldLock(at: ContinuousClock.now, isEnabled: isEnabled, lockAfter: lockAfter) {
      isLocked = true
    }
  }

  /// Asks for Face ID / Touch ID or the passcode before the lock is weakened (switched off or
  /// given a longer delay); `true` if allowed. Without any authenticator there is nothing to ask.
  @MainActor
  static func confirmChange(reason: String) async -> Bool {
    let context = LAContext()
    guard context.canEvaluatePolicy(.deviceOwnerAuthentication, error: nil) else { return true }
    return (try? await context.evaluatePolicy(.deviceOwnerAuthentication, localizedReason: reason)) == true
  }

  /// Asks for Face ID / Touch ID or the passcode. Without any, the lock is pointless and opens.
  @MainActor
  func unlock(reason: String) async {
    let context = LAContext()
    guard context.canEvaluatePolicy(.deviceOwnerAuthentication, error: nil) else {
      isLocked = false
      return
    }
    if (try? await context.evaluatePolicy(.deviceOwnerAuthentication, localizedReason: reason)) == true {
      isLocked = false
    }
  }
}
