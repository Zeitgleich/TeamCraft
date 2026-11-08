import Foundation

/// The text rules of the core's crash report scrubber (teamcraft-client-api `crash_reporting::scrub`
/// and `debug_log::redact`, docs/crash-reports.md "Scrubbing"), for what sentry-cocoa puts into a
/// report: exception values, messages and log lines. Quoted text goes whole; then, by shape, file
/// paths, URL queries and credentials, e-mail addresses, tokens, secret `key=value` pairs and long
/// hex or base64 runs. Keep in step with the Rust rules and the Android app's `CrashScrubber.kt`.
enum CrashScrubber {
  /// Longest exception message kept, in characters.
  static let maxMessageChars = 512
  /// Longest breadcrumb kept, in characters.
  static let maxBreadcrumbChars = 512
  /// Breadcrumbs kept; the newest.
  static let maxBreadcrumbs = 50

  private static let redacted = "[redacted]"
  private static let secretRunChars = 24
  private static let secretFields: Set<String> = [
    "authorization", "body", "caption", "code", "comment", "content", "cookie", "description",
    "draft", "email", "file_name", "filename", "key", "label", "name", "note", "passphrase",
    "password", "path", "payload", "preview", "query", "quote", "secret", "signature", "subject",
    "summary", "text", "title", "token", "topic", "url",
  ]
  private static let secretFieldSuffixes = [
    "_body", "_code", "_email", "_key", "_name", "_passphrase", "_password", "_payload", "_secret",
    "_text", "_token", "_url",
  ]
  private static let wrap: Set<Character> = ["\"", "'", "(", ")", "[", "]", "{", "}", ",", ";", "<", ">", "`"]
  private static let runSymbols: Set<Character> = ["+", "/", "=", "_", "-"]

  /// Free text cut to `maxChars`: quoted values go whole, then the redaction by shape.
  static func scrub(_ text: String, maxChars: Int = maxMessageChars) -> String {
    truncate(redact(removeQuoted(text)), maxChars: maxChars)
  }

  /// `"…"` becomes `"[redacted]"`; an unclosed quote runs to the end. Escaped quotes are skipped.
  static func removeQuoted(_ text: String) -> String {
    var out = ""
    var isInQuote = false
    var isEscaped = false
    for character in text {
      if !isInQuote {
        out.append(character)
        if character == "\"" {
          out += redacted
          isInQuote = true
        }
      } else if isEscaped {
        isEscaped = false
      } else if character == "\\" {
        isEscaped = true
      } else if character == "\"" {
        out.append(character)
        isInQuote = false
      }
    }
    return out
  }

  /// The redaction of the core's debug log, word by word; a path takes the rest of the text.
  static func redact(_ text: String) -> String {
    var out = ""
    var redactsNext = false
    for piece in splitKeepingWhitespace(text) {
      if piece.allSatisfy(\.isWhitespace) {
        out += piece
        continue
      }
      if redactsNext {
        // The credentials after an authorization scheme.
        redactsNext = false
        out += redacted + splitPunctuation(piece).trail
        continue
      }
      let bare = piece.trimmingCharacters(in: CharacterSet.alphanumerics.inverted).lowercased()
      if bare == "bearer" || bare == "basic" {
        out += piece
        redactsNext = true
        continue
      }
      let (lead, core, _) = splitPunctuation(piece)
      if startsPath(core) {
        // A path may hold spaces (`/Users/x/My Documents/a b.pdf`): everything after it goes too.
        out += lead + "[path]"
        break
      }
      out += redactWord(piece)
    }
    return out
  }

  private static func redactWord(_ word: String) -> String {
    if word.contains("://") { return redactURL(word) }
    let (lead, core, trail) = splitPunctuation(word)
    if core.isEmpty { return word }
    if looksLikeEmail(core) { return lead + "[email]" + trail }
    if looksLikeJWT(core) { return lead + redacted + trail }
    if let pair = secretPair(core) { return lead + pair + trail }
    return lead + redactSecretRuns(core) + trail
  }

  /// `token=abc` or `password:abc` with a secret key: the key stays, the value goes.
  private static func secretPair(_ core: String) -> String? {
    guard let separator = core.firstIndex(where: { $0 == "=" || $0 == ":" }), separator != core.startIndex,
      core.index(after: separator) != core.endIndex
    else { return nil }
    let name = String(core[..<separator])
    let key = name.trimmingCharacters(
      in: CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "_")).inverted)
    guard isSecretField(key) else { return nil }
    return name + String(core[separator]) + redacted
  }

  /// Keeps scheme, host and path; user info, query and fragment go. File and content URLs go whole.
  private static func redactURL(_ word: String) -> String {
    guard let schemeRange = word.range(of: "://") else { return word }
    let schemeStart =
      word[..<schemeRange.lowerBound].lastIndex(where: { !($0.isASCII && ($0.isLetter || $0.isNumber)) })
      .map { word.index(after: $0) } ?? word.startIndex
    let scheme = word[schemeStart..<schemeRange.lowerBound]
    if scheme == "file" || scheme == "content" { return String(word[..<schemeStart]) + "[path]" }
    let trail = splitPunctuation(word).trail
    var kept = String(word.dropLast(trail.count))
    guard let keptScheme = kept.range(of: "://") else { return word }
    let authorityStart = kept.distance(from: kept.startIndex, to: keptScheme.upperBound)
    if let cut = kept[keptScheme.upperBound...].firstIndex(where: { $0 == "?" || $0 == "#" }) {
      kept = String(kept[..<cut]) + "?" + redacted
    }
    let start = kept.index(kept.startIndex, offsetBy: authorityStart)
    let authorityEnd = kept[start...].firstIndex(of: "/") ?? kept.endIndex
    if let at = kept[start..<authorityEnd].lastIndex(of: "@") {
      kept = String(kept[..<start]) + redacted + "@" + String(kept[kept.index(after: at)...])
    }
    return kept + trail
  }

  /// Runs of hex or base64 long enough to be a key, hash or token. Decimal IDs stay.
  private static func redactSecretRuns(_ word: String) -> String {
    var out = ""
    var run = ""
    func flush() {
      let isSecret =
        run.count >= secretRunChars && run.contains(where: \.isNumber)
        && run.contains(where: \.isLetter) && !isRoute(run)
      out += isSecret ? redacted : run
      run = ""
    }
    for character in word {
      if character.isASCII && (character.isLetter || character.isNumber || runSymbols.contains(character)) {
        run.append(character)
      } else {
        flush()
        out.append(character)
      }
    }
    flush()
    return out
  }

  /// `/api/v1/conversations/7346/messages`: route segments, not base64.
  private static func isRoute(_ run: String) -> Bool {
    run.contains("/")
      && run.split(separator: "/", omittingEmptySubsequences: false).allSatisfy { segment in
        segment.count <= 3 || segment.allSatisfy(\.isNumber)
          || segment.allSatisfy { ($0.isLetter && $0.isLowercase) || $0 == "_" || $0 == "-" }
      }
  }

  private static func startsPath(_ core: String) -> Bool {
    if core.hasPrefix("~/") || core.hasPrefix("~\\") || core.hasPrefix("\\\\") { return true }
    let characters = Array(core)
    let isWindows = characters.count > 3 && characters[0].isLetter && characters[1] == ":" && characters[2] == "\\"
    let isAbsolute =
      characters.count > 1 && characters[0] == "/" && characters.dropFirst().contains("/")
      && (characters[1].isLetter || characters[1].isNumber || characters[1] == "." || characters[1] == "_")
      && !core.hasPrefix("/api/")
    return isWindows || isAbsolute
  }

  private static func isSecretField(_ name: String) -> Bool {
    let lower = name.lowercased()
    return secretFields.contains(lower) || secretFieldSuffixes.contains { lower.hasSuffix($0) }
  }

  private static func looksLikeEmail(_ word: String) -> Bool {
    guard let at = word.firstIndex(of: "@"), at != word.startIndex else { return false }
    let domain = word[word.index(after: at)...]
    return domain.contains(".") && !domain.hasPrefix(".") && !domain.hasSuffix(".")
  }

  private static func looksLikeJWT(_ word: String) -> Bool {
    word.hasPrefix("eyJ") && word.filter { $0 == "." }.count == 2
  }

  private static func splitKeepingWhitespace(_ text: String) -> [String] {
    var pieces: [String] = []
    var current = ""
    for character in text {
      if let last = current.last, last.isWhitespace != character.isWhitespace {
        pieces.append(current)
        current = ""
      }
      current.append(character)
    }
    if !current.isEmpty { pieces.append(current) }
    return pieces
  }

  /// Quotes and brackets around a word, and trailing dots.
  private static func splitPunctuation(_ word: String) -> (lead: String, core: String, trail: String) {
    let characters = Array(word)
    var start = 0
    while start < characters.count && wrap.contains(characters[start]) { start += 1 }
    var end = characters.count
    while end > start && (wrap.contains(characters[end - 1]) || characters[end - 1] == ".") { end -= 1 }
    return (String(characters[..<start]), String(characters[start..<end]), String(characters[end...]))
  }

  private static func truncate(_ text: String, maxChars: Int) -> String {
    text.count <= maxChars ? text : String(text.prefix(maxChars)) + "…"
  }
}
