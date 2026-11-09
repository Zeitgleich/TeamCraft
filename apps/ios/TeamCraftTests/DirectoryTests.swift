import XCTest

@testable import TeamCraft

final class DirectoryTests: XCTestCase {
  func testGuestsGetPeopleOnlyAndInvitationsNeedTheRightToInvite() {
    XCTAssertEqual(DirectoryTab.offered(isGuest: true, canInvite: true), [.people])
    XCTAssertEqual(DirectoryTab.offered(isGuest: false, canInvite: false), [.people, .channels, .external])
    XCTAssertEqual(DirectoryTab.offered(isGuest: false, canInvite: true), [.people, .channels, .external, .invitations])
  }

  func testAnEmptyAddressIsMissing() {
    XCTAssertEqual(InviteRules.errors(email: "  ", note: "")[.email], .emailMissing)
  }

  func testMalformedAddressesSayWhatAnAddressLooksLike() {
    for typed in ["ann", "ann@", "@example.com", "ann@example", "a b@example.com", "a@b@example.com"] {
      XCTAssertEqual(InviteRules.errors(email: typed, note: "")[.email], .emailInvalid, typed)
    }
  }

  func testAPlainAddressWithANoteIsValid() {
    XCTAssertTrue(InviteRules.errors(email: " ann@example.com ", note: "Welcome").isValid)
  }

  func testALongNoteIsRefusedAtTheNote() {
    let errors = InviteRules.errors(email: "ann@example.com", note: String(repeating: "x", count: InviteRules.maxNoteCharacters + 1))
    XCTAssertEqual(errors.maybeFirst, .note)
  }

  func testSpacesOfferInvitingWhereTheUserMayAddOrInviteGuests() throws {
    let json = """
      {"me":{"id":"1","username":"ann","display_name":"Ann"},"spaces":[
        {"id":"9","name":"Sales","can_add_members":true},
        {"id":"8","name":"Ops","can_invite_guests":true},
        {"id":"7","name":"Docs"}
      ]}
      """
    let snapshot = try XCTUnwrap(Wire.decode(BrowseSnapshot.self, from: json))
    XCTAssertEqual(snapshot.spaces?.filter(\.canInvite).map(\.id), ["9", "8"])
  }
}
