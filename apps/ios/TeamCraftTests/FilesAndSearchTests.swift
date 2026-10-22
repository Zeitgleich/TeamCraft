import XCTest

@testable import TeamCraft

final class FilesAndSearchTests: XCTestCase {
  func testExtrasDecodeFilesAndLinkPreview() throws {
    let json = """
      {"answer":"extras","extras":[{"message":"7","files":[{"conversation":"1","message":"7","file_id":"9",
      "file_name":"plan.pdf","media_type":"application/pdf","size_bytes":2048,"maybe_thumbnail_png":[1,2,3],
      "transfer":{"state":"uploading","done_bytes":512,"total_bytes":2048}}],
      "maybe_link_preview":{"url":"https://example.com/a","maybe_title":"Title","maybe_image":[4,5]},
      "is_pinned":false}]}
      """
    let answer = try XCTUnwrap(Wire.decode(ChatAnswer.self, from: json))
    let extras = try XCTUnwrap(answer.extras?.first)
    let file = try XCTUnwrap(extras.files?.first)
    XCTAssertEqual(file.fileName, "plan.pdf")
    XCTAssertEqual(file.maybeThumbnailData, Data([1, 2, 3]))
    XCTAssertEqual(file.transfer.maybeProgress, 0.25)
    XCTAssertTrue(file.transfer.isInProgress)
    XCTAssertEqual(extras.maybeLinkPreview?.maybeTitle, "Title")
    XCTAssertEqual(extras.maybeLinkPreview?.maybeOpenableURL?.absoluteString, "https://example.com/a")
  }

  func testOnlyWebLinksOpenFromAPreview() {
    let preview = LinkPreview(
      url: "javascript:alert(1)", maybeTitle: nil, maybeDescription: nil, maybeSiteName: nil, maybeImage: nil)
    XCTAssertNil(preview.maybeOpenableURL)
    let file = LinkPreview(
      url: "file:///etc/passwd", maybeTitle: nil, maybeDescription: nil, maybeSiteName: nil, maybeImage: nil)
    XCTAssertNil(file.maybeOpenableURL)
  }

  func testFinishedTransfersHaveNoProgress() throws {
    let json = #"{"state":"local","path":"/tmp/x"}"#
    let transfer = try XCTUnwrap(Wire.decode(FileTransfer.self, from: json))
    XCTAssertNil(transfer.maybeProgress)
    XCTAssertFalse(transfer.isInProgress)
    XCTAssertEqual(transfer.path, "/tmp/x")
  }

  func testSendRequestSerializesToTheCoreSchema() throws {
    let request = SendRequest(
      conversationId: "10", maybeThreadId: nil, body: "see attached",
      files: [PickedFile(fileName: "a.png", mediaType: "image/png", path: "/data/uploads/a.png")])
    let object = try XCTUnwrap(
      JSONSerialization.jsonObject(with: Data(ChatAction.sendMessage(request).json.utf8)) as? [String: Any])
    XCTAssertEqual(object["action"] as? String, "send_message")
    let message = try XCTUnwrap(object["message"] as? [String: Any])
    XCTAssertEqual(message["conversation"] as? String, "10")
    XCTAssertTrue(message["maybe_thread"] is NSNull)
    let files = try XCTUnwrap(message["files"] as? [[String: Any]])
    XCTAssertEqual(files.first?["file_name"] as? String, "a.png")
    let source = try XCTUnwrap(files.first?["source"] as? [String: String])
    XCTAssertEqual(source, ["kind": "path", "path": "/data/uploads/a.png"])
  }

  func testQueriesSerializeToTheCoreSchema() throws {
    let query = ChatQuery.messageExtras(conversationId: "1", messageIds: ["2", "3"])
    let object = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(query.json.utf8)) as? [String: Any])
    XCTAssertEqual(object["kind"] as? String, "message_extras")
    XCTAssertEqual(object["messages"] as? [String], ["2", "3"])
    XCTAssertEqual(ChatQuery.maxUploadBytes.json, #"{"kind":"max_upload_bytes"}"#)
  }

  func testFileNamesLoseTheirPaths() {
    XCTAssertEqual(AttachmentImporter.safeName("../../etc/passwd"), "passwd")
    XCTAssertEqual(AttachmentImporter.safeName("IMG 1.jpg"), "IMG 1.jpg")
    XCTAssertEqual(AttachmentImporter.safeName(".."), "file")
    XCTAssertEqual(AttachmentImporter.safeName(""), "file")
  }

  func testImporterRefusesFilesOverTheServerLimit() throws {
    let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    let source = FileManager.default.temporaryDirectory.appendingPathComponent("\(UUID().uuidString).txt")
    try Data(repeating: 1, count: 100).write(to: source)
    defer {
      try? FileManager.default.removeItem(at: directory)
      try? FileManager.default.removeItem(at: source)
    }
    let tooSmallLimit = AttachmentImporter(directory: directory, maybeLimitBytes: 10)
    XCTAssertThrowsError(try tooSmallLimit.importPickedMedia(PickedMedia(url: source))) { error in
      XCTAssertEqual(error as? AttachmentError, .tooLarge(limitBytes: 10))
    }
    let importer = AttachmentImporter(directory: directory, maybeLimitBytes: 1000)
    let picked = try importer.importPickedMedia(PickedMedia(url: source))
    XCTAssertEqual(picked.mediaType, "text/plain")
    XCTAssertTrue(FileManager.default.fileExists(atPath: picked.path))
  }

  func testSearchFiltersBecomeQuerySyntax() throws {
    var filters = SearchFilters()
    XCTAssertTrue(filters.isEmpty)
    XCTAssertEqual(filters.query(for: "  budget "), "budget")
    filters.from = "@alex"
    filters.channel = "#general"
    filters.hasFile = true
    filters.hasLink = true
    var calendar = Calendar(identifier: .gregorian)
    calendar.timeZone = TimeZone(identifier: "UTC") ?? .gmt
    filters.after = calendar.date(from: DateComponents(year: 2026, month: 3, day: 5, hour: 12))
    XCTAssertEqual(
      filters.query(for: "budget"), "budget from:alex in:general after:\(SearchFilters.day(filters.after ?? Date())) has:file has:link")
    XCTAssertEqual(filters.activeCount, 5)
    XCTAssertEqual(SearchFilters.day(try XCTUnwrap(filters.after), calendar: calendar), "2026-03-05")
  }

  func testFiltersAloneMakeAQuery() {
    var filters = SearchFilters()
    filters.hasFile = true
    XCTAssertEqual(filters.query(for: ""), "has:file")
  }

  func testNamesWithSpacesStayOneToken() {
    XCTAssertEqual(SearchFilters.word(" Alex  Example "), "AlexExample")
  }

  func testMeetingsDecodeAndTellTheOwnAnswer() throws {
    let json = """
      {"meetings":[{"id":"5","title":"Planning","starts_at":1800000000000,"ends_at":1800003600000,
      "kind":"scheduled","status":"scheduled","has_lobby":true,"organizer_user_id":"1","conversation_id":"77",
      "join_url":"https://chat.example.com/m/5","attendees":[{"user_id":"2","rsvp":"tentative"},
      {"email":"guest@example.com","rsvp":"needs_action"}]}]}
      """
    let list = try XCTUnwrap(Wire.decode(MeetingList.self, from: json))
    let meeting = try XCTUnwrap(list.meetings.first)
    XCTAssertEqual(meeting.conversationId, "77")
    XCTAssertEqual(meeting.rsvp(of: "2"), .tentative)
    XCTAssertNil(meeting.rsvp(of: "9"))
    XCTAssertEqual(meeting.endDate.timeIntervalSince(meeting.startDate), 3600)
  }

  func testMentionsRankAboveOtherNotifications() {
    let urgent = NotificationContentBuilder.relevance(isMention: false, isUrgent: true)
    let mention = NotificationContentBuilder.relevance(isMention: true, isUrgent: false)
    let plain = NotificationContentBuilder.relevance(isMention: false, isUrgent: false)
    XCTAssertGreaterThan(urgent, mention)
    XCTAssertGreaterThan(mention, plain)
  }
}
