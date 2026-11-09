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

  func testOnlyADirectMessageWithABotIsMarkedAndTakesTheCoreSHue() {
    func direct(_ kind: ConversationKind, _ peer: PeerInfo?) -> ConversationItem {
      ConversationItem(
        id: "1", maybeSpaceId: nil, maybeSpaceName: nil, kind: kind, name: "Hook", topic: "",
        unreadCount: 0, mentionCount: 0, isFavorite: false, isMuted: false, canPost: true,
        maybePeerId: peer?.userId, maybePeer: peer)
    }
    let bot = PeerInfo(userId: "7", isBot: true, avatarHue: 200)
    XCTAssertTrue(direct(.direct, bot).isWithBot)
    XCTAssertFalse(direct(.direct, PeerInfo(userId: "7", isBot: false, avatarHue: 200)).isWithBot)
    XCTAssertFalse(direct(.direct, nil).isWithBot)
    XCTAssertFalse(direct(.group, bot).isWithBot)
    XCTAssertEqual(direct(.direct, bot).avatarColor, PersonColor.avatar(hue: 200))
    XCTAssertEqual(direct(.direct, nil).avatarColor, PersonColor.avatar("1"))
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

  /// #1488: Home has the DMs with something new, then the favorites, then each space; the DMs tab
  /// lists no channels, starred or not.
  func testHomeHasUnreadDirectMessagesThenFavoritesThenSpaces() {
    let at = { (seconds: TimeInterval) in Date(timeIntervalSince1970: seconds) }
    let list = [
      item("1", "Amy", kind: .direct, unread: 2),
      item("2", "Bob", kind: .direct),
      item("3", "muted", kind: .group, unread: 4, muted: true),
      item("4", "Cid", kind: .direct, unread: 1),
      item("5", "release", space: "Project A", favorite: true),
      item("6", "Dan", kind: .direct, favorite: true, unread: 1),
      item("7", "general", space: "Project A"),
      item("8", "design", space: "Project B"),
    ]
    let home = homeSections(list + [list[0]], lastActivity: ["1": at(10), "4": at(20)])
    XCTAssertEqual(home.map(\.title), [.unreadDirect, .favorites, .space("Project A"), .space("Project B")])
    XCTAssertEqual(home[0].conversations.map(\.name), ["Cid", "Amy"])
    XCTAssertEqual(home[1].conversations.map(\.name), ["Dan", "release"])
    XCTAssertEqual(home[2].conversations.map(\.name), ["general"])
    XCTAssertTrue(directSections([item("5", "release", space: "Project A", favorite: true)]).isEmpty)
  }

  /// #1488: a collapsed section still shows what is new and the open conversation.
  func testACollapsedSectionKeepsWhatIsNewAndTheOpenConversation() {
    let section = ConversationSection(
      title: .space("Project A"),
      conversations: [
        item("1", "general", space: "Project A"),
        item("2", "release", space: "Project A", unread: 1),
        item("3", "muted", space: "Project A", unread: 3, muted: true),
        item("4", "open", space: "Project A"),
      ])
    XCTAssertEqual(section.visibleRows(isCollapsed: false, selectedId: nil).count, 4)
    XCTAssertEqual(section.visibleRows(isCollapsed: true, selectedId: "4").map(\.name), ["release", "open"])
    XCTAssertEqual(section.collapseKey, "space:Project A")
    XCTAssertEqual(ConversationSection(title: .unreadDirect, conversations: []).collapseKey, "unread-direct")
  }

  func testActivityHoldsMentionsFirstThenUnreadWithoutMutedOrRead() {
    let at = { (seconds: TimeInterval) in Date(timeIntervalSince1970: seconds) }
    let result = activityConversations(
      [
        item("1", "read", space: "Project A"),
        item("2", "Amy", kind: .direct, unread: 2),
        item("3", "muted", unread: 5, muted: true),
        item("4", "release", unread: 1, mentions: 1),
        item("5", "Bob", kind: .direct, unread: 1),
        item("6", "quiet", kind: .private, unread: 3, mentions: 2, muted: true),
      ],
      lastActivity: ["2": at(10), "5": at(20)])
    XCTAssertEqual(result.map(\.name), ["quiet", "release", "Bob", "Amy"])
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
  }

  /// #1526: Home of the spaces layout shows the selected space's channels, also when it has none.
  func testSpaceHomeShowsOnlyTheSelectedSpacesChannels() {
    let list = [
      item("1", "general", space: "Project A"),
      item("2", "random", space: "Project B"),
      item("3", "release", space: "Project B", favorite: true),
      item("4", "Amy", kind: .direct, unread: 1),
      item("5", "Bob", kind: .direct),
    ]
    let home = spaceHomeSections(list, spaceId: "Project A")
    XCTAssertEqual(home.map(\.title), [.unreadDirect, .favorites, .spaceChannels(spaceId: "Project A")])
    XCTAssertEqual(home[2].conversations.map(\.name), ["general"])
    XCTAssertEqual(home[2].collapseKey, "space:Project A")
    let empty = spaceHomeSections([item("1", "general", space: "Project A")], spaceId: "empty")
    XCTAssertEqual(empty.map(\.title), [.spaceChannels(spaceId: "empty")])
    XCTAssertTrue(empty[0].conversations.isEmpty)
  }

  /// #1526: the switcher's spaces count what is new in their channels and include empty spaces.
  func testSpacesCountWhatIsNewAndKeepEmptySpaces() {
    let spaces = [
      HomeSpace(id: "b", name: "beta"), HomeSpace(id: "Project A", name: "Old name"), HomeSpace(id: "e", name: "Empty"),
    ]
    let list = [
      item("1", "general", space: "Project A", unread: 2, mentions: 1),
      item("2", "quiet", space: "Project A", unread: 4, mentions: 2, muted: true),
      item("3", "Amy", kind: .direct, unread: 1),
    ]
    let result = homeSpaces(spaces, conversations: list)
    // The name follows the channels (a rename before the next snapshot read).
    XCTAssertEqual(result.map(\.name), ["beta", "Empty", "Project A"])
    XCTAssertEqual(result[2].mentionCount, 3)
    XCTAssertTrue(result[2].hasUnread)
    XCTAssertEqual(result[1], HomeSpace(id: "e", name: "Empty"))
    XCTAssertTrue(hasNewElsewhere(result, selectedId: "b"))
    XCTAssertFalse(hasNewElsewhere(result, selectedId: "Project A"))
    XCTAssertEqual(selectedSpace(result, remembered: "e")?.id, "e")
    XCTAssertEqual(selectedSpace(result, remembered: "gone")?.id, "b")
    XCTAssertNil(selectedSpace([], remembered: "e"))
  }

  /// #1600: a DM, also one tied to a space, never marks a space; Chats counts it.
  func testDMsNeverMarkASpace() {
    let spaces = [HomeSpace(id: "Project A", name: "Project A"), HomeSpace(id: "Project B", name: "Project B")]
    let list = [
      item("1", "Amy", kind: .direct, space: "Project A", unread: 3, mentions: 1),
      item("2", "Amy, Bob", kind: .group, unread: 2),
      item("3", "general", space: "Project B", unread: 1, mentions: 1),
    ]
    let result = homeSpaces(spaces, conversations: list)
    XCTAssertEqual(result[0], HomeSpace(id: "Project A", name: "Project A"))
    XCTAssertEqual(result[1].mentionCount, 1)
    XCTAssertTrue(result[1].hasUnread)
  }

  /// #1526: grouped by space, the list names spaces without joined channels too, by name.
  func testTheListBySpaceListsEmptySpaces() {
    let sections = listSections([item("1", "general", space: "Project B"), item("2", "Amy", kind: .direct)], bySpace: true)
    let spaces = [HomeSpace(id: "Project B", name: "Project B"), HomeSpace(id: "a", name: "Project A", maybeImageVersion: "v1")]
    let result = withEmptySpaces(sections, spaces: spaces)
    XCTAssertEqual(result.map(\.title), [.directMessages, .space("Project A"), .space("Project B")])
    XCTAssertEqual(result[1].spaceId, "a")
    XCTAssertEqual(result[1].spaceImageVersion, "v1")
    XCTAssertEqual(result[1].collapseKey, "space:a")
    XCTAssertEqual(withEmptySpaces(sections, spaces: Array(spaces.prefix(1))).map(\.title), sections.map(\.title))
  }

  /// #1345: grouped by space, the DMs come after the favorites, then each space by name.
  func testTheListGroupedBySpaceKeepsFavoritesOnTopThenDmsThenEachSpace() {
    let list = [
      item("1", "Amy", kind: .direct),
      item("2", "general", space: "Project B"),
      item("3", "random", space: "Project A"),
      item("4", "release", space: "Project A", favorite: true),
      item("5", "launch", space: "Project A"),
      item("6", "only", space: "Project C", favorite: true),
    ]
    let at = { (seconds: TimeInterval) in Date(timeIntervalSince1970: seconds) }
    let sections = listSections(list, lastActivity: ["3": at(10), "5": at(30)], bySpace: true)
    XCTAssertEqual(sections.map(\.title), [.favorites, .directMessages, .space("Project A"), .space("Project B")])
    XCTAssertEqual(sections[2].conversations.map(\.id), ["5", "3"])
    XCTAssertTrue(sections[2].isSpace)
    XCTAssertEqual(listSections(list, chip: .people, bySpace: true).map(\.title), [.directMessages])
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

  /// Issue #1041: a search hit in the notes (empty name) is called "Notes" like the conversation.
  func testASearchHitInTheNotesIsNamedLikeTheNotes() {
    let message = MessageItem(
      id: "7", conversationId: "5", authorId: "4", authorName: "Sam", sentAtMs: 0, body: "Hello",
      isEdited: false, isPending: false, replyCount: 0, maybeThreadId: nil, reactions: [], isAuthorDeleted: false)
    let notes = SearchHitItem(message: message, conversationName: "", isNotesToSelf: true, maybeSpaceName: nil)
    XCTAssertEqual(notes.listName, String(localized: "notes_title"))
    let general = SearchHitItem(message: message, conversationName: "general", isNotesToSelf: false, maybeSpaceName: nil)
    XCTAssertEqual(general.listName, "general")
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
