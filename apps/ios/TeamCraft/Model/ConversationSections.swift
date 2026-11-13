import Foundation

/// A sidebar section, as the desktop shows them.
enum SectionTitle: Hashable {
  case favorites
  case space(String)
  case channels
  case directMessages
  /// The list layout's one list (#1151), favorites pinned on top (#1786).
  case everything
  /// The Chats tab's one list of DMs (#1786), favorites pinned on top.
  case chats
  /// Archived channels, for guests, who have no channel browser to find them in.
  case archived
  /// Home's DMs with something new, on top of its list (#1488).
  case unreadDirect
  /// The selected space's channels on Home of the spaces layout (#1526); the header names the space.
  case spaceChannels(spaceId: String)
  /// The Activity tab's one list (shown without a header).
  case activity

  /// A tab's one list, shown without a header (#1786): Chats, the list layout's mixed list and
  /// Activity.
  var isHeaderless: Bool {
    switch self {
    case .chats, .everything, .activity: return true
    default: return false
    }
  }
}

struct ConversationSection: Identifiable {
  let title: SectionTitle
  let conversations: [ConversationItem]
  /// Set where a space's section comes from the space list rather than its channels: a space
  /// without joined channels (#1526).
  var maybeSpaceId: String? = nil
  var maybeImageVersion: String? = nil

  var id: SectionTitle { title }

  /// The channels of one space, under its name.
  var isSpace: Bool {
    if case .space = title { return true }
    return false
  }

  /// The key the section's collapsed state is remembered under (#1488): a space by its id, so
  /// renaming it keeps the choice; `nil` for sections that can't be collapsed. Same as Android.
  var collapseKey: String? {
    switch title {
    case .favorites: return "favorites"
    case .unreadDirect: return "unread-direct"
    case .channels: return "channels"
    case .archived: return "archived"
    case .directMessages: return "direct"
    case .space(let name):
      if let spaceId { return "space:\(spaceId)" }
      return "space-name:\(name)"
    case .spaceChannels(let spaceId): return "space:\(spaceId)"
    case .chats, .everything, .activity: return nil
    }
  }

  /// The space the section is of, from the space list or its channels; `nil` for other sections.
  var spaceId: String? {
    switch title {
    case .space: return maybeSpaceId ?? conversations.compactMap(\.maybeSpaceId).first
    case .spaceChannels(let spaceId): return spaceId
    default: return nil
    }
  }

  /// The space's picture version, from the space list or its channels.
  var spaceImageVersion: String? { maybeImageVersion ?? conversations.compactMap(\.maybeSpaceImageVersion).first }

  /// The rows the section shows: all while open; while collapsed only those with something new and
  /// the open conversation, as the desktop sidebar does, so nothing new hides.
  func visibleRows(isCollapsed: Bool, selectedId: String?) -> [ConversationItem] {
    guard isCollapsed else { return conversations }
    return conversations.filter { HomeFilter.unread.matches($0) || $0.id == selectedId }
  }
}

/// How the chats screen lists conversations (#1151). A setting of this device.
enum ChatListLayout: String, CaseIterable {
  /// Home for the DMs, a tab for the channels by space.
  case spaces
  /// The DMs and the channels of all spaces in one list.
  case list

  static let key = "chat_list_layout"
}

/// How the list layout orders its channels (#1345). A setting of this device, like the layout.
enum ChatListGrouping: String, CaseIterable {
  /// DMs and channels together, the most recent first.
  case mixed
  /// The DMs, then the channels of each space under its name.
  case bySpace

  static let key = "chat_list_grouping"
}

/// How the Favorites section of Home (the Channels tab of the spaces layout) is ordered. The choice
/// follows the account (the desktop's Favorites sort).
enum FavoritesSort: String, CaseIterable {
  /// By name (what the section did before there was a choice).
  case alphabetical
  /// The favorite with the newest message first.
  case recent

  static let key = "favorites_sort"

  static func stored(_ maybeRaw: String?) -> FavoritesSort {
    maybeRaw.flatMap(FavoritesSort.init(rawValue:)) ?? .alphabetical
  }
}

/// The floating bottom bar of the chats screen (#1786): Chats (the DMs, the tab the app starts on),
/// Channels (the selected space's channels, Home before), Meetings and Activity (what waits for the
/// user). iOS only for now; Android keeps Home, DMs and Activity (#1488).
enum ChatTab: Hashable, CaseIterable {
  case chats, channels, meetings, activity

  /// The bar's items: the list layout has every conversation on Chats and no Channels tab.
  static func barItems(isList: Bool) -> [ChatTab] {
    isList ? allCases.filter { $0 != .channels } : allCases
  }

  /// The tab on screen for the one picked: the list layout shows its channels on Chats.
  func shown(isList: Bool) -> ChatTab {
    isList && self == .channels ? .chats : self
  }

  /// The tab a conversation opened from elsewhere (a notification, a link, a meeting) lands on: one
  /// that lists it. Channels lists the DMs with something new too and Activity both, so a DM opened
  /// from there stays highlighted there.
  func opening(_ item: ConversationItem, isList: Bool) -> ChatTab {
    let tab = shown(isList: isList)
    switch tab {
    case .chats where !isList && !item.isDirect: return .channels
    case .meetings: return isList || item.isDirect ? .chats : .channels
    default: return tab
    }
  }

  /// What the bar shows on the item: a number for what is addressed to the user (mentions, DMs,
  /// requests to join), `hasNew` (a dot) for new channel messages. Channels and Activity count only
  /// mentions; Chats every unread DM, and in the list layout the channels' mentions too. Meetings
  /// has no mark.
  func mark(_ conversations: [ConversationItem], isList: Bool, joinRequests: Int) -> (count: Int, hasNew: Bool) {
    let direct = TabBadge(conversations.filter(\.isDirect))
    let channels = TabBadge(conversations.filter { !$0.isDirect })
    switch shown(isList: isList) {
    case .chats where isList: return (max(direct.mentions + channels.mentions, direct.unread), channels.unread > 0)
    case .chats: return (max(direct.mentions, direct.unread), false)
    case .channels: return (channels.mentions, channels.unread > 0)
    case .meetings: return (0, false)
    case .activity: return (direct.mentions + channels.mentions + joinRequests, false)
    }
  }
}

/// The filter chips of the Chats tab (as in Element): one at a time, "All" selected by default.
enum HomeFilter: Hashable, CaseIterable {
  case all, unread, people, groups, channels

  /// The chips, in display order (#1786: "All" is a chip of its own).
  static let chips: [HomeFilter] = [.all, .unread, .people, .groups]
  /// The chips of the list layout, where channels are in the list too.
  static let listChips: [HomeFilter] = [.all, .unread, .people, .groups, .channels]

  func matches(_ item: ConversationItem) -> Bool {
    switch self {
    case .all: return true
    case .unread: return item.mentionCount > 0 || (item.unreadCount > 0 && !item.isMuted)
    case .people: return item.kind == .direct
    case .groups: return item.kind == .group
    case .channels: return !item.isDirect
    }
  }

  /// The number on the Unread chip (#1786): the conversations of the Chats tab it lets through, the
  /// DMs or in the list layout every listed conversation, each counted once.
  static func unreadCount(_ conversations: [ConversationItem], isList: Bool) -> Int {
    var seen = Set<String>()
    return conversations.filter {
      seen.insert($0.id).inserted && ($0.isDirect || (isList && !$0.isArchived)) && unread.matches($0)
    }.count
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

  /// The name lists and titles show: the notes to self have none of their own (they are named
  /// after the other people, and there are none), so they are called "Notes" in the user's
  /// language, like the web and desktop (`notes-title`, issue #829).
  var listName: String { conversationListName(name, isNotesToSelf: isNotesToSelf) }
}

/// The name of a conversation as lists show it: the notes to self have an empty name of their
/// own, so they are called "Notes" in the user's language (`notes-title`, issue #1041).
func conversationListName(_ name: String, isNotesToSelf: Bool) -> String {
  isNotesToSelf ? String(localized: "notes_title") : name
}

extension SearchHitItem {
  /// The conversation name a search hit row shows (same rule as `ConversationItem.listName`).
  var listName: String { conversationListName(conversationName, isNotesToSelf: isNotesToSelf) }
}

private func byName(_ lhs: ConversationItem, _ rhs: ConversationItem) -> Bool {
  lhs.listName.localizedCaseInsensitiveCompare(rhs.listName) == .orderedAscending
}

/// The favorites in the order the user chose: by name, or the newest message first (one without a
/// message last, ties by name). The one ordering of every Favorites section of the spaces layout.
func sortedFavorites(
  _ favorites: [ConversationItem], by sort: FavoritesSort, lastActivity: [String: Date]
) -> [ConversationItem] {
  switch sort {
  case .alphabetical:
    return favorites.sorted(by: byName)
  case .recent:
    return favorites.sorted { lhs, rhs in
      let left = lastActivity[lhs.id] ?? .distantPast
      let right = lastActivity[rhs.id] ?? .distantPast
      return left == right ? byName(lhs, rhs) : left > right
    }
  }
}

/// Channels of the spaces: favorite channels first, then each space (spaces and channels by name).
/// Same grouping as the Android app.
func channelSections(
  _ conversations: [ConversationItem], listsArchived: Bool = false, favoritesSort: FavoritesSort = .alphabetical,
  lastActivity: [String: Date] = [:]
) -> [ConversationSection] {
  // Archived channels leave the lists; Browse finds them (issue #212).
  let channels = conversations.filter { !$0.isDirect && !$0.isArchived }
  let favorites = sortedFavorites(channels.filter(\.isFavorite), by: favoritesSort, lastActivity: lastActivity)
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
  let archived = conversations.filter { !$0.isDirect && $0.isArchived }
  if listsArchived && !archived.isEmpty {
    sections.append(ConversationSection(title: .archived, conversations: archived.sorted(by: byName)))
  }
  return sections
}

/// Home of the spaces layout (#1488): the DMs with something new (not favorites, the most recent
/// first), then the favorites (DMs and channels, by name or the newest first, as chosen), then each space with its channels, as
/// `channelSections` has them. Read DMs are on the DMs tab. Empty sections are left out.
func homeSections(
  _ conversations: [ConversationItem], lastActivity: [String: Date] = [:], listsArchived: Bool = false,
  favoritesSort: FavoritesSort = .alphabetical
) -> [ConversationSection] {
  var seen = Set<String>()
  let unique = conversations.filter { seen.insert($0.id).inserted }
  let unreadDirect = unique
    .filter { $0.isDirect && !$0.isFavorite && HomeFilter.unread.matches($0) }
    .sorted { lhs, rhs in
      let left = lastActivity[lhs.id] ?? .distantPast
      let right = lastActivity[rhs.id] ?? .distantPast
      return left == right ? byName(lhs, rhs) : left > right
    }
  let favorites = sortedFavorites(
    unique.filter { $0.isFavorite && !$0.isArchived }, by: favoritesSort, lastActivity: lastActivity)
  var sections: [ConversationSection] = []
  if !unreadDirect.isEmpty { sections.append(ConversationSection(title: .unreadDirect, conversations: unreadDirect)) }
  if !favorites.isEmpty { sections.append(ConversationSection(title: .favorites, conversations: favorites)) }
  sections += channelSections(unique, listsArchived: listsArchived).filter { $0.title != .favorites }
  return sections
}

/// The Activity tab: every conversation with a mention, or unread and not muted, mentions first,
/// then the most recent. Same as Android.
func activityConversations(_ conversations: [ConversationItem], lastActivity: [String: Date] = [:]) -> [ConversationItem] {
  conversations
    .filter { HomeFilter.unread.matches($0) }
    .sorted { lhs, rhs in
      if lhs.mentionCount != rhs.mentionCount { return lhs.mentionCount > rhs.mentionCount }
      let left = lastActivity[lhs.id] ?? .distantPast
      let right = lastActivity[rhs.id] ?? .distantPast
      return left == right ? byName(lhs, rhs) : left > right
    }
}

/// The Chats tab (#1786): one list of the DMs, the favorites pinned on top, then the rest, each part
/// the most recent first (`lastActivity`: newest message per conversation; by name without one). Only
/// conversations `chip` lets through whose names contain `filter` (any case) are kept. No section
/// without rows.
func directSections(
  _ conversations: [ConversationItem], lastActivity: [String: Date] = [:], filter: String = "",
  chip: HomeFilter = .all
) -> [ConversationSection] {
  let direct = pinnedByRecency(
    conversations.filter { $0.isDirect && chip.matches($0) && nameContains($0, filter) }, lastActivity: lastActivity)
  return direct.isEmpty ? [] : [ConversationSection(title: .chats, conversations: direct)]
}

/// What the one search field finds by name (#1786), above the message hits: every listed DM and
/// channel whose name contains `query` (any case), the favorites first, then the most recent.
/// Archived channels only where they are listed (`listsArchived`: guests, who can't browse them).
func nameMatches(
  _ conversations: [ConversationItem], query: String, lastActivity: [String: Date] = [:], listsArchived: Bool = false
) -> [ConversationItem] {
  guard !query.trimmingCharacters(in: .whitespaces).isEmpty else { return [] }
  return pinnedByRecency(
    conversations.filter { (listsArchived || !$0.isArchived) && nameContains($0, query) }, lastActivity: lastActivity)
}

private func nameContains(_ item: ConversationItem, _ filter: String) -> Bool {
  let needle = filter.trimmingCharacters(in: .whitespaces)
  return needle.isEmpty || item.listName.localizedCaseInsensitiveContains(needle)
}

/// Each conversation once, however often a caller hands it over; the favorites first, each part the
/// most recent first (by name without a message).
private func pinnedByRecency(_ conversations: [ConversationItem], lastActivity: [String: Date]) -> [ConversationItem] {
  var seen = Set<String>()
  return conversations
    .filter { seen.insert($0.id).inserted }
    .sorted { lhs, rhs in
      if lhs.isFavorite != rhs.isFavorite { return lhs.isFavorite }
      let left = lastActivity[lhs.id] ?? .distantPast
      let right = lastActivity[rhs.id] ?? .distantPast
      return left == right ? byName(lhs, rhs) : left > right
    }
}

/// The list layout's one list (#1151): every DM and channel, the favorites pinned on top (#1786),
/// then the rest, each part the most recent first. Only conversations `chip` lets through whose
/// names contain `filter` (any case) are kept. No section without rows.
///
/// `bySpace` (#1345): the favorites in a section of their own, then the DMs, then the other channels
/// of each space under its name (spaces by name, as in the Spaces tab), each still the most recent
/// first. Favorites stay on top, across spaces; a space whose channels are all favorites has no
/// section. Same as Android.
func listSections(
  _ conversations: [ConversationItem], lastActivity: [String: Date] = [:], filter: String = "",
  chip: HomeFilter = .all, bySpace: Bool = false
) -> [ConversationSection] {
  let shown = pinnedByRecency(
    conversations.filter { chip.matches($0) && nameContains($0, filter) }, lastActivity: lastActivity)
  guard bySpace else {
    return shown.isEmpty ? [] : [ConversationSection(title: .everything, conversations: shown)]
  }
  let favorites = shown.filter(\.isFavorite)
  let rest = shown.filter { !$0.isFavorite }
  var sections: [ConversationSection] = []
  if !favorites.isEmpty { sections.append(ConversationSection(title: .favorites, conversations: favorites)) }
  let direct = rest.filter(\.isDirect)
  if !direct.isEmpty { sections.append(ConversationSection(title: .directMessages, conversations: direct)) }
  // Grouping keeps the recency order inside each space.
  let channels = rest.filter { !$0.isDirect }
  let bySpaceName = Dictionary(grouping: channels, by: \.maybeSpaceName)
  let names = bySpaceName.keys.compactMap { $0 }.sorted { $0.localizedCaseInsensitiveCompare($1) == .orderedAscending }
  for name in names {
    sections.append(ConversationSection(title: .space(name), conversations: bySpaceName[name] ?? []))
  }
  if let homeless = bySpaceName[nil], !homeless.isEmpty {
    sections.append(ConversationSection(title: .channels, conversations: homeless))
  }
  return sections
}

/// A space the user belongs to, as the space switcher and the lists name it (#1526).
struct HomeSpace: Equatable, Identifiable, Sendable {
  let id: String
  var name: String
  var maybeImageVersion: String?
  /// Mentions in its channels.
  var mentionCount = 0
  /// Something new in a channel that isn't muted.
  var hasUnread = false
}

/// Every space of the snapshot by name, with what is new in its channels counted from the live
/// `conversations`, so the badges follow each change without a new snapshot read (#1466); so are its
/// name and picture, where it has a channel. Spaces without joined channels are listed too. Same as
/// Android.
func homeSpaces(_ spaces: [HomeSpace], conversations: [ConversationItem]) -> [HomeSpace] {
  var seenChannels = Set<String>()
  let channels = conversations.filter { !$0.isDirect && !$0.isArchived && seenChannels.insert($0.id).inserted }
  let bySpace = Dictionary(grouping: channels.filter { $0.maybeSpaceId != nil }, by: { $0.maybeSpaceId ?? "" })
  var seen = Set<String>()
  return spaces
    .filter { seen.insert($0.id).inserted }
    .map { space in
      let own = bySpace[space.id] ?? []
      var result = space
      // A rename or a new picture reaches the channels before the next snapshot read.
      if let live = own.first {
        result.name = live.maybeSpaceName ?? space.name
        result.maybeImageVersion = live.maybeSpaceImageVersion ?? space.maybeImageVersion
      }
      result.mentionCount = own.reduce(0) { $0 + Int($1.mentionCount) }
      result.hasUnread = own.contains { $0.unreadCount > 0 && !$0.isMuted }
      return result
    }
    .sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
}

/// The space Home of the spaces layout shows: the remembered one while the user still belongs to
/// it, otherwise the first by name; `nil` without spaces.
func selectedSpace(_ spaces: [HomeSpace], remembered: String?) -> HomeSpace? {
  spaces.first { $0.id == remembered } ?? spaces.first
}

/// Whether a space other than `selectedId` has something new: the switcher shows a dot then.
func hasNewElsewhere(_ spaces: [HomeSpace], selectedId: String?) -> Bool {
  spaces.contains { $0.id != selectedId && ($0.mentionCount > 0 || $0.hasUnread) }
}

/// Home of the spaces layout with a space selected (#1526), as desktop's sidebar shows one space at
/// a time: the DMs with something new, the favorites (of every space, as on desktop), then the
/// selected space's channels under `.spaceChannels`, also when it has none yet. Channels outside any
/// space stay listed. Same as Android.
func spaceHomeSections(
  _ conversations: [ConversationItem], spaceId: String, lastActivity: [String: Date] = [:], listsArchived: Bool = false,
  favoritesSort: FavoritesSort = .alphabetical
) -> [ConversationSection] {
  let shown = conversations.filter {
    $0.isDirect || $0.isFavorite || $0.maybeSpaceId == nil || $0.maybeSpaceId == spaceId
  }
  let sections = homeSections(
    shown, lastActivity: lastActivity, listsArchived: listsArchived, favoritesSort: favoritesSort)
  let channels = sections.filter(\.isSpace).flatMap(\.conversations)
  let head = sections.filter { $0.title == .unreadDirect || $0.title == .favorites }
  let tail = sections.filter { $0.title == .channels || $0.title == .archived }
  return head + [ConversationSection(title: .spaceChannels(spaceId: spaceId), conversations: channels)] + tail
}

/// The list layout grouped by space (#1526): `sections` as `listSections` made them, plus a section
/// for each space that has none (no joined channels, or all of them favorites), so every space can
/// be found, its menu opened and its channels browsed. Spaces stay ordered by name. Same as Android.
func withEmptySpaces(_ sections: [ConversationSection], spaces: [HomeSpace]) -> [ConversationSection] {
  let listed = Set(sections.filter(\.isSpace).compactMap(\.spaceId))
  let missing = spaces.filter { !listed.contains($0.id) }.map {
    ConversationSection(title: .space($0.name), conversations: [], maybeSpaceId: $0.id, maybeImageVersion: $0.maybeImageVersion)
  }
  guard !missing.isEmpty else { return sections }
  let head = sections.filter { !$0.isSpace && $0.title != .channels }
  let tail = sections.filter { $0.title == .channels }
  let merged = (sections.filter(\.isSpace) + missing).sorted { lhs, rhs in
    guard case .space(let left) = lhs.title, case .space(let right) = rhs.title else { return false }
    return left.localizedCaseInsensitiveCompare(right) == .orderedAscending
  }
  return head + merged + tail
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
    // An archived channel has no list row to clear it from.
    let listed = conversations.filter { !$0.isArchived }
    mentions = listed.reduce(0) { $0 + Int($1.mentionCount) }
    unread = listed.filter { !$0.isMuted }.reduce(0) { $0 + Int($1.unreadCount) }
  }
}
