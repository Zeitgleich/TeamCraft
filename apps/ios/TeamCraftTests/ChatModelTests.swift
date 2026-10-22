import XCTest

@testable import TeamCraft

final class ChatModelTests: XCTestCase {
  private func item(
    _ id: String, _ name: String, kind: ConversationKind = .public, space: String? = nil,
    favorite: Bool = false
  ) -> ConversationItem {
    ConversationItem(
      id: id, maybeSpaceId: space, maybeSpaceName: space, kind: kind, name: name, topic: "",
      unreadCount: 0, mentionCount: 0, isFavorite: favorite, isMuted: false, canPost: true)
  }

  func testSectionsOrderFavoritesSpacesAndDirectMessages() {
    let sections = conversationSections([
      item("1", "random", space: "Team B"),
      item("2", "general", space: "Team A"),
      item("3", "Sam", kind: .direct),
      item("4", "announcements", space: "Team A", favorite: true),
      item("5", "loose"),
    ])
    XCTAssertEqual(sections.map(\.title), [.favorites, .space("Team A"), .space("Team B"), .channels, .directMessages])
    XCTAssertEqual(sections[0].conversations.map(\.id), ["4"])
    XCTAssertEqual(sections[4].conversations.map(\.id), ["3"])
  }

  func testEmptySectionsAreLeftOut() {
    XCTAssertTrue(conversationSections([]).isEmpty)
  }

  func testActionsSerializeToTheCoreSchema() throws {
    let reaction = ChatAction.toggleReaction(conversationId: "10", messageId: "20", emoji: "👍")
    let object = try JSONSerialization.jsonObject(with: Data(reaction.json.utf8)) as? [String: String]
    XCTAssertEqual(object, ["action": "toggle_reaction", "conversation": "10", "message": "20", "emoji": "👍"])

    let status = ChatAction.setCustomStatus(CustomStatusValue(emoji: "🌴", text: "Away", expiry: .oneHour))
    let nested = try JSONSerialization.jsonObject(with: Data(status.json.utf8)) as? [String: Any]
    XCTAssertEqual(nested?["action"] as? String, "set_custom_status")
    XCTAssertEqual((nested?["maybe_status"] as? [String: String])?["expiry"], "OneHour")

    let cleared = try JSONSerialization.jsonObject(with: Data(ChatAction.setCustomStatus(nil).json.utf8)) as? [String: Any]
    XCTAssertTrue(cleared?["maybe_status"] is NSNull)
  }

  func testPassphraseRules() {
    XCTAssertEqual(PassphraseRules.problem(passphrase: "short", repeated: "short"), .tooShort)
    XCTAssertEqual(PassphraseRules.problem(passphrase: "long enough pass", repeated: "different pass!"), .mismatch)
    XCTAssertNil(PassphraseRules.problem(passphrase: "long enough pass", repeated: "long enough pass"))
  }

  func testAvatarInitials() {
    XCTAssertEqual(AvatarView.initials(of: "Alex Example"), "AE")
    XCTAssertEqual(AvatarView.initials(of: ""), "?")
  }
}
