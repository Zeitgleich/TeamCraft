import Foundation

/// Group mentions in the composer (issue #206): the core decides which groups the user may mention
/// in a conversation and expands them on sending; the app suggests them and says who they miss.

/// A composer text after a suggestion was picked, with the caret behind what was put in (a UTF-16
/// offset, as `UITextView` counts).
struct TextCompletion: Equatable {
  let text: String
  let caret: Int
}

/// The word around `caret` (a UTF-16 offset; `nil` is the end): whitespace separates words (issue #2051).
func wordRange(in text: String, caret: Int?) -> Range<String.Index> {
  let at = caretIndex(in: text, caret: caret)
  var start = at
  while start > text.startIndex, !isWordBreak(text[text.index(before: start)]) { start = text.index(before: start) }
  var end = at
  while end < text.endIndex, !isWordBreak(text[end]) { end = text.index(after: end) }
  return start..<end
}

/// The part of the word before the caret: what has been typed so far.
func typedWord(in text: String, caret: Int?) -> Substring {
  text[wordRange(in: text, caret: caret).lowerBound..<caretIndex(in: text, caret: caret)]
}

/// Puts `insert` and a space in place of the word around the caret (the text after it stays).
func replaceWord(in text: String, caret: Int?, with insert: String) -> TextCompletion {
  let range = wordRange(in: text, caret: caret)
  var tail = String(text[range.upperBound...])
  if tail.hasPrefix(" ") { tail.removeFirst() }
  let head = String(text[..<range.lowerBound]) + insert + " "
  return TextCompletion(text: head + tail, caret: head.utf16.count)
}

private func caretIndex(in text: String, caret: Int?) -> String.Index {
  guard let caret else { return text.endIndex }
  return String.Index(utf16Offset: min(max(caret, 0), text.utf16.count), in: text)
}

/// The `@word` being typed at the caret of the composer text (the end by default), without the `@`;
/// `nil` when none is.
func activeMentionQuery(_ text: String, caret: Int? = nil) -> String? {
  let word = typedWord(in: text, caret: caret)
  guard word.hasPrefix("@"), !word.dropFirst().contains("@") else { return nil }
  return String(word.dropFirst())
}

/// Members whose username, or a word of their display name, starts with `query`; at most `limit`.
/// Bots and deleted accounts are never offered (issues #654, #751). Those named in `priority`
/// (usernames, most relevant first: recent senders here, then recently mentioned) come first
/// (issue #2052); the rest keep the member order.
func personSuggestions(
  query: String, people: [PeopleSnapshot.Person], limit: Int = 6, priority: [String] = []
) -> [PeopleSnapshot.Person] {
  let needle = query.lowercased()
  let rank = Dictionary(priority.enumerated().map { ($1.lowercased(), $0) }, uniquingKeysWith: { first, _ in first })
  let found = people.filter { person in
    person.isBot != true && person.isDeleted != true
      && (person.username.lowercased().hasPrefix(needle)
        || person.displayName.lowercased().split(separator: " ").contains { $0.hasPrefix(needle) })
  }
  // `sorted` isn't stable: the index keeps the member order among those with the same rank.
  let ordered = found.enumerated().sorted { left, right in
    let (l, r) = (rank[left.element.username.lowercased()] ?? Int.max, rank[right.element.username.lowercased()] ?? Int.max)
    return l != r ? l < r : left.offset < right.offset
  }.map(\.element)
  return Array(ordered.prefix(limit))
}

/// The usernames to rank first: the authors of `messages` from the newest (not `myId`), then
/// `recentlyMentioned`.
func mentionPriority(
  messages: [Message], myId: String, people: [PeopleSnapshot.Person], recentlyMentioned: [String]
) -> [String] {
  let byId = Dictionary(people.map { ($0.id, $0.username) }, uniquingKeysWith: { first, _ in first })
  var seen = Set<String>()
  var result: [String] = []
  for username in messages.reversed().compactMap({ $0.authorId == myId ? nil : byId[$0.authorId] }) + recentlyMentioned
  where seen.insert(username.lowercased()).inserted {
    result.append(username)
  }
  return result
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

/// Replaces the `@query` being typed at the caret with `@handle ` (what the core expands on sending).
func completeMention(in text: String, handle: String, caret: Int? = nil) -> String {
  completingMention(in: text, handle: handle, caret: caret).text
}

/// Like `completeMention`, with the caret behind the inserted mention.
func completingMention(in text: String, handle: String, caret: Int? = nil) -> TextCompletion {
  replaceWord(in: text, caret: caret, with: "@\(handle)")
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

func isWordBreak(_ character: Character) -> Bool {
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

  /// How large a poll the user may send: the server's limits, else the built-in ones (#2120).
  func pollLimits() async -> PollLimitsItem {
    (try? await core().pollLimits())
      ?? PollLimitsItem(maxOptions: 12, maxQuestionChars: 300, maxOptionChars: 100, maxOpenDays: 365)
  }

  /// Whether and how large a Live Photo's motion may be sent: the server's limits, else the built-in
  /// ones (#2181). Asked before the clip is converted.
  func motionLimits() async -> MotionLimitsItem {
    (try? await core().motionLimits()) ?? MotionLimitsItem(isAllowed: true, maxBytes: 16 * 1024 * 1024)
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

/// The people the user mentioned lately, newest first, kept on this device: they rank first in
/// the suggestions (issue #2052).
enum MentionHistory {
  private static let key = "mention.recent"
  private static let limit = 20

  static var recent: [String] { UserDefaults.standard.stringArray(forKey: key) ?? [] }

  static func note(_ username: String) {
    let next = [username] + recent.filter { $0.caseInsensitiveCompare(username) != .orderedSame }
    UserDefaults.standard.set(Array(next.prefix(limit)), forKey: key)
  }
}
