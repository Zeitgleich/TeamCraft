import XCTest

@testable import TeamCraft

final class LegalHoldTests: XCTestCase {
  func testAHoldIsFoundByItsConversation() throws {
    let json = """
      {"security":{"my_fingerprint":"ab","legal_holds":[
        {"conversation_id":"5","reference":"Case 2026-14","placed_at":1700000000000},
        {"conversation_id":"9","reference":"Matter B","placed_at":1700000100000}]}}
      """
    let snapshot = try XCTUnwrap(Wire.decode(LegalHoldSnapshot.self, from: json))
    XCTAssertEqual(snapshot.hold(on: "5")?.reference, "Case 2026-14")
    XCTAssertEqual(snapshot.hold(on: "9")?.placedAt, 1_700_000_100_000)
    XCTAssertNil(snapshot.hold(on: "6"))
  }

  func testNoHoldsBeforeAPolicyWasVerified() throws {
    let bare = try XCTUnwrap(Wire.decode(LegalHoldSnapshot.self, from: #"{"security":{"my_fingerprint":"ab"}}"#))
    XCTAssertNil(bare.hold(on: "5"))
    let none = try XCTUnwrap(Wire.decode(LegalHoldSnapshot.self, from: #"{}"#))
    XCTAssertNil(none.hold(on: "5"))
  }

  func testARefusedDeletionUnderLegalHoldSaysSo() {
    // The core's errors are classified by their name, as the other failures are.
    struct LegalHold: Error {}
    XCTAssertEqual(CoreFailure(LegalHold()), .legalHold)
    XCTAssertEqual(AppFailure(.legalHold).message, "error_legal_hold")
  }
}
