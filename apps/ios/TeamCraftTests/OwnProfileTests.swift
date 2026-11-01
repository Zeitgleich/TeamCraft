import XCTest

@testable import TeamCraft

final class OwnProfileTests: XCTestCase {
  private static let profile = """
    {"user_id":"1","title":"Lead","phone":"+49 30 1","has_avatar":true,"avatar_version":"7",\
    "display_name_override":"Ada","can_override_display_name":true}
    """

  func testTheNameCanBeChangedOnlyWhenTheServerSaysSo() {
    XCTAssertTrue(OwnProfileDocument.canOverrideDisplayName(in: Self.profile))
    XCTAssertFalse(
      OwnProfileDocument.canOverrideDisplayName(in: #"{"user_id":"1","can_override_display_name":false}"#))
    XCTAssertFalse(OwnProfileDocument.canOverrideDisplayName(in: #"{"user_id":"1"}"#), "older servers")
  }

  func testAChosenNameIsRead() {
    XCTAssertEqual(OwnProfileDocument.displayNameOverride(in: Self.profile), "Ada")
    XCTAssertNil(OwnProfileDocument.displayNameOverride(in: #"{"user_id":"1"}"#))
  }

  func testTheOtherFieldsAreSentBackWhenOnlyTheNameChanges() {
    XCTAssertEqual(
      OwnProfileDocument.updateBody(from: Self.profile, name: "Grace"),
      #"{"display_name":"Grace","phone":"+49 30 1","title":"Lead"}"#)
    XCTAssertEqual(
      OwnProfileDocument.updateBody(from: Self.profile, name: nil),
      #"{"phone":"+49 30 1","title":"Lead"}"#)
  }

  func testATypedNameIsCheckedBeforeItIsSent() {
    XCTAssertEqual(OwnProfileDocument.problem(typed: "  "), .missing("profile_name_empty"))
    XCTAssertEqual(OwnProfileDocument.problem(typed: "Ada\u{7}"), .rejected("profile_name_invalid"))
    XCTAssertNil(OwnProfileDocument.problem(typed: " Ada L. "))
  }
}
