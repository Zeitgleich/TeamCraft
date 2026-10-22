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

final class NotificationAuthenticatorTests: XCTestCase {
  private let key = Data(repeating: 7, count: 32)

  func testATagVerifiesOnlyForItsConversationAndKey() {
    let tag = NotificationAuthenticator.tag(for: "42", key: key)
    XCTAssertTrue(NotificationAuthenticator.isValid(tag: tag, for: "42", key: key))
    XCTAssertFalse(NotificationAuthenticator.isValid(tag: tag, for: "43", key: key))
    XCTAssertFalse(NotificationAuthenticator.isValid(tag: tag, for: "42", key: Data(repeating: 8, count: 32)))
  }

  func testMalformedTagsAreRejected() {
    XCTAssertFalse(NotificationAuthenticator.isValid(tag: "", for: "42", key: key))
    XCTAssertFalse(NotificationAuthenticator.isValid(tag: "zz", for: "42", key: key))
    XCTAssertFalse(NotificationAuthenticator.isValid(tag: "abc", for: "42", key: key))
  }
}
