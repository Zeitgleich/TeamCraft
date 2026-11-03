import XCTest

@testable import TeamCraft

final class LinkRequestsTests: XCTestCase {
  private let now = Date(timeIntervalSince1970: 1_000_000)

  func testPromptExpiresWhenItsTimeRunsOut() {
    let prompt = LinkPrompt(request("a", secondsLeft: 90), readAt: now)
    XCTAssertFalse(prompt.isExpired(at: now.addingTimeInterval(89)))
    XCTAssertTrue(prompt.isExpired(at: now.addingTimeInterval(90)))
  }

  func testMergeKeepsTheDeadlineOfAPromptAlreadyShown() {
    let shown = LinkPrompt(request("a", secondsLeft: 60), readAt: now)
    let later = now.addingTimeInterval(30)
    let merged = LinkPrompts.merge(
      current: [shown], requests: [request("a", secondsLeft: 60)], readAt: later, hiding: nil)
    XCTAssertEqual(merged, [shown])
  }

  func testMergeDropsRequestsThatLeftTheListAndTheReviewedOne() {
    let current = [LinkPrompt(request("a", secondsLeft: 60), readAt: now)]
    let merged = LinkPrompts.merge(
      current: current, requests: [request("b", secondsLeft: 60), request("c", secondsLeft: 60)], readAt: now,
      hiding: "c")
    XCTAssertEqual(merged.map(\.id), ["b"])
  }

  func testMergeSkipsRequestsWithoutTimeLeft() {
    let merged = LinkPrompts.merge(
      current: [], requests: [request("a", secondsLeft: 0)], readAt: now, hiding: nil)
    XCTAssertTrue(merged.isEmpty)
  }

  func testNextExpiryIsTheEarliestDeadline() {
    let prompts = [
      LinkPrompt(request("a", secondsLeft: 120), readAt: now),
      LinkPrompt(request("b", secondsLeft: 30), readAt: now),
    ]
    XCTAssertEqual(LinkPrompts.nextExpiry(of: prompts), now.addingTimeInterval(30))
    XCTAssertNil(LinkPrompts.nextExpiry(of: []))
  }

  func testTypedCodeKeepsOnlyDigits() {
    XCTAssertEqual(LinkCode.digits(of: " 123 456\n"), "123456")
    XCTAssertEqual(LinkCode.digits(of: "12-34"), "1234")
    XCTAssertEqual(LinkCode.digits(of: "abc"), "")
  }

  func testLinkErrorsAreToldApart() {
    // The core's errors are classified by their name, as the other failures are.
    struct LinkCodeRejected: Error {}
    struct LinkAttemptEnded: Error {}
    XCTAssertEqual(CoreFailure(LinkCodeRejected()), .linkCodeRejected)
    XCTAssertEqual(CoreFailure(LinkAttemptEnded()), .linkAttemptEnded)
  }

  func testCodesForTheOtherSideAreToldApart() {
    // Issue #998: a computer's code for a new phone, and a new device's code, each have their side.
    struct LinkCodeInvalid: Error {}
    struct LinkCodeForNewDevice: Error {}
    struct LinkCodeForSignedInDevice: Error {}
    struct LinkCodeExpired: Error {}
    XCTAssertEqual(CoreFailure(LinkCodeInvalid()), .linkCodeInvalid)
    XCTAssertEqual(CoreFailure(LinkCodeForNewDevice()), .linkCodeForNewDevice)
    XCTAssertEqual(CoreFailure(LinkCodeForSignedInDevice()), .linkCodeForSignedInDevice)
    XCTAssertEqual(CoreFailure(LinkCodeExpired()), .linkCodeExpired)
    XCTAssertEqual(AppFailure(.linkCodeForNewDevice).message, "link_code_for_new_device")
  }

  private func request(_ id: String, secondsLeft: UInt64) -> LinkRequestItem {
    LinkRequestItem(id: id, deviceName: "Device", platform: .ios, secondsLeft: secondsLeft)
  }
}
