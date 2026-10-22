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

/// Favorites first, then the channels of each space (spaces and channels by name), then direct
/// messages. Empty sections are left out. Same grouping as the Android app.
func conversationSections(_ conversations: [ConversationItem]) -> [ConversationSection] {
  func byName(_ lhs: ConversationItem, _ rhs: ConversationItem) -> Bool {
    lhs.name.localizedCaseInsensitiveCompare(rhs.name) == .orderedAscending
  }
  let favorites = conversations.filter(\.isFavorite).sorted(by: byName)
  let rest = conversations.filter { !$0.isFavorite }
  let direct = rest.filter { $0.kind == .direct || $0.kind == .group }
  let channels = rest.filter { $0.kind != .direct && $0.kind != .group }
  let spaces = Dictionary(grouping: channels, by: \.maybeSpaceName)
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
  if !direct.isEmpty {
    sections.append(ConversationSection(title: .directMessages, conversations: direct.sorted(by: byName)))
  }
  return sections
}
