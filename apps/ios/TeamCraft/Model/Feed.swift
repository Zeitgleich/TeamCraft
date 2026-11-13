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
/// `gapsAfter` are the holes in the history right below it (issue #1438).
struct FeedEntry: Identifiable, Equatable {
  let message: Message
  let isContinuation: Bool
  var isGroupEnd = true
  var gapsAfter: [HistoryGap] = []

  var id: String { message.id }
}

/// The holes in the history among a page's messages (issues #1156, #1438), placed by the message
/// right before each, so that they stay where the core counted them; those before every message lie
/// above the first.
struct PlacedGaps: Equatable {
  var above: [HistoryGap] = []
  var after: [String: [HistoryGap]] = [:]

  init() {}

  init(_ gaps: [HistoryGap], among messages: [Message]) {
    for gap in gaps where Int(gap.index) <= messages.count {
      if gap.index == 0 {
        above.append(gap)
      } else {
        after[messages[Int(gap.index) - 1].id, default: []].append(gap)
      }
    }
  }
}

/// Which side of a hole in the history a page is read from (`GapEdge` in Rust).
enum GapEdge: String, Sendable {
  /// Its oldest messages, after the row before it: the reader reads down towards it.
  case older
  /// Its newest messages, before the row after it: the reader reads up towards it.
  case newer
}

/// The hole a feed asks a page of (issue #1438, as web and desktop do): the first, oldest first,
/// within `ahead` rows of those on screen (`firstVisible...lastVisible`, positions among the page's
/// messages); none while the conversation's last request is on its way or failed (that waits for
/// "Try again" or the next connection). A hole below the first row on screen is read from its older
/// side, the reader reading down towards it; one above it from its newer side, so what arrives
/// continues what the reader looks at.
func gapToFill(_ gaps: [HistoryGap], firstVisible: Int, lastVisible: Int, ahead: Int) -> (
  gap: HistoryGap, edge: GapEdge
)? {
  guard
    let gap = gaps.first(where: { Int($0.index) + ahead >= firstVisible && Int($0.index) <= lastVisible + ahead }),
    gap.isAskable
  else { return nil }
  return (gap, gapEdge(gap, firstVisible: firstVisible))
}

/// The side of `gap` the reader comes from, with `firstVisible` the first row on screen.
func gapEdge(_ gap: HistoryGap, firstVisible: Int) -> GapEdge {
  Int(gap.index) > firstVisible ? .older : .newer
}

/// Lays out a conversation (oldest first): one section per day, and follow-up messages of the same
/// author grouped into one block of bubbles. Messages without text (joins, renames) never group,
/// nor do two with a hole in the history between them (`gaps`).
func feedSections(_ messages: [Message], gaps: PlacedGaps = PlacedGaps(), calendar: Calendar = .current)
  -> [FeedSection]
{
  var sections: [FeedSection] = []
  var maybePrevious: Message?
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
    let gapsAfter = gaps.after[message.id] ?? []
    sections[last].entries.append(
      FeedEntry(message: message, isContinuation: isContinuation, gapsAfter: gapsAfter))
    maybePrevious = gapsAfter.isEmpty ? message : nil
  }
  return sections
}

private func continues(_ previous: Message, _ message: Message) -> Bool {
  // One bot posts for several sources.
  previous.authorId == message.authorId && previous.authorName == message.authorName
    && !previous.body.isEmpty && !message.body.isEmpty
    && (0...groupWindowMs).contains(message.sentAtMs - previous.sentAtMs)
}

extension Message {
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
    case placeholder(Message.Placeholder)
  }

  let sender: Sender
  let content: Content
  let sentDate: Date
  let messageId: String
}

/// In a one-to-one chat the other person's name is the conversation's name already, so only the
/// user's own messages get a sender ("You: …").
func chatPreview(_ message: Message, myId: String, isOneToOne: Bool) -> ChatPreview {
  let text = oneLine(message.body)
  let sender: ChatPreview.Sender =
    if text.isEmpty && message.maybePlaceholder == nil {
      .none
    } else if message.authorId == myId {
      .me
    } else if isOneToOne {
      .none
    } else {
      .other(message.shownAuthorName)
    }
  return ChatPreview(
    sender: sender, content: previewContent(message, text: text), sentDate: message.sentDate,
    messageId: message.id)
}

private func previewContent(_ message: Message, text: String) -> ChatPreview.Content {
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

/// Others typing in one place right now (#882): a conversation itself, or with `maybeThread` a reply
/// in one of its threads.
struct TypingNow: Equatable, Sendable {
  let conversation: String
  let maybeThread: String?
  let names: [String]
}

/// Who types in this place: the conversation itself, or with `thread` a reply in that thread.
func typingNames(in places: [TypingNow], conversation: String, thread: String?) -> [String] {
  places.first { $0.conversation == conversation && $0.maybeThread == thread }?.names ?? []
}

/// What the chat list's row and the header say for these names: "typing…" in a direct message (the
/// conversation is the person), else the same line as above the composer.
func typingRowText(_ names: [String], isDirect: Bool) -> LocalizedStringKey? {
  if isDirect && names.count == 1 { return "typing_direct" }
  return typingText(names)
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
