import Foundation

/// The main thread never waits (#2184): a call that can block (a token refresh, the process lock)
/// checks first that it isn't on the main thread. Debug builds stop there (`assertionFailure`), so
/// the mistake shows the moment it is made; release builds report each call site once to the crash
/// reports, by name only (docs/crash-reports.md: no content), and go on.
///
/// Core calls that can block are `async` exports and can't be called and waited for on the main
/// thread; this covers what the app itself does that can block.
final class UiThreadGuard: @unchecked Sendable {
  static let shared = UiThreadGuard()

  private let isOnUiThread: @Sendable () -> Bool
  private let failsLoudly: Bool
  /// Guards `reported` and `maybeReporter`; held only for the set and the read, never while reporting.
  private let lock = NSLock()
  private var reported = Set<String>()
  private var maybeReporter: (@Sendable (String) -> Void)?

  init(
    isOnUiThread: @escaping @Sendable () -> Bool = { Thread.isMainThread },
    failsLoudly: Bool = UiThreadGuard.isDebugBuild
  ) {
    self.isOnUiThread = isOnUiThread
    self.failsLoudly = failsLoudly
  }

  /// Where a release build reports a call site: the app sets it while crash reports run, `nil`
  /// otherwise. The notification extension sets none.
  func setReporter(_ maybeReporter: (@Sendable (String) -> Void)?) {
    lock.withLock { self.maybeReporter = maybeReporter }
  }

  /// Call first in anything that may block. `callSite` names the function, never what it works on.
  func check(_ callSite: String) {
    guard isOnUiThread() else { return }
    let isFirst = lock.withLock { reported.insert(callSite).inserted }
    if failsLoudly {
      assertionFailure("\(callSite) can block and ran on the main thread (#2184)")
    }
    guard isFirst, let reporter = lock.withLock({ maybeReporter }) else { return }
    reporter(callSite)
  }

  /// The call sites reported so far, for the tests.
  var reportedCallSites: Set<String> {
    lock.withLock { reported }
  }

  #if DEBUG
    static let isDebugBuild = true
  #else
    static let isDebugBuild = false
  #endif
}
