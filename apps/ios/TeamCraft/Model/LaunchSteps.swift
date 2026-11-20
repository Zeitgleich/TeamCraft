import Foundation
import Sentry
import os

/// Where a start stands, as breadcrumbs of the crash reports (docs/crash-reports.md, "Launch
/// steps") and lines in the system log: a launch that hangs or is killed says which step it was in.
/// Each step is a fixed name; nothing about the account, its content or the server
/// goes along.
enum LaunchSteps {
  /// The breadcrumbs' category; `CrashReports` lets only these through.
  static let category = "launch"

  private static let log = Logger(subsystem: "de.zeitgleich.teamcraft", category: "Launch")

  enum Step: String {
    case bootstrap = "bootstrap"
    case storeOpening = "store_opening"
    case storeOpen = "store_open"
    case storeOpenFailed = "store_open_failed"
    case accountState = "account_state"
    case syncLockWaiting = "sync_lock_waiting"
    case syncLockTaken = "sync_lock_taken"
    case syncStarted = "sync_started"
    case ready = "ready"
  }

  /// Notes `step`. Cheap; a no-op for the crash reports while they are off.
  static func mark(_ step: Step) {
    log.info("launch step \(step.rawValue, privacy: .public)")
    let breadcrumb = Breadcrumb(level: .info, category: category)
    breadcrumb.message = step.rawValue
    SentrySDK.addBreadcrumb(breadcrumb)
  }
}
