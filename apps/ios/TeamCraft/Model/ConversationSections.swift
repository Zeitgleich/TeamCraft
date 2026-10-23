import Foundation

/// A sidebar section, as the desktop shows them.
enum SectionTitle: Hashable {
  case favorites
  case space(String)
  case channels
  case directMessages
}

struct ConversationSection: Identifiable {
  let title: SectionTitle
  let conversations: [ConversationItem]

  var id: SectionTitle { title }
}

/// The two lists of the chats screen: channels by space, and the direct messages (outside spaces).
enum ChatTab: Hashable, CaseIterable {
  case spaces, directMessages
}

extension ConversationItem {
  /// DMs and group DMs live outside spaces and have their own tab.
  var isDirect: Bool { kind == .direct || kind == .group }
}

private func byName(_ lhs: ConversationItem, _ rhs: ConversationItem) -> Bool {
  lhs.name.localizedCaseInsensitiveCompare(rhs.name) == .orderedAscending
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

/// The direct messages tab: favorites, then the rest, each the most recent first (`lastActivity`:
/// newest message per conversation; by name without one). Only names containing `filter` (any case)
/// are kept. Empty sections are left out.
func directSections(
  _ conversations: [ConversationItem], lastActivity: [String: Date] = [:], filter: String = ""
) -> [ConversationSection] {
  let needle = filter.trimmingCharacters(in: .whitespaces)
  func byRecency(_ lhs: ConversationItem, _ rhs: ConversationItem) -> Bool {
    let left = lastActivity[lhs.id] ?? .distantPast
    let right = lastActivity[rhs.id] ?? .distantPast
    return left == right ? byName(lhs, rhs) : left > right
  }
  let direct = conversations
    .filter { $0.isDirect && (needle.isEmpty || $0.name.localizedCaseInsensitiveContains(needle)) }
    .sorted(by: byRecency)
  let favorites = direct.filter(\.isFavorite)
  let rest = direct.filter { !$0.isFavorite }
  var sections: [ConversationSection] = []
  if !favorites.isEmpty { sections.append(ConversationSection(title: .favorites, conversations: favorites)) }
  if !rest.isEmpty { sections.append(ConversationSection(title: .directMessages, conversations: rest)) }
  return sections
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
