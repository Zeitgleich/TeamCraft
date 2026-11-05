import Foundation

/// A sidebar section, as the desktop shows them.
enum SectionTitle: Hashable {
  case favorites
  case space(String)
  case channels
  case directMessages
  /// The list layout's conversations that are not favorites (#1151).
  case everything
}

struct ConversationSection: Identifiable {
  let title: SectionTitle
  let conversations: [ConversationItem]

  var id: SectionTitle { title }
}

/// How the chats screen lists conversations (#1151). A setting of this device.
enum ChatListLayout: String, CaseIterable {
  /// Home for the DMs, a tab for the channels by space.
  case spaces
  /// The DMs and the channels of all spaces in one list.
  case list

  static let key = "chat_list_layout"
}

/// The two lists of the chats screen: Home (the DMs, outside spaces) and the channels by space.
enum ChatTab: Hashable, CaseIterable {
  case home, spaces
}

/// The filter chips of Home (as in Element): one at a time, `all` is none selected.
enum HomeFilter: Hashable, CaseIterable {
  case all, unread, people, groups, channels

  /// The chips, in display order.
  static let chips: [HomeFilter] = [.unread, .people, .groups]
  /// The chips of the list layout, where channels are in the list too.
  static let listChips: [HomeFilter] = [.unread, .people, .groups, .channels]

  func matches(_ item: ConversationItem) -> Bool {
    switch self {
    case .all: return true
    case .unread: return item.mentionCount > 0 || (item.unreadCount > 0 && !item.isMuted)
    case .people: return item.kind == .direct
    case .groups: return item.kind == .group
    case .channels: return !item.isDirect
    }
  }
}

/// How loud a counter is: red means someone needs you, plain is quiet. The chat list's badge takes
/// its tone from the shared rule (`rowBadge`).
enum BadgeTone: Equatable {
  case mention
  case unread
}

extension ConversationItem {
  /// DMs and group DMs live outside spaces and have their own tab.
  var isDirect: Bool { kind == .direct || kind == .group }

  /// Whether the Home tab lists it: DMs and group DMs, and the channels of a space the user starred
  /// (#1152). The channel stays under Favorites in its space too.
  var isListedInHome: Bool { isDirect || (isFavorite && maybeSpaceId != nil) }

  /// The name lists and titles show: the notes to self have none of their own (they are named
  /// after the other people, and there are none), so they are called "Notes" in the user's
  /// language, like the web and desktop (`notes-title`, issue #829).
  var listName: String { isNotesToSelf ? String(localized: "notes_title") : name }
}

private func byName(_ lhs: ConversationItem, _ rhs: ConversationItem) -> Bool {
  lhs.listName.localizedCaseInsensitiveCompare(rhs.listName) == .orderedAscending
}

/// Channels of the spaces: favorite channels first, then each space (spaces and channels by name).
/// Same grouping as the Android app.
func channelSections(_ conversations: [ConversationItem]) -> [ConversationSection] {
  let channels = conversations.filter { !$0.isDirect }
  let favorites = channels.filter(\.isFavorite).sorted(by: byName)
  let spaces = Dictionary(grouping: channels.filter { !$0.isFavorite }, by: \.maybeSpaceName)
  let orderedSpaces = spaces.keys.sorted { lhs, rhs in
    switch (lhs, rhs) {
    case (nil, nil): return false
    case (nil, _): return false  // channels without a space last
    case (_, nil): return true
    case (let left?, let right?): return left.localizedCaseInsensitiveCompare(right) == .orderedAscending
    }
  }
  var sections: [ConversationSection] = []
  if !favorites.isEmpty { sections.append(ConversationSection(title: .favorites, conversations: favorites)) }
  for space in orderedSpaces {
    let title: SectionTitle = space.map(SectionTitle.space) ?? .channels
    sections.append(ConversationSection(title: title, conversations: (spaces[space] ?? []).sorted(by: byName)))
  }
  return sections
}

/// The Home tab: favorites (starred channels of spaces among them, #1152), then the rest, each the most recent first (`lastActivity`: newest message
/// per conversation; by name without one). Only conversations `chip` lets through whose names
/// contain `filter` (any case) are kept. Empty sections are left out.
func directSections(
  _ conversations: [ConversationItem], lastActivity: [String: Date] = [:], filter: String = "",
  chip: HomeFilter = .all
) -> [ConversationSection] {
  let needle = filter.trimmingCharacters(in: .whitespaces)
  func byRecency(_ lhs: ConversationItem, _ rhs: ConversationItem) -> Bool {
    let left = lastActivity[lhs.id] ?? .distantPast
    let right = lastActivity[rhs.id] ?? .distantPast
    return left == right ? byName(lhs, rhs) : left > right
  }
  var seen = Set<String>()
  let direct = conversations
    // Each conversation is one row, however often a caller hands it over.
    .filter { seen.insert($0.id).inserted }
    .filter {
      $0.isListedInHome && chip.matches($0) && (needle.isEmpty || $0.listName.localizedCaseInsensitiveContains(needle))
    }
    .sorted(by: byRecency)
  let favorites = direct.filter(\.isFavorite)
  let rest = direct.filter { !$0.isFavorite }
  var sections: [ConversationSection] = []
  if !favorites.isEmpty { sections.append(ConversationSection(title: .favorites, conversations: favorites)) }
  if !rest.isEmpty { sections.append(ConversationSection(title: .directMessages, conversations: rest)) }
  return sections
}

/// The list layout's one list (#1151): favorites, then every other DM and channel, each the most
/// recent first. Only conversations `chip` lets through whose names contain `filter` (any case) are
/// kept. Empty sections are left out.
func listSections(
  _ conversations: [ConversationItem], lastActivity: [String: Date] = [:], filter: String = "",
  chip: HomeFilter = .all
) -> [ConversationSection] {
  let needle = filter.trimmingCharacters(in: .whitespaces)
  func byRecency(_ lhs: ConversationItem, _ rhs: ConversationItem) -> Bool {
    let left = lastActivity[lhs.id] ?? .distantPast
    let right = lastActivity[rhs.id] ?? .distantPast
    return left == right ? byName(lhs, rhs) : left > right
  }
  var seen = Set<String>()
  let shown = conversations
    .filter { seen.insert($0.id).inserted }
    .filter { chip.matches($0) && (needle.isEmpty || $0.listName.localizedCaseInsensitiveContains(needle)) }
    .sorted(by: byRecency)
  let favorites = shown.filter(\.isFavorite)
  let rest = shown.filter { !$0.isFavorite }
  var sections: [ConversationSection] = []
  if !favorites.isEmpty { sections.append(ConversationSection(title: .favorites, conversations: favorites)) }
  if !rest.isEmpty { sections.append(ConversationSection(title: .everything, conversations: rest)) }
  return sections
}

/// One section per space the conversations are in, by name: what the list layout names under its
/// list, each with its menu (#1151).
func listedSpaces(_ conversations: [ConversationItem]) -> [ConversationSection] {
  let bySpace = Dictionary(grouping: conversations.filter { !$0.isDirect && $0.maybeSpaceName != nil }, by: \.maybeSpaceName)
  return bySpace.keys.compactMap { $0 }
    .sorted { $0.localizedCaseInsensitiveCompare($1) == .orderedAscending }
    .map { ConversationSection(title: .space($0), conversations: bySpace[$0] ?? []) }
}

/// What a tab shows next to its name: mentions everywhere; unread messages count in DMs (they are
/// addressed to the user), in channels only as "something new". Muted conversations count only
/// mentions.
struct TabBadge: Equatable {
  let mentions: Int
  let unread: Int

  init(mentions: Int, unread: Int) {
    self.mentions = mentions
    self.unread = unread
  }

  init(_ conversations: [ConversationItem]) {
    mentions = conversations.reduce(0) { $0 + Int($1.mentionCount) }
    unread = conversations.filter { !$0.isMuted }.reduce(0) { $0 + Int($1.unreadCount) }
  }
}
