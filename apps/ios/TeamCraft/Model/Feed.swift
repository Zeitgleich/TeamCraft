import SwiftUI

/// Messages of one author this close together are shown as one block (no repeated name).
let groupWindowMs: Int64 = 5 * 60 * 1000

/// One day of a conversation's message list: its date pill floats above its messages while they
/// scroll (same grouping rules as the Android app).
struct FeedSection: Identifiable, Equatable {
  let day: Date
  var entries: [FeedEntry]

  var id: String { "day-\(Int(day.timeIntervalSince1970))" }
}

/// A message in the list. `isContinuation` when it follows the same author's previous one closely;
/// `isGroupEnd` when the next one doesn't continue it (the group's last bubble carries the avatar).
struct FeedEntry: Identifiable, Equatable {
  let message: MessageItem
  let isContinuation: Bool
  var isGroupEnd = true

  var id: String { message.id }
}

/// Lays out a conversation (oldest first): one section per day, and follow-up messages of the same
/// author grouped into one block of bubbles. Messages without text (joins, renames) never group.
func feedSections(_ messages: [MessageItem], calendar: Calendar = .current) -> [FeedSection] {
  var sections: [FeedSection] = []
  var maybePrevious: MessageItem?
  for message in messages {
    let day = calendar.startOfDay(for: message.sentDate)
    if sections.last?.day != day {
      sections.append(FeedSection(day: day, entries: []))
      maybePrevious = nil
    }
    let isContinuation = maybePrevious.map { continues($0, message) } == true
    // Invariant: a section is appended above, so `sections` is not empty.
    let last = sections.count - 1
    if isContinuation, !sections[last].entries.isEmpty {
      sections[last].entries[sections[last].entries.count - 1].isGroupEnd = false
    }
    sections[last].entries.append(FeedEntry(message: message, isContinuation: isContinuation))
    maybePrevious = message
  }
  return sections
}

private func continues(_ previous: MessageItem, _ message: MessageItem) -> Bool {
  previous.authorId == message.authorId && !previous.body.isEmpty && !message.body.isEmpty
    && (0...groupWindowMs).contains(message.sentAtMs - previous.sentAtMs)
}

extension MessageItem {
  var sentDate: Date { Date(timeIntervalSince1970: TimeInterval(sentAtMs) / 1000) }
}

/// The chat list's second line: who wrote the newest message and what.
struct ChatPreview: Equatable {
  enum Sender: Equatable {
    /// Nobody named: the other person of a direct message, or an event.
    case none
    case me
    case other(String)
  }

  enum Content: Equatable {
    case text(String)
    /// No text: a join or rename, or a deleted message (files carry their name as text).
    case noText
    /// Not readable on this device yet (issue #888).
    case placeholder(PlaceholderItem)
  }

  let sender: Sender
  let content: Content
  let sentDate: Date
  let messageId: String
}

/// In a one-to-one chat the other person's name is the conversation's name already, so only the
/// user's own messages get a sender ("You: …").
func chatPreview(_ message: MessageItem, myId: String, isOneToOne: Bool) -> ChatPreview {
  let text = oneLine(message.body)
  let sender: ChatPreview.Sender =
    if text.isEmpty && message.maybePlaceholder == nil {
      .none
    } else if message.authorId == myId {
      .me
    } else if isOneToOne {
      .none
    } else {
      .other(message.authorName)
    }
  return ChatPreview(
    sender: sender, content: previewContent(message, text: text), sentDate: message.sentDate,
    messageId: message.id)
}

private func previewContent(_ message: MessageItem, text: String) -> ChatPreview.Content {
  if let placeholder = message.maybePlaceholder { return .placeholder(placeholder) }
  return text.isEmpty ? .noText : .text(text)
}

/// Text folded into one short line.
func oneLine(_ body: String, maxLength: Int = 160) -> String {
  let words = body.prefix(maxLength * 2).split(whereSeparator: \.isWhitespace)
  return String(words.joined(separator: " ").prefix(maxLength))
}

/// How often the app tells the core that the user types; the core and the server throttle too.
let typingNotifyInterval: TimeInterval = 3

/// Whether a change of the composer text should announce typing now.
func shouldNotifyTyping(_ text: String, now: Date, lastNotified: Date?) -> Bool {
  guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return false }
  guard let last = lastNotified else { return true }
  return now.timeIntervalSince(last) >= typingNotifyInterval
}

/// The "… is typing" line for these names, or nothing when nobody types.
func typingText(_ names: [String]) -> LocalizedStringKey? {
  switch names.count {
  case 0: return nil
  case 1: return "typing_one \(names[0])"
  case 2: return "typing_two \(names[0]) \(names[1])"
  default: return "typing_several"
  }
}

/// The `:name` being typed at the end of the composer text (two characters at least, so `:)` and
/// `:D` stay emoticons), or nothing.
func activeEmojiQuery(_ text: String) -> String? {
  let word = text.split(whereSeparator: { $0 == " " || $0 == "\n" || $0 == "\t" }).last.map(String.init) ?? ""
  guard text.last.map({ !$0.isWhitespace }) ?? false, word.hasPrefix(":") else { return nil }
  let name = word.dropFirst()
  let isName = name.count >= 2 && name.allSatisfy { $0.isLetter || $0.isNumber || $0 == "_" || $0 == "+" || $0 == "-" }
  return isName ? String(name) : nil
}

/// Replaces the typed `:name` at the end with the emoji.
func completeEmoji(in text: String, with emoji: String) -> String {
  guard let start = text.lastIndex(where: { $0 == " " || $0 == "\n" || $0 == "\t" }) else { return emoji + " " }
  return String(text[...start]) + emoji + " "
}
