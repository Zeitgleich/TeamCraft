import CoreGraphics
import XCTest

@testable import TeamCraft

final class AvatarCropTests: XCTestCase {
  func testAtZoomOneTheShorterSideFillsTheCircleInTheMiddle() {
    XCTAssertEqual(AvatarCrop(pictureWidth: 600, pictureHeight: 300).square, CGRect(x: 150, y: 0, width: 300, height: 300))
    XCTAssertEqual(AvatarCrop(pictureWidth: 300, pictureHeight: 600).square, CGRect(x: 0, y: 150, width: 300, height: 300))
  }

  func testTheSquareNeverLeavesThePicture() {
    let crop = AvatarCrop(pictureWidth: 600, pictureHeight: 300)
    XCTAssertEqual(crop.dragged(dx: -10_000, dy: 10_000, viewSide: 300).square, CGRect(x: 300, y: 0, width: 300, height: 300))
    let zoomed = crop.zoomed(to: 2).dragged(dx: 10_000, dy: 10_000, viewSide: 300)
    XCTAssertEqual(zoomed.square, CGRect(x: 0, y: 0, width: 150, height: 150))
  }

  func testThePictureFollowsTheFinger() {
    // Dragging right shows more of the left: the square moves left, scaled from view to picture.
    let moved = AvatarCrop(pictureWidth: 600, pictureHeight: 300).dragged(dx: 50, dy: 0, viewSide: 150)
    XCTAssertEqual(moved.square.minX, 50)
  }

  func testZoomStaysWithinItsLimits() {
    XCTAssertEqual(AvatarCrop(pictureWidth: 480, pictureHeight: 600).maxZoom, 5)
    XCTAssertEqual(AvatarCrop(pictureWidth: 4000, pictureHeight: 4000).maxZoom, AvatarCrop.maxZoomLimit)
    XCTAssertEqual(AvatarCrop(pictureWidth: 50, pictureHeight: 80).maxZoom, 1)
    let crop = AvatarCrop(pictureWidth: 960, pictureHeight: 960)
    XCTAssertEqual(crop.zoomed(to: 0.2).zoom, 1)
    XCTAssertGreaterThanOrEqual(crop.zoomed(to: 100).square.width, AvatarCrop.minSidePx)
  }

  func testTheProfileSaysWhetherThePictureMayChange() {
    XCTAssertTrue(OwnProfileDocument.canChangeAvatar(in: #"{"user_id":"1","has_avatar":false}"#))
    XCTAssertFalse(OwnProfileDocument.canChangeAvatar(in: #"{"user_id":"1","has_avatar":true,"can_change_avatar":false}"#))
    XCTAssertTrue(OwnProfileDocument.hasAvatar(in: #"{"has_avatar":true}"#))
  }
}
