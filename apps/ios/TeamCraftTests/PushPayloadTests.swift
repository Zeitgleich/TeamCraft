import XCTest

@testable import TeamCraft

final class PushPayloadTests: XCTestCase {
  func testDecodesBase64URLWithoutPadding() {
    // "hello?>" encodes to aGVsbG8_Pg in base64url.
    XCTAssertEqual(PushPayload.decodeBase64URL("aGVsbG8_Pg"), Data("hello?>".utf8))
  }

  func testReadsTheSealedBytesFromTheCustomKey() {
    let userInfo: [AnyHashable: Any] = ["aps": ["alert": "x"], "w": "AAEC"]
    XCTAssertEqual(PushPayload.sealedBytes(from: userInfo), Data([0, 1, 2]))
  }

  func testRejectsMissingEmptyAndMalformedPayloads() {
    XCTAssertNil(PushPayload.sealedBytes(from: [:]))
    XCTAssertNil(PushPayload.sealedBytes(from: ["w": ""]))
    XCTAssertNil(PushPayload.sealedBytes(from: ["w": "***"]))
    XCTAssertNil(PushPayload.sealedBytes(from: ["w": 5]))
  }

  func testDeviceTokenIsLowercaseHex() {
    XCTAssertEqual(Data([0x0A, 0xFF, 0x10]).hexString, "0aff10")
  }
}
