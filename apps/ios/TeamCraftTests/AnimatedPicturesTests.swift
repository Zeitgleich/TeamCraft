import ImageIO
import XCTest

@testable import TeamCraft

/// Animated images (#201): the setting, reduced motion, and what the file's metadata says.
final class AnimatedPicturesTests: XCTestCase {
  func testTheSettingDecidesUntilTheUserChooses() {
    XCTAssertTrue(PlayChoice.unset.wantsToMove(.autoplay))
    XCTAssertFalse(PlayChoice.unset.wantsToMove(.onTap))
    XCTAssertFalse(PlayChoice.paused.wantsToMove(.autoplay))
    XCTAssertTrue(PlayChoice.playing.wantsToMove(.onTap))
  }

  func testReduceMotionStopsWhatWouldStartByItself() {
    XCTAssertEqual(AnimatedImagesSetting.autoplay.effective(isMotionReduced: true), .onTap)
    XCTAssertEqual(AnimatedImagesSetting.autoplay.effective(isMotionReduced: false), .autoplay)
    XCTAssertEqual(AnimatedImagesSetting.onTap.effective(isMotionReduced: false), .onTap)
  }

  func testOnlyAnExplicitChoiceLeavesAutomaticPlay() {
    XCTAssertEqual(AnimatedImagesSetting.stored(nil), .autoplay)
    XCTAssertEqual(AnimatedImagesSetting.stored("sometimes"), .autoplay)
    XCTAssertEqual(AnimatedImagesSetting.stored("on_tap"), .onTap)
  }

  func testMediaTypesAreComparedByTheirEssence() {
    XCTAssertTrue(AnimationKind.isAnimation(" Image/GIF; foo=bar"))
    XCTAssertTrue(AnimationKind.isAnimation("image/apng"))
    XCTAssertFalse(AnimationKind.isAnimation("image/webp"))
    XCTAssertTrue(AnimationKind.mayMove("image/webp"))
    XCTAssertTrue(AnimationKind.mayMove("image/avif"))
    XCTAssertFalse(AnimationKind.mayMove("image/jpeg"))
    XCTAssertEqual(AnimationKind.label("image/gif;x=y"), "GIF")
  }

  func testAFramesDelayPrefersTheUnclampedValueAndSlowsDownTheShortest() {
    let gif = kCGImagePropertyGIFDictionary
    let exact: [CFString: Any] = [gif: [kCGImagePropertyGIFUnclampedDelayTime: 0.05, kCGImagePropertyGIFDelayTime: 0.1]]
    XCTAssertEqual(AnimationFrames.delay(in: exact), 0.05)
    XCTAssertEqual(AnimationFrames.delay(in: [gif: [kCGImagePropertyGIFDelayTime: 0.2]]), 0.2)
    XCTAssertEqual(AnimationFrames.delay(in: [gif: [kCGImagePropertyGIFUnclampedDelayTime: 0.0]]), 0.1)
    XCTAssertEqual(AnimationFrames.delay(in: [:]), 0.1)
  }

  func testOnlyFilesWithinTheLimitsAreAnimated() {
    func properties(_ width: Int, _ height: Int) -> [CFString: Any] {
      [kCGImagePropertyPixelWidth: width, kCGImagePropertyPixelHeight: height]
    }
    XCTAssertTrue(AnimationFrames.isWithinLimits(properties: properties(4000, 4000), count: 10))
    XCTAssertFalse(AnimationFrames.isWithinLimits(properties: properties(4001, 4000), count: 10))
    XCTAssertFalse(AnimationFrames.isWithinLimits(properties: properties(Int.max, Int.max), count: 10))
    XCTAssertFalse(AnimationFrames.isWithinLimits(properties: properties(0, 0), count: 10))
    XCTAssertFalse(AnimationFrames.isWithinLimits(properties: [:], count: 10), "no size")
    XCTAssertFalse(
      AnimationFrames.isWithinLimits(properties: properties(10, 10), count: AnimationKind.maxFrames + 1))
  }

  func testTheFetchBudgetStopsAtItsLimitAndTakesNothingThen() {
    var budget = AutoFetchBudget(maxBytes: 10)
    XCTAssertTrue(budget.take(6))
    XCTAssertFalse(budget.take(5))
    XCTAssertTrue(budget.take(4))
    XCTAssertFalse(budget.take(1))
  }

  func testLoopsComeFromTheContainerAndZeroIsForever() {
    let gif = kCGImagePropertyGIFDictionary
    XCTAssertNil(AnimationFrames.loops(in: [gif: [kCGImagePropertyGIFLoopCount: 0]]))
    XCTAssertEqual(AnimationFrames.loops(in: [gif: [kCGImagePropertyGIFLoopCount: 3]]), 3)
    XCTAssertEqual(AnimationFrames.loops(in: [gif: [String: Any]()]), 1)
    XCTAssertNil(AnimationFrames.loops(in: [:]))
  }
}
