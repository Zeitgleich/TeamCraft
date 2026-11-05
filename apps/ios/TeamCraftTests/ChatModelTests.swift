import XCTest

@testable import TeamCraft

final class ChatModelTests: XCTestCase {
  private func item(
    _ id: String, _ name: String, kind: ConversationKind = .public, space: String? = nil,
    favorite: Bool = false, unread: UInt32 = 0, mentions: UInt32 = 0, muted: Bool = false,
    notes: Bool = false
  ) -> ConversationItem {
    ConversationItem(
      id: id, maybeSpaceId: space, maybeSpaceName: space, kind: kind, name: name, topic: "",
      unreadCount: unread, mentionCount: mentions, isFavorite: favorite, isMuted: muted, canPost: true,
      isNotesToSelf: notes)
  }

  func testRightsTheAppDoesNotSetAreOfferedAndTheServerDecides() {
    let built = item("1", "general", space: "Team A")
    XCTAssertTrue(built.canReact && built.canManageBookmarks && built.canManageChannel)
    let reader = ConversationItem(
      id: "2", maybeSpaceId: "9", maybeSpaceName: "Team A", kind: .public, name: "news", topic: "",
      unreadCount: 0, mentionCount: 0, isFavorite: false, isMuted: false, canPost: false,
      canReact: false, canManageBookmarks: false, canManageChannel: false)
    XCTAssertFalse(reader.canPost || reader.canReact || reader.canManageBookmarks || reader.canManageChannel)
  }

  func testChannelSectionsOrderFavoritesAndSpacesWithoutDirectMessages() {
    let sections = channelSections([
      item("1", "random", space: "Team B"),
      item("2", "general", space: "Team A"),
      item("3", "Sam", kind: .direct),
      item("4", "announcements", space: "Team A", favorite: true),
      item("5", "loose"),
    ])
    XCTAssertEqual(sections.map(\.title), [.favorites, .space("Team A"), .space("Team B"), .channels])
    XCTAssertEqual(sections[0].conversations.map(\.id), ["4"])
  }

  func testDirectSectionsFavoritesFirstAndFiltered() {
    let list = [
      item("1", "Sam", kind: .direct),
      item("2", "Sam, Alex", kind: .group, favorite: true),
      item("3", "general", space: "Team A"),
      item("4", "Kim", kind: .direct),
    ]
    let sections = directSections(list)
    XCTAssertEqual(sections.map(\.title), [.favorites, .directMessages])
    XCTAssertEqual(sections[1].conversations.map(\.name), ["Kim", "Sam"])
    XCTAssertEqual(directSections(list, filter: " sAm").flatMap(\.conversations).map(\.id), ["2", "1"])
  }

  /// #1152: a starred channel lists in Home next to the favorite DMs, once, and stays in its space.
  func testStarredChannelsListInHomeOnceAndStayInTheirSpace() {
    let starred = item("2", "release", space: "Team A", favorite: true, unread: 3)
    let list = [
      item("1", "Sam", kind: .direct, favorite: true), starred, starred,
      item("3", "general", space: "Team A"),
    ]
    let home = directSections(list, lastActivity: ["1": Date(timeIntervalSince1970: 10)])
    XCTAssertEqual(home.map(\.title), [.favorites])
    XCTAssertEqual(home[0].conversations.map(\.id), ["1", "2"])
    XCTAssertEqual(channelSections([starred, list[3]]).first?.conversations.map(\.id), ["2"])
    // Removing the star takes it out of Home.
    XCTAssertTrue(directSections([item("2", "release", space: "Team A")]).isEmpty)
    // The Home tab counts DMs only, so the channel's unread is counted once, in its space.
    XCTAssertEqual(TabBadge(list.filter { $0.isDirect }).unread, 0)
    XCTAssertEqual(TabBadge([starred, list[3]]).unread, 3)
  }

  /// #1151: the list layout holds the DMs and the channels of all spaces, favorites first.
  func testTheListHoldsDmsAndChannelsOfAllSpacesByRecency() {
    let list = [
      item("1", "Amy", kind: .direct),
      item("2", "general", space: "Project A"),
      item("3", "general", space: "Project B"),
      item("4", "release", space: "Project A", favorite: true),
    ]
    let at = { (seconds: TimeInterval) in Date(timeIntervalSince1970: seconds) }
    let sections = listSections(list + [list[0]], lastActivity: ["1": at(20), "2": at(30), "3": at(10)])
    XCTAssertEqual(sections.map(\.title), [.favorites, .everything])
    XCTAssertEqual(sections[1].conversations.map(\.id), ["2", "1", "3"])
    XCTAssertEqual(listSections(list, chip: .channels).flatMap(\.conversations).count, 3)
    XCTAssertEqual(listSections(list, chip: .people).flatMap(\.conversations).map(\.id), ["1"])
    XCTAssertEqual(listedSpaces(list).map(\.title), [.space("Project A"), .space("Project B")])
  }

  func testHomeChipsPickUnreadPeopleOrGroups() {
    let list = [
      item("1", "Amy", kind: .direct, unread: 1),
      item("2", "Bob", kind: .direct, unread: 4, muted: true),
      item("3", "Amy, Bob", kind: .group),
    ]
    func names(_ chip: HomeFilter) -> [String] { directSections(list, chip: chip).flatMap(\.conversations).map(\.name) }
    XCTAssertEqual(names(.unread), ["Amy"])
    XCTAssertEqual(names(.people), ["Amy", "Bob"])
    XCTAssertEqual(names(.groups), ["Amy, Bob"])
  }

  /// #897: the rule is the core's; the app only reads count, label and tone from it.
  func testRowBadgeComesFromTheCoreRule() {
    XCTAssertNil(rowBadge(unreadCount: 0, mentionCount: 0, isMuted: false))
    let red = rowBadge(unreadCount: 120, mentionCount: 0, isMuted: false)
    XCTAssertEqual(red?.label, "99+")
    XCTAssertEqual(red?.tone, .red)
    XCTAssertEqual(rowBadge(unreadCount: 3, mentionCount: 0, isMuted: true)?.tone, .quiet)
    XCTAssertEqual(rowBadge(unreadCount: 3, mentionCount: 1, isMuted: true)?.count, 1)
  }

  /// Issue #829: the notes have an empty name; lists call them "Notes" and the filter finds them.
  func testNotesAreNamedAndFoundByThatName() {
    let notesTitle = String(localized: "notes_title")
    let list = [item("1", "", kind: .direct, notes: true), item("2", "Sam", kind: .direct)]
    XCTAssertEqual(list[0].listName, notesTitle)
    XCTAssertEqual(list[1].listName, "Sam")
    XCTAssertEqual(directSections(list, filter: notesTitle).flatMap(\.conversations).map(\.id), ["1"])
  }

  func testEmptySectionsAreLeftOut() {
    XCTAssertTrue(channelSections([]).isEmpty)
    XCTAssertTrue(directSections([]).isEmpty)
    XCTAssertTrue(channelSections([item("1", "Sam", kind: .group)]).isEmpty)
  }

  func testTabBadgeSkipsMutedUnreadButCountsMentions() {
    let badge = TabBadge([
      item("1", "Sam", kind: .direct, unread: 2),
      item("2", "Kim", kind: .direct, unread: 5, mentions: 1, muted: true),
    ])
    XCTAssertEqual(badge, TabBadge(mentions: 1, unread: 2))
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

  func testFormerAccountsNameOldMessagesAndDeletedOnesGetTheAppsName() throws {
    let json = """
      {
        "me": {"id": "1", "username": "alex", "display_name": "Alex Example"},
        "users": [{"id": "2", "username": "sam", "display_name": "Sam Doe"}],
        "former_users": [
          {"id": "3", "username": "bea", "display_name": "Bea Former", "is_deactivated": true},
          {"id": "4", "username": "", "display_name": "Tombstone", "is_deactivated": true, "is_deleted": true}
        ]
      }
      """
    let people = try XCTUnwrap(Wire.decode(PeopleSnapshot.self, from: json))
    let names = people.namesById
    XCTAssertEqual(names["2"], "Sam Doe")
    XCTAssertEqual(names["3"], "Bea Former")
    XCTAssertEqual(names["4"], String(localized: "user_deleted_name"))
    // Only the people to offer are mentionable.
    XCTAssertEqual(people.users?.map(\.id), ["2"])
  }

  func testADeletedAuthorGetsTheAppsName() {
    let message = MessageItem(
      id: "7", conversationId: "5", authorId: "4", authorName: "Tombstone", sentAtMs: 0, body: "Hello",
      isEdited: false, isPending: false, replyCount: 0, maybeThreadId: nil, reactions: [], isAuthorDeleted: true)
    XCTAssertEqual(message.namingDeletedAuthor().authorName, String(localized: "user_deleted_name"))
    let hit = SearchHitItem(message: message, conversationName: "general", maybeSpaceName: nil)
    XCTAssertEqual(hit.namingDeletedAuthor().message.authorName, String(localized: "user_deleted_name"))

    var kept = message
    kept.isAuthorDeleted = false
    kept.authorName = "Sam Doe"
    XCTAssertEqual(kept.namingDeletedAuthor().authorName, "Sam Doe")
  }
}
