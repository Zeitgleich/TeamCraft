import Sentry
import XCTest

@testable import TeamCraft

/// The launch steps a crash report carries (docs/crash-reports.md, "Launch steps"): a hang or kill
/// reported at the next start keeps its own run's steps and none of this run's log lines.
final class LaunchStepsTests: XCTestCase {
  private let startedAt = Date(timeIntervalSince1970: 1_000_000)

  func testAFatalHangOfTheRunBeforeKeepsItsStepsWithoutThisRunsLog() throws {
    let event = fatalEvent(at: startedAt.addingTimeInterval(-60), steps: ["bootstrap", "store_opening"])
    let prepared = try XCTUnwrap(prepare(event))
    XCTAssertEqual(prepared.breadcrumbs?.map(\.message), ["bootstrap", "store_opening"])
    XCTAssertEqual(prepared.breadcrumbs?.map(\.category), [LaunchSteps.category, LaunchSteps.category])
  }

  func testACrashOfThisRunGetsItsStepsAndTheLog() throws {
    let event = fatalEvent(at: startedAt.addingTimeInterval(5), steps: ["bootstrap"])
    let prepared = try XCTUnwrap(prepare(event))
    XCTAssertEqual(prepared.breadcrumbs?.map(\.category), [LaunchSteps.category, "log"])
    XCTAssertEqual(prepared.breadcrumbs?.last?.message, "engine open")
  }

  func testOtherBreadcrumbsNeverGoAlong() throws {
    let event = fatalEvent(at: startedAt.addingTimeInterval(-60), steps: [])
    let foreign = Breadcrumb(level: .info, category: "ui.click")
    foreign.message = "Alice Example"
    event.breadcrumbs = [foreign]
    let prepared = try XCTUnwrap(prepare(event))
    XCTAssertEqual(prepared.breadcrumbs?.count, 0)
  }

  private func prepare(_ event: Event) -> Event? {
    CrashEventScrub.prepare(
      event, installationID: "0123", sampleRate: 0, roll: 0.5, startedAt: startedAt, logLines: { ["engine open"] })
  }

  private func fatalEvent(at timestamp: Date, steps: [String]) -> Event {
    let event = Event(level: .fatal)
    event.timestamp = timestamp
    event.breadcrumbs = steps.map { step in
      let breadcrumb = Breadcrumb(level: .info, category: LaunchSteps.category)
      breadcrumb.message = step
      return breadcrumb
    }
    return event
  }
}
