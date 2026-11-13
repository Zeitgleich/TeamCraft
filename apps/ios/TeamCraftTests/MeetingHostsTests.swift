import XCTest

@testable import TeamCraft

/// Co-hosts and handing a meeting over in its call (#1720), as desktop and web offer them.
final class MeetingHostsTests: XCTestCase {
  private func hosts(_ ownPart: MeetingPart) -> MeetingHosts {
    MeetingHosts(meetingId: "7", organizerUserId: "1", coHostUserIds: ["2"], ownPart: ownPart)
  }

  private func person(_ id: String, isGuest: Bool? = nil, isBot: Bool? = nil) -> CallPerson {
    CallPerson(id: id, displayName: "User \(id)", isGuest: isGuest, isBot: isBot)
  }

  func testTilesNameTheHostAndTheCoHosts() {
    let hosts = hosts(.participant)
    XCTAssertEqual(HostBadge(userId: "1", hosts: hosts), .host)
    XCTAssertEqual(HostBadge(userId: "2", hosts: hosts), .coHost)
    XCTAssertNil(HostBadge(userId: "3", hosts: hosts))
    XCTAssertNil(HostBadge(userId: "1", hosts: nil), "a call outside a meeting has no hosts")
  }

  func testTheOrganizersMenuMakesOrRemovesCoHostsAndHandsOver() {
    let hosts = hosts(.organizer)
    XCTAssertEqual(hostActions(for: person("3"), hosts: hosts), [.makeCoHost, .handOver])
    XCTAssertEqual(hostActions(for: person("2"), hosts: hosts), [.removeCoHost, .handOver])
  }

  func testGuestsBotsAndOtherPeoplesMenusGetNothing() {
    let hosts = hosts(.organizer)
    XCTAssertEqual(hostActions(for: person("3", isGuest: true), hosts: hosts), [], "the server refuses guests as hosts")
    XCTAssertEqual(hostActions(for: person("4", isBot: true), hosts: hosts), [])
    XCTAssertEqual(hostActions(for: person("3"), hosts: self.hosts(.coHost)), [], "only the organizer changes the hosts")
    XCTAssertEqual(hostActions(for: person("3"), hosts: nil), [])
  }

  func testTheMeetingGoesOnlyToMembersInTheCall() throws {
    let snapshot = try XCTUnwrap(
      Wire.decode(
        CallPeopleSnapshot.self,
        from: #"""
          {"me":{"id":"1","display_name":"Me"},
           "users":[{"id":"2","display_name":"Bo"},{"id":"3","display_name":"Guest","is_guest":true},
                    {"id":"4","display_name":"Bot","is_bot":true},{"id":"5","display_name":"Away"}]}
          """#))
    let candidates = handoverCandidates(["1", "2", "3", "4", "2", "9"], people: snapshot)
    XCTAssertEqual(candidates.map(\.id), ["2"])
  }

  func testTheLobbyCountsItsEntries() {
    XCTAssertEqual(lobbyWaitingCount(#"{"entries":[{"user":{}},{"user":{}}],"is_host":true}"#), 2)
    XCTAssertEqual(lobbyWaitingCount(#"{"entries":[]}"#), 0)
    XCTAssertNil(lobbyWaitingCount("not json"))
    XCTAssertNil(lobbyWaitingCount("[1,2]"))
  }
}
