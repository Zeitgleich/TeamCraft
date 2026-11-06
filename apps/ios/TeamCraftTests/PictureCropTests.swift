import XCTest

@testable import TeamCraft

final class PictureCropTests: XCTestCase {
  func testTurningLeftFourTimesGivesTheSameFrame() {
    let frame = CropFrame(left: 0.1, top: 0.2, right: 0.5, bottom: 0.9)
    assertFrame(frame, frame.turnedLeft().turnedLeft().turnedLeft().turnedLeft())
  }

  func testAFrameInTheTopLeftIsInTheBottomLeftAfterATurnLeft() {
    assertFrame(CropFrame(left: 0, top: 0.5, right: 0.5, bottom: 1), CropFrame(left: 0, top: 0, right: 0.5, bottom: 0.5).turnedLeft())
  }

  func testTheOriginalFrameUndoesTheTurns() {
    let frame = CropFrame(left: 0.1, top: 0.2, right: 0.5, bottom: 0.9)
    for turns in 0...5 {
      var shown = frame
      for _ in 0..<turns { shown = shown.turnedLeft() }
      assertFrame(frame, shown.inOriginal(quarterTurnsLeft: turns))
    }
  }

  func testACornerNeverFoldsTheFrameOrLeavesThePicture() {
    let frame = CropFrame().dragged(.topLeft, dx: 2, dy: -1)
    XCTAssertEqual(frame.top, 0)
    XCTAssertEqual(frame.left, 1 - minCropShare, accuracy: 0.0001)
  }

  func testMovingKeepsTheSizeInsideThePicture() {
    let frame = CropFrame(left: 0.2, top: 0.2, right: 0.6, bottom: 0.5).moved(dx: 0.9, dy: -0.9)
    XCTAssertEqual(frame.width, 0.4, accuracy: 0.0001)
    XCTAssertEqual(frame.right, 1, accuracy: 0.0001)
    XCTAssertEqual(frame.top, 0, accuracy: 0.0001)
  }

  private func assertFrame(_ expected: CropFrame, _ actual: CropFrame, file: StaticString = #filePath, line: UInt = #line) {
    XCTAssertEqual(expected.left, actual.left, accuracy: 0.0001, file: file, line: line)
    XCTAssertEqual(expected.top, actual.top, accuracy: 0.0001, file: file, line: line)
    XCTAssertEqual(expected.right, actual.right, accuracy: 0.0001, file: file, line: line)
    XCTAssertEqual(expected.bottom, actual.bottom, accuracy: 0.0001, file: file, line: line)
  }
}
