import XCTest

@testable import TeamCraft

/// The rules of "New channel" and "New space" (#1534), and how the core's outcome reaches the form.
final class CreateTests: XCTestCase {
  func testAGoodNameAndNoDescriptionPass() {
    XCTAssertTrue(CreateRules.errors(name: "Project A", description: "", missing: "channel_name_missing").isValid)
  }

  func testAnEmptyNameSaysWhatToEnter() {
    let errors = CreateRules.errors(name: "   ", description: "", missing: "space_name_missing")
    XCTAssertEqual(errors[.name], .nameMissing("space_name_missing"))
    XCTAssertEqual(errors.maybeFirst, .name)
  }

  func testEveryErrorShowsAtOnceInFieldOrder() {
    let errors = CreateRules.errors(
      name: String(repeating: "x", count: CreateRules.maxNameCharacters + 1),
      description: String(repeating: "y", count: CreateRules.maxDescriptionCharacters + 1),
      missing: "channel_name_missing")
    XCTAssertEqual(errors[.name], .nameTooLong)
    XCTAssertEqual(errors[.description], .descriptionTooLong)
    XCTAssertEqual(errors.maybeFirst, .name)
  }

  func testControlCharactersAreRefusedButEmojiSequencesAreNot() {
    XCTAssertEqual(CreateRules.errors(name: "a\u{07}b", description: "", missing: "m")[.name], .nameInvalid)
    // A zero-width joiner is a format character, not a control character.
    XCTAssertNil(CreateRules.errors(name: "Team \u{1F469}\u{200D}\u{1F4BB}", description: "", missing: "m")[.name])
  }

  func testTheCoresOutcomesMapToTheForm() {
    XCTAssertEqual(CreateResult(.created(id: "42")), .created("42"))
    XCTAssertEqual(CreateResult(.nameRefused(detail: "too long")), .nameRefused)
    XCTAssertEqual(CreateResult(.notAllowed), .notAllowed)
    XCTAssertEqual(
      CreateResult(.channelNameRefused(maybeProblem: .bidiControl)), .channelNameRefused(.bidiControl))
    XCTAssertEqual(CreateResult(.channelNameRefused(maybeProblem: nil)), .channelNameRefused(nil))
  }

  /// The channel name rules are the core's (#1842): each problem has its text at the field, an empty
  /// name keeps the form's own.
  func testAChannelNameShowsTheProblemTheCoreFound() {
    XCTAssertEqual(ChannelAnchor.problem(of: "a\u{200B}b"), .invisibleCharacter)
    XCTAssertEqual(ChannelAnchor.problem(of: "a\u{202E}b"), .bidiControl)
    XCTAssertNil(ChannelAnchor.problem(of: "Team \u{1F469}\u{200D}\u{1F4BB}"))
    let errors = CreateRules.errors(
      name: "a\u{200B}b", description: "", missing: "m", maybeChannelProblem: ChannelAnchor.problem(of: "a\u{200B}b"))
    XCTAssertEqual(errors[.name], .channelName("channel_name_error_invisible"))
    let empty = CreateRules.errors(name: " ", description: "", missing: "m", maybeChannelProblem: .empty)
    XCTAssertEqual(empty[.name], .nameMissing("m"))
  }

  func testTheDeleteNoteNamesWhoLastRenamed() {
    XCTAssertEqual(ChannelAnchor.deleteNoteKey(.renamedByMember(maybeName: "Anna")).key, "channel_delete_renamed_by_member %@")
    XCTAssertEqual(ChannelAnchor.deleteNoteKey(.nameStale).key, "channel_delete_name_stale")
    XCTAssertTrue(ChannelAnchor.deleteNote(.renamedByWorkspace(maybeName: "Anna")).contains("Anna"))
    XCTAssertFalse(ChannelAnchor.deleteNote(.namedByFounder(maybeName: nil)).contains("%@"))
    XCTAssertEqual(ChannelAnchor.forkKey(.founding), "channel_anchor_forked_founding")
  }

  func testAddPeopleShowsOnlyWhereTheServerSaysSo() throws {
    let json = #"""
      {"me": {"id": "1", "username": "me", "display_name": "Me"},
       "conversations": [
         {"id": "10", "kind": "Public", "maybe_permissions": ["post", "manage_members"]},
         {"id": "11", "kind": "Private", "maybe_permissions": ["post"]},
         {"id": "12", "kind": "Private"},
         {"id": "13", "kind": "Group", "maybe_permissions": ["manage_members"]}
       ]}
      """#
    let snapshot = try XCTUnwrap(Wire.decode(BrowseSnapshot.self, from: json))
    let canAdd = (snapshot.conversations ?? []).map(\.canAddMembers)
    XCTAssertEqual(canAdd, [true, false, false, false])
  }
}
