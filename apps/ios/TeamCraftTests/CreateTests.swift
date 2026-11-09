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
