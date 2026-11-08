import XCTest

@testable import TeamCraft

final class ComposerReplyTests: XCTestCase {
  /// The core reads the target back as a quote after the app was killed (#1115).
  func testAStoredTargetComesBackWithItsPicture() {
    let quote = Wire.decode(
      ReplyPreview.self,
      from: #"{"maybe_message":"9","author":"5","excerpt":"ignored","attachment":{"file_id":"4","file_name":"c.jpg"}}"#)
    let reply = quote.flatMap(ComposerReply.init(stored:))

    XCTAssertEqual(reply?.messageId, "9")
    XCTAssertEqual(reply?.maybeAttachmentId, "4")
    XCTAssertEqual(reply?.excerpt, "c.jpg")
    XCTAssertEqual(reply?.authorName(from: ["5": "Dana"]), "Dana")
  }

  func testAStoredTargetWithoutItsMessageIsDropped() {
    let quote = Wire.decode(ReplyPreview.self, from: #"{"author":"5","excerpt":"gone"}"#)
    XCTAssertNil(quote.flatMap(ComposerReply.init(stored:)))
  }

  func testChoosingAndClearingAreSentAsTheCoreReadsThem() throws {
    let quote = try XCTUnwrap(Wire.decode(ReplyPreview.self, from: #"{"maybe_message":"9","author":"5","excerpt":"hi"}"#))
    let chosen = try object(.setDraftReply(conversationId: "1", reply: ComposerReply(stored: quote)))
    XCTAssertEqual(chosen?["action"] as? String, "set_draft_reply")
    let reply = chosen?["maybe_reply"] as? [String: Any]
    XCTAssertEqual(reply?["message"] as? String, "9")

    let cleared = try object(.setDraftReply(conversationId: "1", reply: nil))
    XCTAssertTrue(cleared?["maybe_reply"] is NSNull)
  }

  private func object(_ action: ChatAction) throws -> [String: Any]? {
    try JSONSerialization.jsonObject(with: Data(action.json.utf8)) as? [String: Any]
  }
}
