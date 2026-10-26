import XCTest

@testable import TeamCraft

/// The image viewer (#240): what the app adds around the core's shared paging, albums and limits.
final class PictureViewerTests: XCTestCase {
  func testAlbumsAreLaidOutByTheCore() throws {
    XCTAssertNil(albumLayout(pictures: 1))
    let three = try XCTUnwrap(albumLayout(pictures: 3))
    XCTAssertEqual(three.tiles, 3)
    XCTAssertTrue(three.isFirstTall)
    let many = try XCTUnwrap(albumLayout(pictures: 9))
    XCTAssertEqual(many.tiles, 4)
    XCTAssertEqual(many.hidden, 5)
  }

  func testTheSettingDecidesByTheNetwork() {
    XCTAssertEqual(OriginalDownloadSetting(rawValue: "unmetered_only"), .unmeteredOnly)
    XCTAssertTrue(fetchesOriginalAutomatically(setting: OriginalDownloadSetting.always.setting, isMetered: true))
    XCTAssertTrue(fetchesOriginalAutomatically(setting: OriginalDownloadSetting.unmeteredOnly.setting, isMetered: false))
    XCTAssertFalse(fetchesOriginalAutomatically(setting: OriginalDownloadSetting.unmeteredOnly.setting, isMetered: true))
    XCTAssertFalse(fetchesOriginalAutomatically(setting: OriginalDownloadSetting.never.setting, isMetered: false))
  }

  func testTheViewerReadsTheOpenedMessageFirstAndShowsItsAlbum() throws {
    let viewer = try MediaViewer(conversationId: "1", messageId: "2", fileId: "22")
    XCTAssertFalse(viewer.state().isLoaded)
    let query = try XCTUnwrap(viewer.nextQuery())
    XCTAssertTrue(query.contains("\"kind\":\"message_extras\""))
    let answer = """
      {"answer":"message_extras","extras":[{"message":"2","files":[
      \(Self.file(id: "21")),\(Self.file(id: "22")),\(Self.file(id: "23"))]}]}
      """
    XCTAssertTrue(try viewer.addAnswer(queryJson: query, answerJson: answer))
    let state = viewer.state()
    XCTAssertTrue(state.isLoaded)
    XCTAssertEqual(state.items.map(\.fileId), ["21", "22", "23"])
    XCTAssertEqual(state.index, 1)
    XCTAssertEqual(state.scope, .album)
    XCTAssertTrue(state.canSwitchScope)
    let json = try XCTUnwrap(viewer.fileJson(messageId: "2", fileId: "22"))
    let file = try XCTUnwrap(Wire.decode(FileInfo.self, from: json))
    XCTAssertEqual(file.maybeSender, "5")
    XCTAssertEqual(file.maybeSentAt, 1_700_000_000_000)
    XCTAssertTrue(file.isViewablePicture)
  }

  func testOneFileOfAMessageIsDownloaded() throws {
    let action = ChatAction.downloadAttachment(conversationId: "1", messageId: "2", fileId: "22")
    let object = try XCTUnwrap(try JSONSerialization.jsonObject(with: Data(action.json.utf8)) as? [String: String])
    XCTAssertEqual(object["action"], "download_attachment")
    XCTAssertEqual(object["file_id"], "22")
  }

  func testOriginalsAreDecodedAtTwiceTheScreenAtMost4096Pixels() {
    XCTAssertEqual(PictureDecoder.maxPixels(screen: CGSize(width: 390, height: 844), scale: 3), 4096)
    XCTAssertEqual(PictureDecoder.maxPixels(screen: CGSize(width: 320, height: 480), scale: 2), 1920)
  }

  private static func file(id: String) -> String {
    """
    {"conversation":"1","message":"2","file_id":"\(id)","file_name":"\(id).png","media_type":"image/png",\
    "size_bytes":10,"maybe_thumbnail_png":[1],"transfer":{"state":"remote"},"maybe_sender":"5",\
    "maybe_sent_at":1700000000000}
    """
  }
}
