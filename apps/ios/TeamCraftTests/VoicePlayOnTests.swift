import XCTest

@testable import TeamCraft

final class VoicePlayOnTests: XCTestCase {
  func testPlaysOnToTheNextUnplayedMessage() {
    let order = ["a", "b", "c", "d"]
    XCTAssertEqual(nextUnplayedVoice(order: order, endedKey: "a", played: []), "b")
    XCTAssertEqual(nextUnplayedVoice(order: order, endedKey: "a", played: ["b"]), "c")
    XCTAssertNil(nextUnplayedVoice(order: order, endedKey: "d", played: []))
    XCTAssertNil(nextUnplayedVoice(order: order, endedKey: "x", played: []))
  }
}
