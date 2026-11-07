import XCTest

@testable import TeamCraft

final class DraftsAndSentTests: XCTestCase {
  func testADraftOfAThreadIsIdentifiedByItsPlaceAndDatedByItsLastEdit() {
    let draft = Wire.decode(
      DraftItem.self,
      from: #"{"conversation":"7","maybe_thread":"9","conversation_name":"general","maybe_thread_root":"The plan","body":"half a thought","edited_at":2000}"#
    )
    XCTAssertEqual(draft?.id, "7/9")
    XCTAssertEqual(draft?.maybeThreadRoot, "The plan")
    XCTAssertEqual(draft?.editedDate, Date(timeIntervalSince1970: 2))
  }

  func testDraftsAndSentInTheNotesAreNamedLikeTheNotes() {
    let draft = Wire.decode(
      DraftItem.self,
      from: #"{"conversation":"7","conversation_name":"","is_notes_to_self":true,"body":"x","edited_at":1}"#)
    XCTAssertEqual(draft?.listName, String(localized: "notes_title"))
    let sent = Wire.decode(
      SentItem.self,
      from: #"{"conversation":"7","conversation_name":"","is_notes_to_self":true,"message":"1","excerpt":"","sent_at":1}"#)
    XCTAssertEqual(sent?.listName, String(localized: "notes_title"))
    let plain = Wire.decode(
      SentItem.self,
      from: #"{"conversation":"7","conversation_name":"general","message":"1","excerpt":"","sent_at":1}"#)
    XCTAssertEqual(plain?.listName, "general")
  }

  func testAConversationsOwnDraftHasNoThread() {
    let draft = Wire.decode(
      DraftItem.self, from: #"{"conversation":"7","conversation_name":"general","body":"x","edited_at":1}"#)
    XCTAssertEqual(draft?.id, "7/")
    XCTAssertNil(draft?.maybeThread)
  }

  func testASentMessageOfOnlyFilesDecodes() {
    let sent = Wire.decode(
      SentItem.self,
      from: #"{"conversation":"7","conversation_name":"general","message":"11","excerpt":"","file_count":2,"sent_at":1000}"#
    )
    XCTAssertEqual(sent?.id, "11")
    XCTAssertEqual(sent?.fileCount, 2)
    XCTAssertEqual(sent?.sentDate, Date(timeIntervalSince1970: 1))
  }

  func testAPlainSentMessageHasNoFileCount() {
    let sent = Wire.decode(
      SentItem.self,
      from: #"{"conversation":"7","conversation_name":"general","message":"11","excerpt":"hi","sent_at":1000}"#)
    XCTAssertNil(sent?.fileCount)
  }

  func testAnAnswerCarriesTheLists() {
    let answer = Wire.decode(ChatAnswer.self, from: #"{"answer":"drafts","drafts":[]}"#)
    XCTAssertEqual(answer?.drafts, [])
    XCTAssertNil(answer?.sent)
  }
}
