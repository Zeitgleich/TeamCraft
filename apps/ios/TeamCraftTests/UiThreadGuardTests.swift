import XCTest

@testable import TeamCraft

/// #2184: a call that can block is reported once per call site when it runs on the main thread, and
/// nothing happens off it. (The debug build's `assertionFailure` stops the process, so these guards
/// are built with `failsLoudly: false`.)
final class UiThreadGuardTests: XCTestCase {
  func testOffTheMainThreadNothingIsReported() async {
    let reports = Reports()
    let guardian = UiThreadGuard(failsLoudly: false)
    guardian.setReporter { reports.add($0) }
    await offMain { guardian.check("ProcessLock.acquire") }
    XCTAssertEqual(reports.values, [])
    XCTAssertTrue(guardian.reportedCallSites.isEmpty)
  }

  @MainActor
  func testOnTheMainThreadEachCallSiteIsReportedOnce() {
    let reports = Reports()
    let guardian = UiThreadGuard(failsLoudly: false)
    guardian.setReporter { reports.add($0) }
    for _ in 0..<3 { guardian.check("SessionVault.currentAccessToken") }
    guardian.check("ProcessLock.acquire")
    XCTAssertEqual(reports.values, ["SessionVault.currentAccessToken", "ProcessLock.acquire"])
  }

  func testWithoutAReporterTheCallSiteIsStillNoted() {
    let guardian = UiThreadGuard(isOnUiThread: { true }, failsLoudly: false)
    guardian.check("ProcessLock.acquire")
    XCTAssertEqual(guardian.reportedCallSites, ["ProcessLock.acquire"])
  }
}

private final class Reports: @unchecked Sendable {
  private let lock = NSLock()
  private var stored: [String] = []

  var values: [String] { lock.withLock { stored } }

  func add(_ value: String) {
    lock.withLock { stored.append(value) }
  }
}
