import XCTest

@testable import TeamCraft

final class TextPreviewKindTests: XCTestCase {
  func testTextAndDelimitedFilesOpenInThePreview() {
    XCTAssertTrue(TextPreviewKind.isPreviewable(fileName: "notes.txt", mediaType: "text/plain"))
    XCTAssertTrue(TextPreviewKind.isPreviewable(fileName: "sheet.csv", mediaType: "text/csv"))
    XCTAssertTrue(TextPreviewKind.isPreviewable(fileName: "sheet.tsv", mediaType: "text/tab-separated-values; charset=utf-8"))
    XCTAssertTrue(TextPreviewKind.isPreviewable(fileName: "export.csv", mediaType: "application/vnd.ms-excel"))
  }

  func testAGenericTypeIsJudgedByTheName() {
    XCTAssertTrue(TextPreviewKind.isPreviewable(fileName: "data.CSV", mediaType: "application/octet-stream"))
    XCTAssertTrue(TextPreviewKind.isPreviewable(fileName: "server.log", mediaType: ""))
    XCTAssertFalse(TextPreviewKind.isPreviewable(fileName: "photo.jpg", mediaType: "application/octet-stream"))
  }

  func testOtherFilesStayInQuickLook() {
    XCTAssertFalse(TextPreviewKind.isPreviewable(fileName: "report.pdf", mediaType: "application/pdf"))
    XCTAssertFalse(TextPreviewKind.isPreviewable(fileName: "old.xls", mediaType: "application/vnd.ms-excel"))
  }

  func testChoicesAreKeptPerTypeOfFile() {
    XCTAssertEqual(TextPreviewKind.typeKey(fileName: "Sheet.CSV", mediaType: "text/csv"), "csv")
    XCTAssertEqual(TextPreviewKind.typeKey(fileName: "README", mediaType: "text/plain; charset=utf-8"), "text/plain")
  }
}
