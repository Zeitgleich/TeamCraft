import XCTest

@testable import TeamCraft

final class VoiceMessagesTests: XCTestCase {
  func testAnAudioFileWithAWaveformIsAVoiceMessage() throws {
    let voice = try decode(mediaType: "audio/ogg", extra: #","maybe_duration_ms":4200,"maybe_waveform":[8,255,40]"#)
    XCTAssertTrue(voice.isVoiceMessage)
    XCTAssertEqual(voice.maybeWaveform, [8, 255, 40])
    XCTAssertEqual(voice.maybeDurationMs, 4200)
    XCTAssertFalse(try decode(mediaType: "audio/ogg").isVoiceMessage, "plain audio without a waveform")
    XCTAssertFalse(try decode(mediaType: "audio/ogg", extra: #","maybe_waveform":[]"#).isVoiceMessage)
    XCTAssertFalse(try decode(mediaType: "image/png", extra: #","maybe_waveform":[1,2]"#).isVoiceMessage)
  }

  func testThePolicyDefaultsToOnAndClampsItsLength() throws {
    let absent = try XCTUnwrap(Wire.decode(VoicePolicySnapshot.self, from: #"{"security":{}}"#))
    XCTAssertNil(absent.security?.voiceMessagePolicy)
    XCTAssertEqual(VoiceMessagePolicy.fallback.maxSeconds, 600)
    let json = #"{"security":{"voice_message_policy":{"enabled":false,"max_minutes":5}}}"#
    let stored = try XCTUnwrap(Wire.decode(VoicePolicySnapshot.self, from: json)?.security?.voiceMessagePolicy)
    XCTAssertFalse(stored.enabled)
    XCTAssertEqual(stored.maxSeconds, 300)
    XCTAssertEqual(VoiceMessagePolicy(enabled: true, maxMinutes: 0).maxSeconds, 60)
    XCTAssertEqual(VoiceMessagePolicy(enabled: true, maxMinutes: 900).maxSeconds, 3600)
  }

  func testSpeedsCycleAndFallBackToNormal() {
    XCTAssertEqual(VoiceSpeed.normal.next, .oneAndHalf)
    XCTAssertEqual(VoiceSpeed.oneAndHalf.next, .double)
    XCTAssertEqual(VoiceSpeed.double.next, .normal)
    XCTAssertEqual(VoiceSpeed.oneAndHalf.rate, 1.5)
    XCTAssertEqual(VoiceSpeed.stored("3"), .normal)
    XCTAssertEqual(VoiceSpeed.double.label, "2×")
  }

  func testAHeldRecordingSendsCancelsOrLocksByWhereTheFingerWent() {
    let outcome = VoiceHoldGesture.outcome(translation:)
    XCTAssertEqual(outcome(CGSize(width: -20, height: -10)), .send)
    XCTAssertEqual(outcome(CGSize(width: -120, height: -10)), .cancel)
    XCTAssertEqual(outcome(CGSize(width: -10, height: -90)), .lock)
    XCTAssertEqual(outcome(CGSize(width: -150, height: -80)), .cancel, "further left than up")
    XCTAssertEqual(outcome(CGSize(width: 200, height: 40)), .send)
  }

  func testOggAndWebmAreDecodedOtherAudioPlaysAsItIs() {
    XCTAssertTrue(VoiceFormat.needsDecoding(mediaType: "audio/ogg; codecs=opus", fileName: "a.ogg"))
    XCTAssertTrue(VoiceFormat.needsDecoding(mediaType: "audio/webm", fileName: "a"))
    XCTAssertTrue(VoiceFormat.needsDecoding(mediaType: "application/octet-stream", fileName: "note.OPUS"))
    XCTAssertFalse(VoiceFormat.needsDecoding(mediaType: "audio/mp4", fileName: "a.ogg"), "the type decides")
    XCTAssertFalse(VoiceFormat.needsDecoding(mediaType: "audio/mpeg", fileName: "a.mp3"))
  }

  func testVoiceFilesTravelWithTheirLengthAndWaveform() throws {
    let file = PickedFile(
      fileName: "voice-message-20261105-140312.ogg", mediaType: "audio/ogg", path: "/data/uploads/v.ogg",
      maybeDurationMs: 4200, maybeWaveform: [8, 255])
    let request = SendRequest(conversationId: "10", maybeThreadId: nil, body: "", files: [file])
    let object = try XCTUnwrap(
      JSONSerialization.jsonObject(with: Data(ChatAction.sendMessage(request).json.utf8)) as? [String: Any])
    let message = try XCTUnwrap(object["message"] as? [String: Any])
    let sent = try XCTUnwrap((message["files"] as? [[String: Any]])?.first)
    XCTAssertEqual(sent["duration_ms"] as? Int, 4200)
    XCTAssertEqual(sent["waveform"] as? [Int], [8, 255])
    let plain = SendRequest(
      conversationId: "10", maybeThreadId: nil, body: "",
      files: [PickedFile(fileName: "a.png", mediaType: "image/png", path: "/a.png")])
    let plainObject = try XCTUnwrap(
      JSONSerialization.jsonObject(with: Data(ChatAction.sendMessage(plain).json.utf8)) as? [String: Any])
    let plainFile = try XCTUnwrap(((plainObject["message"] as? [String: Any])?["files"] as? [[String: Any]])?.first)
    XCTAssertNil(plainFile["waveform"])
    XCTAssertNil(plainFile["duration_ms"])
  }

  private func decode(mediaType: String, extra: String = "") throws -> FileInfo {
    let json = """
      {"conversation":"1","message":"7","file_id":"9","file_name":"voice","media_type":"\(mediaType)",
      "size_bytes":2048,"transfer":{"state":"remote"}\(extra)}
      """
    return try XCTUnwrap(Wire.decode(FileInfo.self, from: json))
  }
}
