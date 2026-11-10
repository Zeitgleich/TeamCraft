import Foundation

/// Group mentions in the composer (issue #206): the core decides which groups the user may mention
/// in a conversation and expands them on sending; the app suggests them and says who they miss.

/// The `@word` being typed at the end of the composer text, without the `@`; `nil` when none is.
func activeMentionQuery(_ text: String) -> String? {
  let start = text.lastIndex(where: isWordBreak).map { text.index(after: $0) } ?? text.startIndex
  let word = text[start...]
  guard word.hasPrefix("@"), !word.dropFirst().contains("@") else { return nil }
  return String(word.dropFirst())
}

/// Members whose username, or a word of their display name, starts with `query`; at most `limit`.
/// Bots and deleted accounts are never offered (issues #654, #751).
func personSuggestions(query: String, people: [PeopleSnapshot.Person], limit: Int = 6) -> [PeopleSnapshot.Person] {
  let needle = query.lowercased()
  let found = people.filter { person in
    person.isBot != true && person.isDeleted != true
      && (person.username.lowercased().hasPrefix(needle)
        || person.displayName.lowercased().split(separator: " ").contains { $0.hasPrefix(needle) })
  }
  return Array(found.prefix(limit))
}

/// Groups whose handle, or a word of the name they show with, starts with `query`; at most `limit`.
func groupSuggestions(
  query: String, groups: [MentionableGroupItem], adminsName: String, limit: Int = 4
) -> [MentionableGroupItem] {
  let needle = query.lowercased()
  let found = groups.filter { group in
    group.handle.hasPrefix(needle)
      || shownGroupName(group, adminsName: adminsName).lowercased()
        .split(separator: " ").contains { $0.hasPrefix(needle) }
  }
  return Array(found.prefix(limit))
}

/// The group's own name, or the app's translation for the built-in `@admins`.
func shownGroupName(_ group: MentionableGroupItem, adminsName: String) -> String {
  switch group.kind {
  case .custom: group.name
  case .admins: adminsName
  }
}

/// Replaces the `@query` being typed with `@handle ` (what the core expands on sending).
func completeMention(in text: String, handle: String) -> String {
  let start = text.lastIndex(where: isWordBreak).map { text.index(after: $0) } ?? text.startIndex
  return String(text[..<start]) + "@\(handle) "
}

/// What sending a message has to ask first (issue #1619), as on Android, desktop and web.
enum MentionConfirmation: Equatable {
  /// `@channel`, `@here` or `@all` in a conversation with this many members.
  case channel(memberCount: Int)
  /// Group mentions that notify this many people.
  case groups(notified: Int)

  /// The confirmation `body` needs, if any; `confirmOver` is the `@channel` setting (`0` never asks).
  /// `notified` is the group reach, read for the same text.
  static func needed(body: String, memberCount: Int, notified: Int, confirmOver: UInt32) -> MentionConfirmation? {
    if needsChannelConfirmation(body: body, memberCount: UInt32(clamping: memberCount), confirmOver: confirmOver) {
      return .channel(memberCount: memberCount)
    }
    if needsGroupConfirmation(notifiedCount: UInt32(clamping: notified), confirmOver: confirmOver) {
      return .groups(notified: notified)
    }
    return nil
  }
}

private func isWordBreak(_ character: Character) -> Bool {
  character == " " || character == "\n" || character == "\t"
}

extension AppModel {
  /// The conversation's members as the composer offers them after `@`, and how many there are.
  func mentionMembers(_ conversationId: String) async -> (people: [PeopleSnapshot.Person], count: Int) {
    guard let json = try? await core().snapshot(), let snapshot = Wire.decode(PeopleSnapshot.self, from: json),
      let ids = snapshot.conversations?.first(where: { $0.id == conversationId })?.memberIds
    else { return ([], 0) }
    let byId = Dictionary((snapshot.users ?? []).map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
    return (ids.compactMap { $0 == snapshot.me.id ? nil : byId[$0] }, ids.count)
  }

  /// The groups the composer offers here; none without `use_group_mentions`.
  func mentionableGroups(_ conversationId: String) async -> [MentionableGroupItem] {
    (try? await core().mentionableGroups(conversationId: conversationId)) ?? []
  }

  /// Who the group mentions in `body` would notify here, for the hint about members outside it.
  func groupMentionReach(_ conversationId: String, body: String) async -> GroupMentionReachItem {
    (try? await core().groupMentionReach(conversationId: conversationId, body: body))
      ?? GroupMentionReachItem(notifiedCount: 0, outsideCount: 0)
  }
}
