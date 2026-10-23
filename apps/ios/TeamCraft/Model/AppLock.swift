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
/// `lockAfter` in the background. `now` is injectable for tests.
struct AppLockClock {
  private var maybeBackgroundedAt: Date?

  mutating func didEnterBackground(at now: Date) {
    maybeBackgroundedAt = now
  }

  /// Whether the app must lock now that it is active again; uses up the background phase.
  mutating func shouldLock(at now: Date, isEnabled: Bool, lockAfter: LockAfter) -> Bool {
    guard let backgroundedAt = maybeBackgroundedAt else { return false }
    maybeBackgroundedAt = nil
    return isEnabled && now.timeIntervalSince(backgroundedAt) >= lockAfter.seconds
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
  }

  var lockAfter: LockAfter {
    LockAfter(rawValue: defaults.string(forKey: Self.lockAfterKey) ?? "") ?? Self.defaultLockAfter
  }

  /// Whether the device has Face ID, Touch ID or a passcode to check against.
  static var isAvailable: Bool {
    LAContext().canEvaluatePolicy(.deviceOwnerAuthentication, error: nil)
  }

  func didEnterBackground() {
    clock.didEnterBackground(at: .now)
  }

  func didBecomeActive() {
    if clock.shouldLock(at: .now, isEnabled: defaults.bool(forKey: Self.enabledKey), lockAfter: lockAfter) {
      isLocked = true
    }
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
