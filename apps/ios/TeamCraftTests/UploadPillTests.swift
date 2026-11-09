import XCTest

@testable import TeamCraft

private let mb: Int64 = 1_000_000

final class UploadPillTests: XCTestCase {
  func testNothingGoingUpHasNoControl() throws {
    XCTAssertNil(UploadPill.of([]))
    XCTAssertNil(UploadPill.of([try file(#"{"state":"remote"}"#, size: mb)]))
    XCTAssertNil(UploadPill.of([try file(#"{"state":"upload_failed","reason":"too_large"}"#, size: mb)]))
  }

  func testBeforeTheFirstByteItIsProcessing() throws {
    let files = [try file(uploading(0, 5 * mb), size: 5 * mb), try file(uploading(0, 3 * mb), size: 3 * mb)]
    XCTAssertEqual(UploadPill.of(files), .processing)
    XCTAssertNil(UploadPill.processing.maybeFraction)
  }

  func testAFailedAttemptBeforeTheFirstByteIsRetrying() throws {
    XCTAssertEqual(UploadPill.of([try file(uploading(0, 5 * mb, failedAttempts: 2), size: 5 * mb)]), .retrying)
  }

  func testBytesAddUpOverAllFiles() throws {
    let pill = UploadPill.of([
      try file(uploading(2 * mb, 4 * mb), size: 4 * mb), try file(uploading(0, 6 * mb), size: 6 * mb),
    ])
    XCTAssertEqual(pill, .sending(doneBytes: 2 * mb, totalBytes: 10 * mb))
    XCTAssertEqual(pill?.maybeFraction, 0.2)
  }

  func testFinishedFilesCountWholeSoTheRingNeverGoesBack() throws {
    let files = [try file(#"{"state":"remote"}"#, size: 4 * mb), try file(uploading(0, 6 * mb), size: 6 * mb)]
    XCTAssertEqual(UploadPill.of(files), .sending(doneBytes: 4 * mb, totalBytes: 10 * mb))
  }

  func testFilesThatFailedForGoodDoNotCount() throws {
    let files = [
      try file(#"{"state":"upload_failed","reason":"too_large"}"#, size: 90 * mb),
      try file(uploading(mb, 2 * mb), size: 2 * mb),
    ]
    XCTAssertEqual(UploadPill.of(files), .sending(doneBytes: mb, totalBytes: 2 * mb))
  }

  func testAStalledFileMakesTheMessageFailed() throws {
    let files = [
      try file(uploading(mb, 2 * mb), size: 2 * mb),
      try file(#"{"state":"upload_stalled","reason":"storage_unavailable"}"#, size: mb),
    ]
    XCTAssertEqual(UploadPill.of(files), .failed(isStorageUnavailable: true))
  }

  func testReportedBytesBeyondTheTotalDoNotOverfill() throws {
    XCTAssertEqual(UploadPill.of([try file(uploading(9 * mb, 4 * mb), size: 4 * mb)])?.maybeFraction, 1)
  }

  private func uploading(_ done: Int64, _ total: Int64, failedAttempts: Int = 0) -> String {
    #"{"state":"uploading","done_bytes":\#(done),"total_bytes":\#(total),"failed_attempts":\#(failedAttempts)}"#
  }

  private func file(_ transfer: String, size: Int64) throws -> FileInfo {
    let json = """
      {"conversation":"1","message":"2","file_id":"pending-0","file_name":"photo.jpg","media_type":"image/jpeg",
      "size_bytes":\(size),"transfer":\(transfer)}
      """
    return try XCTUnwrap(Wire.decode(FileInfo.self, from: json))
  }
}
