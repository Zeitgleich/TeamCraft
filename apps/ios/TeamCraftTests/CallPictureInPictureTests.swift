import XCTest

@testable import TeamCraft

/// Which video picture-in-picture shows (#2067).
final class CallPictureInPictureTests: XCTestCase {
  private let renderer = RemoteVideoRenderer()

  func testAnAudioCallGetsNoFloatingWindow() {
    XCTAssertNil(CallPictureInPictureChoice.stream(among: [], speaking: ["u1"], userIdByDevice: [:]))
  }

  func testTheSpeakersCameraIsShown() {
    let ada = stream("1", device: "d1")
    let bob = stream("2", device: "d2")
    let shown = CallPictureInPictureChoice.stream(
      among: [ada, bob], speaking: ["bob"], userIdByDevice: ["d1": "ada", "d2": "bob"])
    XCTAssertEqual(shown?.id, bob.id)
  }

  func testASharedScreenComesFirst() {
    let bob = stream("2", device: "d2")
    let screen = stream("3", device: "d1", isScreen: true)
    let shown = CallPictureInPictureChoice.stream(
      among: [bob, screen], speaking: ["bob"], userIdByDevice: ["d1": "ada", "d2": "bob"])
    XCTAssertEqual(shown?.id, screen.id)
  }

  func testVideoThePhoneCantDecodeIsSkipped() {
    let av1 = stream("1", device: "d1", isPlayable: false)
    let h264 = stream("2", device: "d2")
    let shown = CallPictureInPictureChoice.stream(
      among: [av1, h264], speaking: ["ada"], userIdByDevice: ["d1": "ada", "d2": "bob"])
    XCTAssertEqual(shown?.id, h264.id, "nobody watching Ada's undecodable video: the next one")
  }

  private func stream(_ id: String, device: String, isScreen: Bool = false, isPlayable: Bool = true) -> RemoteVideoStream {
    RemoteVideoStream(
      track: RemoteVideoTrack(streamId: id, deviceId: device, isScreen: isScreen, isPlayable: isPlayable),
      renderer: renderer)
  }
}
