import XCTest

@testable import TeamCraft

/// The lobby of a meeting in its call, for its hosts (#1859), as desktop and web offer it.
final class MeetingLobbyTests: XCTestCase {
  private func person(_ id: String, _ status: LobbyPersonStatus) -> LobbyPerson {
    LobbyPerson(userId: id, displayName: "Person \(id)", status: status, canAdmit: false)
  }

  func testTheLobbyIsReadAgainOnlyWhileAGuestsKeyCodeIsMissing() {
    XCTAssertTrue(isWaitingForKeyCode(MeetingLobby(meetingId: "7", people: [person("2", .guest(maybeKeyCode: nil))])))
    XCTAssertFalse(isWaitingForKeyCode(MeetingLobby(meetingId: "7", people: [person("3", .guest(maybeKeyCode: "1"))])))
    XCTAssertFalse(
      isWaitingForKeyCode(MeetingLobby(meetingId: "7", people: [person("1", .member), person("4", .guestDevicePending)])))
  }
}
