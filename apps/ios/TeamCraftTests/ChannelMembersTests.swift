import XCTest

@testable import TeamCraft

/// A channel's members section on the phone (issue #1262), worded as on desktop and web.
final class ChannelMembersTests: XCTestCase {
  func testOnlyAdminsAndModeratorsGetABadge() {
    XCTAssertEqual(ChannelMembers.roleKey(.admin), "channel_role_admin")
    XCTAssertEqual(ChannelMembers.roleKey(.moderator), "channel_role_moderator")
    XCTAssertNil(ChannelMembers.roleKey(.member))
    XCTAssertNil(ChannelMembers.roleKey(.other), "a role this version doesn't know isn't named")
  }

  func testACheckedKeyIsNotMarked() {
    XCTAssertNil(ChannelMembers.keyMarkKey(.checked))
    XCTAssertEqual(ChannelMembers.keyMarkKey(.changed), "channel_roles_review_key_changed")
    XCTAssertEqual(ChannelMembers.keyMarkKey(.uncheckable), "channel_roles_review_key_uncheckable")
  }

  func testEveryRefusalHasItsOwnText() {
    let keys: [RoleReviewRefusalItem] = [.unknownPerson, .uncheckableKey, .noAdmin]
    XCTAssertEqual(Set(keys.map(ChannelMembers.refusalKey)).count, keys.count)
  }

  func testOnlyAConfirmationThatDidNotHappenIsWorded() {
    XCTAssertNil(ChannelMembers.outcomeKey(.confirmed))
    XCTAssertEqual(ChannelMembers.outcomeKey(.changed), "channel_roles_review_changed")
    XCTAssertEqual(ChannelMembers.outcomeKey(.refused), "error_generic")
  }

  func testSomebodyNobodyCanNameIsTheUnknownPerson() {
    XCTAssertEqual(ChannelMembers.droppedNames(["Alice", nil], unknown: "Unknown person"), "Alice, Unknown person")
  }
}
