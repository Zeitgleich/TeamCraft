import XCTest

@testable import TeamCraft

final class DetectedItemsTests: XCTestCase {
  func testFindsPhoneNumbersEmailAndAddresses() {
    let text = "Call +49 170 1234567 or write to anna@example.com. Visit Hauptstrasse 12, 10115 Berlin."
    let found = findDetected(text)
    XCTAssertEqual(found.map(\.kind), [.phone, .email, .address])
    XCTAssertEqual(found[0].url?.absoluteString, "tel:+491701234567")
    XCTAssertEqual(found[1].url?.absoluteString, "mailto:anna@example.com")
    XCTAssertEqual(found[2].url?.host, "maps.apple.com")
  }

  func testDatesTimesAndShortNumbersAreNotPhoneNumbers() {
    XCTAssertTrue(findDetected("On 2026-11-15 at 10:30, ticket 123456, 15.11.2026").isEmpty)
  }

  func testNothingIsFoundInsideCode() {
    XCTAssertTrue(findDetected("run `call 0170 1234567` or\n```\nmail a@example.com\n```").isEmpty)
  }

  func testAnEmailInsideALinkStaysPartOfTheLink() {
    XCTAssertEqual(findActionable("see https://example.com/?to=a@example.com now").map(\.kind), [.web])
  }

  func testALinkLosesTheSentencePunctuationAfterIt() {
    let found = findActionable("go to https://example.com/page.")
    XCTAssertEqual(found.first?.value, "https://example.com/page")
  }

  func testTheMenuListsEachValueOnceAndAtMostThree() {
    let text = "a@example.com a@example.com b@example.com c@example.com d@example.com"
    XCTAssertEqual(uniqueDetected(findActionable(text)).map(\.value), ["a@example.com", "b@example.com", "c@example.com"])
  }

  func testAQuoteIsTheSelectionOrTheWholeText() {
    XCTAssertEqual(quotedPart(of: "hello big world", range: NSRange(location: 6, length: 3)), "big")
    XCTAssertEqual(quotedPart(of: "hello big world", range: NSRange(location: 5, length: 0)), "hello big world")
    XCTAssertEqual(quotedPart(of: "hello big world", range: NSRange(location: 5, length: 1)), "hello big world")
  }
}
