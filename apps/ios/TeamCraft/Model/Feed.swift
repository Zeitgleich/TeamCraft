import SwiftUI

/// Messages of one author this close together are shown as one block (no repeated name).
let groupWindowMs: Int64 = 5 * 60 * 1000

/// One row of a conversation's message list (same layout rules as the Android app).
enum FeedItem: Identifiable, Equatable {
  /// A date chip above the first message of a day.
  case day(Date)
  /// A message; `isContinuation` when it follows the same author's previous one closely.
  case entry(MessageItem, isContinuation: Bool)

  var id: String {
    switch self {
    case .day(let date): return "day-\(Int(date.timeIntervalSince1970))"
    case .entry(let message, _): return message.id
    }
  }
}

/// Lays out a conversation (oldest first): a date chip per day and follow-up messages of the same
/// author grouped under one name. Messages without text (joins, renames) never group.
func feedItems(_ messages: [MessageItem], calendar: Calendar = .current) -> [FeedItem] {
  var items: [FeedItem] = []
  var maybePrevious: MessageItem?
  var maybeDay: Date?
  for message in messages {
    let day = calendar.startOfDay(for: message.sentDate)
    let isNewDay = day != maybeDay
    if isNewDay { items.append(.day(day)) }
    let isContinuation = !isNewDay && maybePrevious.map { continues($0, message) } == true
    items.append(.entry(message, isContinuation: isContinuation))
    maybePrevious = message
    maybeDay = day
  }
  return items
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
    if text.isEmpty {
      .none
    } else if message.authorId == myId {
      .me
    } else if isOneToOne {
      .none
    } else {
      .other(message.authorName)
    }
  return ChatPreview(
    sender: sender, content: text.isEmpty ? .noText : .text(text), sentDate: message.sentDate,
    messageId: message.id)
}

/// Text folded into one short line.
func oneLine(_ body: String, maxLength: Int = 160) -> String {
  let words = body.prefix(maxLength * 2).split(whereSeparator: \.isWhitespace)
  return String(words.joined(separator: " ").prefix(maxLength))
}

/// The body with its web links tappable. Only http(s) links: the text comes from another user.
func linkedText(_ body: String) -> AttributedString {
  messageText(body, usernames: [], myUsername: "")
}

/// Links to a message (`teamcraft:/c/<conversation>/m/<message>[/t/<thread>]`, the core's format).
private let permalinkPattern = try? NSRegularExpression(pattern: #"teamcraft:/c/\d+/m/\d+(?:/t/\d+)?"#)
/// `@word` at the start or after a non-word character (not inside e-mail addresses).
private let mentionPattern = try? NSRegularExpression(pattern: #"(?<![\w@])@([\w.\-]+)"#)
/// The core's names for "everyone here" (teamcraft-client-api `notify::CHANNEL_MENTIONS`).
private let channelMentions: Set<String> = ["channel", "here", "all"]

/// The body with tappable web links (http/https only: the text comes from another user), message
/// links (`teamcraft:` URLs the conversation opens in the app) and highlighted mentions of known
/// people (`usernames` in lower case); the user's own name and `@channel`/`@here`/`@all` stronger.
func messageText(_ body: String, usernames: Set<String>, myUsername: String) -> AttributedString {
  var text = AttributedString(body)
  let whole = NSRange(body.startIndex..., in: body)
  var linked: [Range<String.Index>] = []
  if let detector = try? NSDataDetector(types: NSTextCheckingResult.CheckingType.link.rawValue) {
    for match in detector.matches(in: body, range: whole) {
      guard let url = match.url, let scheme = url.scheme?.lowercased(), scheme == "http" || scheme == "https",
        let range = Range(match.range, in: body)
      else { continue }
      setAttributes(&text, body, range) { $0.link = url }
      linked.append(range)
    }
  }
  for match in permalinkPattern?.matches(in: body, range: whole) ?? [] {
    guard let range = Range(match.range, in: body), let url = URL(string: String(body[range])) else { continue }
    setAttributes(&text, body, range) { $0.link = url }
    linked.append(range)
  }
  for match in mentionPattern?.matches(in: body, range: whole) ?? [] {
    guard let nameRange = Range(match.range(at: 1), in: body), let atRange = Range(match.range, in: body) else {
      continue
    }
    let name = String(body[nameRange]).trimmingCharacters(in: CharacterSet(charactersIn: ".-")).lowercased()
    let range = atRange.lowerBound..<body.index(nameRange.lowerBound, offsetBy: name.count)
    guard !linked.contains(where: { $0.overlaps(range) }) else { continue }
    let isMe = channelMentions.contains(name) || (!myUsername.isEmpty && name == myUsername.lowercased())
    guard isMe || usernames.contains(name) else { continue }
    setAttributes(&text, body, range) { attributes in
      attributes.inlinePresentationIntent = .stronglyEmphasized
      attributes.foregroundColor = isMe ? Theme.mentionText : Theme.accent
      if isMe { attributes.backgroundColor = Theme.mentionBackground }
    }
  }
  return text
}

private func setAttributes(
  _ text: inout AttributedString, _ body: String, _ range: Range<String.Index>,
  _ change: (inout AttributeContainer) -> Void
) {
  guard let lower = AttributedString.Index(range.lowerBound, within: text),
    let upper = AttributedString.Index(range.upperBound, within: text)
  else { return }
  var container = AttributeContainer()
  change(&container)
  text[lower..<upper].mergeAttributes(container)
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
