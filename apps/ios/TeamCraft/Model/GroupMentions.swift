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

private func isWordBreak(_ character: Character) -> Bool {
  character == " " || character == "\n" || character == "\t"
}

extension AppModel {
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
