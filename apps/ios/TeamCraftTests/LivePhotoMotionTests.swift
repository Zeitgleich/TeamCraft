import XCTest

@testable import TeamCraft

/// Live Photos and Motion Photos (#2181): what the app hands the core and reads back.
final class LivePhotoMotionTests: XCTestCase {
  func testAPictureWithMotionGoesToTheCoreWithItsClip() throws {
    var file = PickedFile(fileName: "IMG_0001.jpg", mediaType: "image/jpeg", path: "/uploads/a/IMG_0001.jpg")
    file.maybeMotion = PickedMotion(path: "/uploads/a/IMG_0001.mp4", mediaType: "video/mp4", maybeDurationMs: 2900)

    let motion = try XCTUnwrap(file.object["motion"] as? [String: Any])

    XCTAssertEqual(motion["media_type"] as? String, "video/mp4")
    XCTAssertEqual(motion["duration_ms"] as? Int64, 2900)
    let source = try XCTUnwrap(motion["source"] as? [String: Any])
    XCTAssertEqual(source["kind"] as? String, "path")
    XCTAssertEqual(source["path"] as? String, "/uploads/a/IMG_0001.mp4")
    XCTAssertTrue(JSONSerialization.isValidJSONObject(file.object))
  }

  func testAPictureWithoutMotionSendsNoMotionField() {
    let file = PickedFile(fileName: "IMG_0002.jpg", mediaType: "image/jpeg", path: "/uploads/b/IMG_0002.jpg")
    XCTAssertNil(file.object["motion"])
  }

  func testAReceivedLivePhotoDecodesWithItsMotion() throws {
    let json = """
      {"conversation":"1","message":"2","file_id":"3","file_name":"IMG_0001.jpg","media_type":"image/jpeg",
       "size_bytes":10,"transfer":{"state":"remote"},
       "motion":{"file_id":"4","media_type":"video/mp4","size_bytes":20,"duration_ms":2900,
                 "transfer":{"state":"local","path":"/downloads/4.mp4"}}}
      """
    let file = try XCTUnwrap(Wire.decode(FileInfo.self, from: json))

    XCTAssertTrue(file.isLive)
    XCTAssertEqual(file.motion?.fileId, "4")
    XCTAssertEqual(file.motion?.durationMs, 2900)
    XCTAssertEqual(file.motion?.localPath, "/downloads/4.mp4")
  }

  func testAPictureFromAnOlderCoreIsNoLivePhoto() throws {
    let json = """
      {"conversation":"1","message":"2","file_id":"3","file_name":"a.jpg","media_type":"image/jpeg",
       "size_bytes":10,"transfer":{"state":"remote"}}
      """
    let file = try XCTUnwrap(Wire.decode(FileInfo.self, from: json))
    XCTAssertFalse(file.isLive)
  }

  func testTheUserSendsMotionUntilTurnedOff() {
    let defaults = UserDefaults.standard
    let before = defaults.object(forKey: LivePhotoMotion.settingKey)
    defer { defaults.set(before, forKey: LivePhotoMotion.settingKey) }

    defaults.removeObject(forKey: LivePhotoMotion.settingKey)
    XCTAssertTrue(LivePhotoMotion.isSendingMotion)
    defaults.set(false, forKey: LivePhotoMotion.settingKey)
    XCTAssertFalse(LivePhotoMotion.isSendingMotion)
  }

  func testAClipIsDroppedWhenTheUserSendsStillPictures() async throws {
    let clip = try temporaryClip()
    let defaults = UserDefaults.standard
    let before = defaults.object(forKey: LivePhotoMotion.settingKey)
    defer { defaults.set(before, forKey: LivePhotoMotion.settingKey) }
    defaults.set(false, forKey: LivePhotoMotion.settingKey)

    let prepared = await LivePhotoMotion.prepared(
      PickedMotion(path: clip.path, mediaType: "video/quicktime"), limits: MotionLimitsItem(isAllowed: true, maxBytes: 1 << 24))

    XCTAssertNil(prepared)
    XCTAssertFalse(FileManager.default.fileExists(atPath: clip.path), "the staged clip is deleted")
  }

  func testAClipOverTheLimitGoesUnconvertedSoTheCoreShowsItAsNotSent() async throws {
    let clip = try temporaryClip()
    let defaults = UserDefaults.standard
    let before = defaults.object(forKey: LivePhotoMotion.settingKey)
    defer { defaults.set(before, forKey: LivePhotoMotion.settingKey) }
    defaults.removeObject(forKey: LivePhotoMotion.settingKey)

    let motion = PickedMotion(path: clip.path, mediaType: "video/quicktime")
    let prepared = await LivePhotoMotion.prepared(motion, limits: MotionLimitsItem(isAllowed: true, maxBytes: 4))

    XCTAssertEqual(prepared, motion)
  }

  private func temporaryClip() throws -> URL {
    let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
    try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    let clip = folder.appendingPathComponent("IMG_0001-motion.mov")
    try Data("not really a movie".utf8).write(to: clip)
    addTeardownBlock { try? FileManager.default.removeItem(at: folder) }
    return clip
  }
}
