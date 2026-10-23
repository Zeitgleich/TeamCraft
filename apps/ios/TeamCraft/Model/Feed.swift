import Foundation

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
  var text = AttributedString(body)
  guard let detector = try? NSDataDetector(types: NSTextCheckingResult.CheckingType.link.rawValue) else { return text }
  let range = NSRange(body.startIndex..., in: body)
  for match in detector.matches(in: body, range: range) {
    guard let url = match.url, let scheme = url.scheme?.lowercased(), scheme == "http" || scheme == "https",
      let stringRange = Range(match.range, in: body),
      let lower = AttributedString.Index(stringRange.lowerBound, within: text),
      let upper = AttributedString.Index(stringRange.upperBound, within: text)
    else { continue }
    text[lower..<upper].link = url
  }
  return text
}
