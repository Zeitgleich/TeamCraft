import XCTest

@testable import TeamCraft

final class VideoViewerTests: XCTestCase {
  func testLengthsReadLikeAClock() {
    XCTAssertEqual(VideoDurationText.format(milliseconds: 7_900), "0:07")
    XCTAssertEqual(VideoDurationText.format(milliseconds: 754_000), "12:34")
    XCTAssertEqual(VideoDurationText.format(milliseconds: 3_723_000), "1:02:03")
    XCTAssertEqual(VideoDurationText.format(milliseconds: -5), "0:00")
  }

  func testAVideoDecodesItsLengthAndOpensInThePlayer() throws {
    let file = try decode(mediaType: "video/webm", state: "remote", extra: #","maybe_duration_ms":61000"#)
    XCTAssertTrue(file.isVideo)
    XCTAssertEqual(file.maybeDurationMs, 61000)
    XCTAssertTrue(file.opensInVideoPlayer)
  }

  func testAVideoStillGoingUpOrAnotherFileDoesNotOpenInThePlayer() throws {
    for state in ["uploading", "upload_stalled", "upload_failed"] {
      XCTAssertFalse(try decode(mediaType: "video/mp4", state: state).opensInVideoPlayer, state)
    }
    XCTAssertFalse(try decode(mediaType: "application/pdf", state: "local").opensInVideoPlayer)
  }

  func testAPlaylistIsRecognisedByNameTypeOrContent() {
    let movie = Data([0, 0, 0, 24]) + Data("ftypmp42".utf8)
    let playlist = Data("#EXTM3U\nhttps://example.com/a.ts\n".utf8)
    let check = VideoFileCheck.isPlaylist(fileExtension:mediaType:head:)
    XCTAssertFalse(check("mp4", "video/mp4", movie))
    XCTAssertTrue(check("m3u8", "video/mp4", movie))
    XCTAssertTrue(check("M3U", "video/mp4", movie))
    XCTAssertTrue(check("mp4", "video/x-mpegURL", movie))
    XCTAssertTrue(check("mp4", "video/mp4", playlist))
    XCTAssertTrue(check("", "video/mp4", playlist))
    XCTAssertTrue(check("mp4", "video/mp4", Data([0xEF, 0xBB, 0xBF]) + playlist), "after a byte order mark")
    XCTAssertTrue(check("mp4", "video/mp4", Data("\r\n ".utf8) + playlist), "after blank space")
    XCTAssertFalse(check("mp4", "video/mp4", Data()))
  }

  func testAFileThatCanNotBeReadCountsAsAPlaylist() {
    let missing = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString + ".mp4")
    XCTAssertTrue(VideoFileCheck.isPlaylist(at: missing, mediaType: "video/mp4"))
  }

  func testQuickLookGetsNoPlaylistWhateverItsType() throws {
    let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: folder) }
    let named = folder.appendingPathComponent("list.m3u8")
    let hidden = folder.appendingPathComponent("list.bin")
    let plain = folder.appendingPathComponent("plan.pdf")
    try Data("%PDF-1.7".utf8).write(to: named)
    try Data("#EXTM3U\n".utf8).write(to: hidden)
    try Data("%PDF-1.7".utf8).write(to: plain)
    let type = "application/octet-stream"
    XCTAssertNil(VideoFileCheck.previewURL(path: named.path, mediaType: type))
    XCTAssertNil(VideoFileCheck.previewURL(path: hidden.path, mediaType: type))
    XCTAssertEqual(VideoFileCheck.previewURL(path: plain.path, mediaType: "application/pdf")?.lastPathComponent, "plan.pdf")
  }

  private func decode(mediaType: String, state: String, extra: String = "") throws -> FileInfo {
    let json = """
      {"conversation":"1","message":"7","file_id":"9","file_name":"clip","media_type":"\(mediaType)",
      "size_bytes":2048,"transfer":{"state":"\(state)"}\(extra)}
      """
    return try XCTUnwrap(Wire.decode(FileInfo.self, from: json))
  }
}
