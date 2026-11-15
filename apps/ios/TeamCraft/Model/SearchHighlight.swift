import Foundation

/// A search hit's text with the searched words emphasised, as the web's `<mark>` does (#2003).
enum SearchHighlight {
  static func attributed(_ body: String, terms: [String]) -> AttributedString {
    var result = AttributedString(body)
    for term in terms where !term.isEmpty {
      var searchRange = body.startIndex..<body.endIndex
      while let found = body.range(of: term, options: [.caseInsensitive, .diacriticInsensitive], range: searchRange) {
        if let range = Range(found, in: result) {
          result[range].inlinePresentationIntent = .stronglyEmphasized
          result[range].foregroundColor = Theme.accent
        }
        searchRange = found.upperBound..<body.endIndex
      }
    }
    return result
  }
}
