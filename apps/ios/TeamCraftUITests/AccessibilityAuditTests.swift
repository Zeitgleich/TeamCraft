import XCTest

/// Accessibility audit of the app's screens (#2287): `performAccessibilityAudit` checks contrast,
/// hit regions, element descriptions and traits of what is on screen, the iOS counterpart of axe on
/// the web and the Accessibility Test Framework on Android.
///
/// Only the screens that need no server are reachable: the app has no seeded demo mode, so the
/// signed-in screens wait for one (listed in docs/accessibility.md). Issue types that are only
/// informational today are logged, not failed, so they show up in the test log instead of being
/// switched off.
final class AccessibilityAuditTests: XCTestCase {
  /// What the audit gates on. Dynamic Type and clipped text are reported below, not gated: they
  /// depend on the simulator's text size and are tracked in #2286.
  private static let gated: XCUIAccessibilityAuditType = [
    .contrast, .elementDetection, .hitRegion, .sufficientElementDescription, .trait,
  ]

  override func setUpWithError() throws {
    continueAfterFailure = false
  }

  func testFirstScreenPassesTheAccessibilityAudit() throws {
    let app = XCUIApplication()
    app.launch()
    XCTAssertTrue(app.wait(for: .runningForeground, timeout: 30))
    try audit(app, named: "first screen")
  }

  func testFirstScreenInDarkAppearancePassesTheAccessibilityAudit() throws {
    let app = XCUIApplication()
    app.launchArguments += ["-AppleInterfaceStyle", "Dark"]
    app.launch()
    XCTAssertTrue(app.wait(for: .runningForeground, timeout: 30))
    try audit(app, named: "first screen, dark")
  }

  private func audit(_ app: XCUIApplication, named screen: String) throws {
    try app.performAccessibilityAudit(for: Self.gated)
    // The rest runs as a report: every issue is logged and none fails the test.
    let reported: XCUIAccessibilityAuditType = [.dynamicType, .textClipped]
    try app.performAccessibilityAudit(for: reported) { issue in
      NSLog("accessibility audit (\(screen)): \(issue.compactDescription)")
      return true
    }
  }
}
