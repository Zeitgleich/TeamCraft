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

  func testADraftCountsItsFiles() {
    let draft = Wire.decode(
      DraftItem.self, from: #"{"conversation":"7","conversation_name":"general","body":"","edited_at":1,"file_count":2}"#)
    XCTAssertEqual(draft?.fileCount, 2)
  }

  func testTheDraftsFilesDecodeWithTheirState() {
    let answer = Wire.decode(
      DraftFilesAnswer.self,
      from: #"""
        {"answer":"draft_files","files":[
          {"id":"a","file_name":"a.png","media_type":"image/png","size_bytes":3,"thumbnail":[1,255],"state":{"state":"ready"}},
          {"id":"b","file_name":"b.pdf","media_type":"application/pdf","size_bytes":9,"state":{"state":"failed"}},
          {"id":"c","file_name":"c.pdf","media_type":"application/pdf","size_bytes":9,"state":{"state":"expired"}},
          {"id":"d","file_name":"d.pdf","media_type":"application/pdf","size_bytes":9,"state":{"state":"on_this_device"}},
          {"id":"e","file_name":"e.pdf","media_type":"application/pdf","size_bytes":9,"state":{"state":"from_the_future"}}
        ]}
        """#)
    let files = answer?.files ?? []
    XCTAssertEqual(files.map(\.fileState), [.ready, .failed, .expired, .onThisDevice, .unknown])
    XCTAssertEqual(files.map(\.isSendable), [true, false, false, true, false])
    XCTAssertEqual(files.first?.thumbnailData, Data([1, 255]))
    XCTAssertEqual(files.map(\.fileState.maybeStatusKey), [
      nil, "composer_draft_file_failed", "composer_draft_file_expired", nil, "composer_draft_file_failed",
    ])
  }

  func testAttachingNamesEachFileWithItsDraftId() throws {
    var file = PickedFile(fileName: "a.png", mediaType: "image/png", path: "/uploads/a.png")
    file.maybeDraftId = "0123456789abcdef0123456789abcdef"
    let json = try object(ChatAction.attachDraftFiles(conversationId: "7", maybeThreadId: "9", files: [file]).json)
    XCTAssertEqual(json["action"] as? String, "attach_draft_files")
    XCTAssertEqual(json["maybe_thread"] as? String, "9")
    XCTAssertEqual(json["travel"] as? String, "on_send")
    let sent = try XCTUnwrap((json["files"] as? [[String: Any]])?.first)
    XCTAssertEqual(sent["draft_id"] as? String, "0123456789abcdef0123456789abcdef")
    XCTAssertEqual((sent["source"] as? [String: Any])?["path"] as? String, "/uploads/a.png")
  }

  func testASendTakesTheDraftsFilesById() throws {
    var picked = PickedFile(fileName: "a.png", mediaType: "image/png", path: "/uploads/a.png")
    picked.maybeDraftId = "0123456789abcdef0123456789abcdef"
    let fromDraft = try XCTUnwrap(
      Wire.decode(
        DraftFileItem.self,
        from: #"{"id":"fedcba9876543210fedcba9876543210","file_name":"b.pdf","media_type":"application/pdf","state":{"state":"ready"}}"#))
    let request = SendRequest(conversationId: "7", body: "", files: [picked], draftFiles: [fromDraft])
    let json = try object(ChatAction.sendMessage(request).json)
    let files = try XCTUnwrap((json["message"] as? [String: Any])?["files"] as? [[String: Any]])
    XCTAssertEqual(files.count, 2)
    XCTAssertEqual(files[0]["draft_id"] as? String, "0123456789abcdef0123456789abcdef")
    let source = try XCTUnwrap(files[1]["source"] as? [String: Any])
    XCTAssertEqual(source["kind"] as? String, "draft")
    XCTAssertEqual(source["id"] as? String, "fedcba9876543210fedcba9876543210")
    XCTAssertNil(files[1]["draft_id"])
  }

  func testRemovingAndDiscardingNameThePlace() throws {
    let remove = try object(ChatAction.removeDraftFile(conversationId: "7", maybeThreadId: nil, id: "a").json)
    XCTAssertEqual(remove["action"] as? String, "remove_draft_file")
    XCTAssertTrue(remove["maybe_thread"] is NSNull)
    XCTAssertEqual(remove["id"] as? String, "a")
    let discard = try object(ChatAction.discardDraft(conversationId: "7", maybeThreadId: "9").json)
    XCTAssertEqual(discard["action"] as? String, "discard_draft")
    let query = try object(ChatQuery.draftFiles(conversationId: "7", maybeThreadId: nil).json)
    XCTAssertEqual(query["kind"] as? String, "draft_files")
  }

  func testAFreshDraftIdIsAClientMessageId() {
    let id = PickedFile.newDraftId()
    XCTAssertEqual(id.count, 32)
    XCTAssertTrue(id.allSatisfy { $0.isHexDigit && !$0.isUppercase })
  }

  func testTheComposerShowsOnlyTheDraftsFilesItDidntPickNorLetGo() throws {
    var mine = PickedFile(fileName: "a.png", mediaType: "image/png", path: "/uploads/a.png")
    mine.maybeDraftId = "mine"
    let items = try ["mine", "other", "removed"].map { id in
      try XCTUnwrap(
        Wire.decode(
          DraftFileItem.self,
          from: #"{"id":"\#(id)","file_name":"f","media_type":"application/pdf","state":{"state":"ready"}}"#))
    }
    XCTAssertEqual(otherDraftFiles(items, local: [mine], gone: ["removed"]).map(\.id), ["other"])
  }

  private func object(_ json: String) throws -> [String: Any] {
    try XCTUnwrap(JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any])
  }
}
