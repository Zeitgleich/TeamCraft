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

  private func decode(mediaType: String, state: String, extra: String = "") throws -> FileInfo {
    let json = """
      {"conversation":"1","message":"7","file_id":"9","file_name":"clip","media_type":"\(mediaType)",
      "size_bytes":2048,"transfer":{"state":"\(state)"}\(extra)}
      """
    return try XCTUnwrap(Wire.decode(FileInfo.self, from: json))
  }
}
