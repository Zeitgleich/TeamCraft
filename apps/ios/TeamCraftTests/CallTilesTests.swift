import SwiftUI
import XCTest

@testable import TeamCraft

/// The call tile's "not verified" label (#1771) and the notice of a video the phone can't play (#1838).
final class CallTilesTests: XCTestCase {
  private func bundle(_ language: String) throws -> Bundle {
    let path = try XCTUnwrap(Bundle.main.path(forResource: language, ofType: "lproj"), "no \(language).lproj")
    return try XCTUnwrap(Bundle(path: path))
  }

  func testTheLabelSaysWhoIsNotVerifiedAsTheWebDoes() throws {
    let english = try bundle("en")
    let german = try bundle("de")
    let unverified = CallTileLabel(name: "Ada", isMuted: false, isUnverified: true)
    XCTAssertEqual(unverified.spoken(in: english), "Ada, not verified, gets no audio or video")
    XCTAssertEqual(unverified.spoken(in: german), "Ada, nicht verifiziert, erhält kein Bild und keinen Ton")
    let both = CallTileLabel(name: "Ada", isMuted: true, isUnverified: true)
    XCTAssertEqual(both.spoken(in: english), "Ada, muted, not verified, gets no audio or video")
    XCTAssertEqual(
      both.spoken(in: german), "Ada, stummgeschaltet, nicht verifiziert, erhält kein Bild und keinen Ton")
  }

  func testAVerifiedPersonKeepsTheNameAndTheMute() throws {
    let english = try bundle("en")
    XCTAssertEqual(CallTileLabel(name: "Ada", isMuted: false, isUnverified: false).spoken(in: english), "Ada")
    XCTAssertEqual(CallTileLabel(name: "Ada", isMuted: true, isUnverified: false).spoken(in: english), "Ada, muted")
  }

  func testAnUnplayableTrackSaysWhyAndAPlayableOneShowsItsPicture() {
    let renderer = RemoteVideoRenderer()
    let av1 = RemoteVideoStream(
      track: RemoteVideoTrack(streamId: "8", deviceId: "2", isScreen: false, isPlayable: false), renderer: renderer)
    XCTAssertFalse(av1.isPlayable, "not watched: the SFU refuses a codec the phone doesn't decode")
    XCTAssertEqual(av1.maybeNotice, LocalizedStringKey("call_video_undecodable"))
    let h264 = RemoteVideoStream(
      track: RemoteVideoTrack(streamId: "7", deviceId: "2", isScreen: true, isPlayable: true), renderer: renderer)
    XCTAssertTrue(h264.isPlayable)
    XCTAssertTrue(h264.isScreen)
    XCTAssertNil(h264.maybeNotice)
  }
}
