import XCTest

@testable import TeamCraft

final class AttachChoiceTests: XCTestCase {
  func testTheLastUsedKindComesFirst() {
    XCTAssertEqual(AttachKind.ordered(last: .file), [.file, .photo, .camera])
    XCTAssertEqual(AttachKind.ordered(last: .photo), [.photo, .camera, .file])
  }

  func testAnUnknownStoredValueFallsBackToThePhotos() {
    XCTAssertEqual(AttachKind.parse(nil), .photo)
    XCTAssertEqual(AttachKind.parse("nonsense"), .photo)
    XCTAssertEqual(AttachKind.parse("camera"), .camera)
  }

  func testAPastedPictureKeepsItsFormatInTheFileName() {
    let image = PastedImage(data: Data([1]), fileExtension: "gif")
    XCTAssertEqual(image.fileName(index: 1, now: Date(timeIntervalSince1970: 100)), "Pasted-100-2.gif")
  }
}
