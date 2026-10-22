import Foundation

/// The filters of the search screen, written into the core's query syntax (teamcraft-client-api
/// `search`: `from:name`, `in:channel`, `has:file`, `has:link`, `after:` and `before:` with
/// `YYYY-MM-DD`). The core parses and applies them; this only builds the text.
struct SearchFilters: Equatable {
  var from = ""
  var channel = ""
  var after: Date?
  var before: Date?
  var hasFile = false
  var hasLink = false

  var isEmpty: Bool { self == SearchFilters() }

  /// How many filters are set (the badge on the filter button).
  var activeCount: Int {
    [!Self.word(from).isEmpty, !Self.word(channel).isEmpty, after != nil, before != nil, hasFile, hasLink]
      .filter { $0 }.count
  }

  /// `text` followed by the filters, as typed into the search field by hand.
  func query(for text: String) -> String {
    var parts = [text.trimmingCharacters(in: .whitespacesAndNewlines)]
    if !Self.word(from).isEmpty { parts.append("from:\(Self.word(from))") }
    if !Self.word(channel).isEmpty { parts.append("in:\(Self.word(channel))") }
    if let after { parts.append("after:\(Self.day(after))") }
    if let before { parts.append("before:\(Self.day(before))") }
    if hasFile { parts.append("has:file") }
    if hasLink { parts.append("has:link") }
    return parts.filter { !$0.isEmpty }.joined(separator: " ")
  }

  /// One token: no leading `@` or `#`, no spaces.
  static func word(_ text: String) -> String {
    text.trimmingCharacters(in: CharacterSet(charactersIn: "@#").union(.whitespacesAndNewlines))
      .components(separatedBy: .whitespacesAndNewlines).joined()
  }

  /// The calendar day the user picked (in their time zone), as `YYYY-MM-DD`.
  static func day(_ date: Date, calendar: Calendar = .current) -> String {
    let parts = calendar.dateComponents([.year, .month, .day], from: date)
    return String(format: "%04d-%02d-%02d", parts.year ?? 1970, parts.month ?? 1, parts.day ?? 1)
  }
}
