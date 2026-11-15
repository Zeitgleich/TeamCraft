import Foundation

/// What a message's text names besides web links (issues #2047, #2048): something to act on.
enum DetectedKind: Equatable, Sendable {
  case web, phone, email, address
}

/// A web link, phone number, e-mail address or postal address found in a message's text.
struct DetectedItem: Equatable, Sendable {
  let kind: DetectedKind
  /// Where it is in the text, in UTF-16 units (what `NSRange` and `AttributedString` conversions use).
  let range: NSRange
  let value: String

  /// What an app is handed to open it: `tel:`, `mailto:`, a map search or the link itself.
  var url: URL? {
    switch kind {
    case .web: URL(string: value)
    case .phone: URL(string: "tel:" + value.filter { $0.isNumber || $0 == "+" })
    case .email: URL(string: "mailto:" + value)
    case .address:
      value.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed).flatMap { URL(string: "https://maps.apple.com/?q=" + $0) }
    }
  }
}

private let webPattern = try? NSRegularExpression(pattern: #"https?://[^\s<>"]+"#, options: [.caseInsensitive])
private let emailPattern = try? NSRegularExpression(pattern: #"[\w.+\-]+@[A-Za-z0-9\-]+(?:\.[A-Za-z0-9\-]+)+"#)
private let phonePattern = try? NSRegularExpression(pattern: #"(?<![\w+])(?:\+|0|\()\d?[\d ()/.\-]{5,18}\d(?!\w)"#)
/// "Street 12, 12345 City": a street, a number and a postcode.
private let addressPattern = try? NSRegularExpression(
  pattern: #"\p{Lu}[\p{L}.\-]+(?: [\p{L}.\-]+){0,3} \d{1,4}[a-z]?,? \d{5} \p{Lu}[\p{L}\-]+"#)
/// A date, which the phone pattern must not take for a number.
private let datePattern = try? NSRegularExpression(pattern: #"^(?:\d{1,2}[./]\d{1,2}[./]\d{2,4}|\d{4}-\d{2}-\d{2})$"#)

/// Characters that end a sentence rather than a link ("see https://example.com.").
private let trailingPunctuation = CharacterSet(charactersIn: ".,;:!?'\"")

/// Phone numbers, e-mail addresses and postal addresses in `text`, in order, never inside code (an
/// inline span or a fenced block) and never inside a web link.
func findDetected(_ text: String) -> [DetectedItem] {
  let plain = withoutCode(text)
  let links = webRanges(in: plain)
  func isInLink(_ range: NSRange) -> Bool { links.contains { NSIntersectionRange($0, range).length > 0 } }
  var found: [DetectedItem] = []
  for item in matches(emailPattern, in: plain, kind: .email) where !isInLink(item.range) { found.append(item) }
  for item in matches(addressPattern, in: plain, kind: .address) where !isInLink(item.range) { found.append(item) }
  for item in matches(phonePattern, in: plain, kind: .phone) {
    let digits = item.value.filter(\.isNumber).count
    let value = item.value.trimmingCharacters(in: .whitespaces)
    let isDate = datePattern?.firstMatch(in: value, range: NSRange(value.startIndex..., in: value)) != nil
    let isTaken = isInLink(item.range) || found.contains { NSIntersectionRange($0.range, item.range).length > 0 }
    if (7...15).contains(digits), !isDate, !isTaken {
      found.append(DetectedItem(kind: .phone, range: item.range, value: value))
    }
  }
  return found.sorted { $0.range.location < $1.range.location }
}

/// Web links and everything `findDetected` finds, in order: what the message menu offers actions for.
func findActionable(_ text: String) -> [DetectedItem] {
  let plain = withoutCode(text)
  let ns = plain as NSString
  let links = webRanges(in: plain).map { DetectedItem(kind: .web, range: $0, value: ns.substring(with: $0)) }
  return (links + findDetected(text)).sorted { $0.range.location < $1.range.location }
}

/// `text` with fenced blocks and inline code replaced by spaces of the same length.
func withoutCode(_ text: String) -> String {
  var units = Array(text.utf16)
  func blank(_ range: NSRange) {
    for index in range.location..<(range.location + range.length) where units[index] != 10 { units[index] = 32 }
  }
  let fenced = try? NSRegularExpression(pattern: #"```[\s\S]*?(?:```|$)"#)
  for match in fenced?.matches(in: text, range: NSRange(text.startIndex..., in: text)) ?? [] { blank(match.range) }
  let afterFences = String(decoding: units, as: UTF16.self)
  let inline = try? NSRegularExpression(pattern: #"`[^`\n]+`"#)
  for match in inline?.matches(in: afterFences, range: NSRange(afterFences.startIndex..., in: afterFences)) ?? [] {
    blank(match.range)
  }
  return String(decoding: units, as: UTF16.self)
}

private func webRanges(in text: String) -> [NSRange] {
  let ns = text as NSString
  return (webPattern?.matches(in: text, range: NSRange(location: 0, length: ns.length)) ?? []).map { match in
    var range = match.range
    // Closing brackets stay only when the link opened one.
    while range.length > 0 {
      let last = ns.substring(with: NSRange(location: range.location + range.length - 1, length: 1))
      let value = ns.substring(with: range)
      let isUnbalanced = last == ")" && value.filter { $0 == "(" }.count < value.filter { $0 == ")" }.count
      let isPunctuation = last.unicodeScalars.allSatisfy { trailingPunctuation.contains($0) }
      if isPunctuation || isUnbalanced { range.length -= 1 } else { break }
    }
    return range
  }
}

private func matches(_ expression: NSRegularExpression?, in text: String, kind: DetectedKind) -> [DetectedItem] {
  let ns = text as NSString
  return (expression?.matches(in: text, range: NSRange(location: 0, length: ns.length)) ?? []).map {
    DetectedItem(kind: kind, range: $0.range, value: ns.substring(with: $0.range))
  }
}

/// At most `limit` items, one for each value, in order.
func uniqueDetected(_ items: [DetectedItem], limit: Int = 3) -> [DetectedItem] {
  var seen = Set<String>()
  return Array(items.filter { seen.insert($0.value).inserted }.prefix(limit))
}

/// A value as a menu row shows it; the whole value is what is opened or copied.
func shortenedValue(_ value: String, limit: Int = 32) -> String {
  value.count <= limit ? value : String(value.prefix(limit - 1)) + "…"
}
