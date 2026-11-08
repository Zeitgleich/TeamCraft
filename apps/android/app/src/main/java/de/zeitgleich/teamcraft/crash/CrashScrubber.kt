package de.zeitgleich.teamcraft.crash

/**
 * The text rules of the core's crash report scrubber (teamcraft-client-api `crash_reporting::scrub`
 * and `debug_log::redact`, docs/crash-reports.md "Scrubbing"), for what the platform SDK puts into
 * a report: exception messages and log lines. Quoted text goes whole; then, by shape, file paths,
 * URL queries and credentials, e-mail addresses, tokens, secret `key=value` pairs and long hex or
 * base64 runs. Keep in step with the Rust rules and with the iOS app's `CrashScrubber.swift`.
 */
object CrashScrubber {
  /** Longest exception message kept, in characters. */
  const val MAX_MESSAGE_CHARS = 512

  /** Longest breadcrumb kept, in characters. */
  const val MAX_BREADCRUMB_CHARS = 512

  /** Breadcrumbs kept; the newest. */
  const val MAX_BREADCRUMBS = 50

  private const val REDACTED = "[redacted]"
  private const val SECRET_RUN_CHARS = 24

  private val SECRET_FIELDS = setOf(
    "authorization", "body", "caption", "code", "comment", "content", "cookie", "description",
    "draft", "email", "file_name", "filename", "key", "label", "name", "note", "passphrase",
    "password", "path", "payload", "preview", "query", "quote", "secret", "signature", "subject",
    "summary", "text", "title", "token", "topic", "url",
  )
  private val SECRET_FIELD_SUFFIXES = listOf(
    "_body", "_code", "_email", "_key", "_name", "_passphrase", "_password", "_payload", "_secret",
    "_text", "_token", "_url",
  )
  private val WRAP = setOf('"', '\'', '(', ')', '[', ']', '{', '}', ',', ';', '<', '>', '`')

  /** Free text cut to [maxChars]: quoted values go whole, then the redaction by shape. */
  fun scrub(text: String, maxChars: Int = MAX_MESSAGE_CHARS): String = truncate(redact(removeQuoted(text)), maxChars)

  /** `"…"` becomes `"[redacted]"`; an unclosed quote runs to the end. Escaped quotes are skipped. */
  fun removeQuoted(text: String): String {
    val out = StringBuilder(text.length)
    var index = 0
    while (index < text.length) {
      val open = text.indexOf('"', index)
      if (open < 0) {
        out.append(text, index, text.length)
        break
      }
      out.append(text, index, open + 1).append(REDACTED)
      val close = closingQuote(text, open + 1)
      if (close < 0) return out.toString()
      out.append('"')
      index = close + 1
    }
    return out.toString()
  }

  /** The redaction of the core's debug log, word by word; a path takes the rest of the text. */
  fun redact(text: String): String {
    val out = StringBuilder(text.length)
    var redactsNext = false
    for (piece in splitKeepWhitespace(text)) {
      if (piece.all(Char::isWhitespace)) {
        out.append(piece)
        continue
      }
      if (redactsNext) {
        // The credentials after an authorization scheme.
        redactsNext = false
        out.append(REDACTED).append(splitPunctuation(piece).third)
        continue
      }
      val bare = piece.trim { !it.isLetterOrDigit() }
      if (bare.equals("bearer", ignoreCase = true) || bare.equals("basic", ignoreCase = true)) {
        out.append(piece)
        redactsNext = true
        continue
      }
      val (lead, core, _) = splitPunctuation(piece)
      if (startsPath(core)) {
        // A path may hold spaces (`/home/x/My Documents/a b.pdf`): everything after it goes too.
        out.append(lead).append("[path]")
        break
      }
      out.append(redactWord(piece))
    }
    return out.toString()
  }

  private fun closingQuote(text: String, from: Int): Int {
    var isEscaped = false
    for (index in from until text.length) {
      val c = text[index]
      when {
        c == '\\' && !isEscaped -> isEscaped = true
        c == '"' && !isEscaped -> return index
        else -> isEscaped = false
      }
    }
    return -1
  }

  private fun redactWord(word: String): String {
    val schemeAt = word.indexOf("://")
    if (schemeAt >= 0) return redactUrl(word, schemeAt)
    val (lead, core, trail) = splitPunctuation(word)
    if (core.isEmpty()) return word
    val maybeSecretPair = secretPair(core)
    return when {
      looksLikeEmail(core) -> "$lead[email]$trail"
      looksLikeJwt(core) -> "$lead$REDACTED$trail"
      maybeSecretPair != null -> "$lead$maybeSecretPair$trail"
      else -> "$lead${redactSecretRuns(core)}$trail"
    }
  }

  /** `token=abc` or `password:abc` with a secret key: the key stays, the value goes. */
  private fun secretPair(core: String): String? {
    val separatorAt = core.indexOfFirst { it == '=' || it == ':' }
    if (separatorAt <= 0 || separatorAt == core.length - 1) return null
    val name = core.substring(0, separatorAt)
    if (!isSecretField(name.trim { !it.isLetterOrDigit() && it != '_' })) return null
    return "$name${core[separatorAt]}$REDACTED"
  }

  /** Keeps scheme, host and path; user info, query and fragment go. File and content URLs go whole. */
  private fun redactUrl(word: String, schemeAt: Int): String {
    val schemeStart = word.substring(0, schemeAt).indexOfLast { !it.isLetterOrDigit() } + 1
    val scheme = word.substring(schemeStart, schemeAt)
    if (scheme == "file" || scheme == "content") return word.substring(0, schemeStart) + "[path]"
    val trail = splitPunctuation(word).third
    var kept = word.substring(0, word.length - trail.length)
    val authorityStart = schemeAt + 3
    val cut = kept.indexOfAny(charArrayOf('?', '#'), authorityStart)
    if (cut >= 0) kept = kept.substring(0, cut) + "?$REDACTED"
    val slash = kept.indexOf('/', authorityStart)
    val authorityEnd = if (slash < 0) kept.length else slash
    val at = kept.lastIndexOf('@', authorityEnd - 1)
    if (at >= authorityStart) kept = kept.substring(0, authorityStart) + "$REDACTED@" + kept.substring(at + 1)
    return kept + trail
  }

  /** Runs of hex or base64 long enough to be a key, hash or token. Decimal IDs stay. */
  private fun redactSecretRuns(word: String): String {
    val out = StringBuilder(word.length)
    val run = StringBuilder()
    fun flush() {
      val isSecret = run.length >= SECRET_RUN_CHARS && run.any(Char::isDigit) &&
        run.any { it in 'a'..'z' || it in 'A'..'Z' } && !isRoute(run.toString())
      out.append(if (isSecret) REDACTED else run)
      run.clear()
    }
    for (c in word) {
      if (c.isAsciiAlphanumeric() || c in "+/=_-") {
        run.append(c)
      } else {
        flush()
        out.append(c)
      }
    }
    flush()
    return out.toString()
  }

  /** `/api/v1/conversations/7346/messages`: route segments, not base64. */
  private fun isRoute(run: String): Boolean =
    run.contains('/') && run.split('/').all { segment ->
      segment.length <= 3 || segment.all(Char::isDigit) || segment.all { it in 'a'..'z' || it == '_' || it == '-' }
    }

  private fun startsPath(core: String): Boolean {
    if (core.startsWith("~/") || core.startsWith("~\\") || core.startsWith("\\\\")) return true
    val isWindows = core.length > 3 && core[0].isLetter() && core[1] == ':' && core[2] == '\\'
    val isAbsolute = core.startsWith('/') && core.length > 1 && core.indexOf('/', 1) > 0 &&
      (core[1].isLetterOrDigit() || core[1] == '.' || core[1] == '_') && !core.startsWith("/api/")
    return isWindows || isAbsolute
  }

  private fun isSecretField(name: String): Boolean {
    val lower = name.lowercase()
    return lower in SECRET_FIELDS || SECRET_FIELD_SUFFIXES.any { lower.endsWith(it) }
  }

  private fun looksLikeEmail(word: String): Boolean {
    val at = word.indexOf('@')
    if (at <= 0) return false
    val domain = word.substring(at + 1)
    return domain.contains('.') && !domain.startsWith('.') && !domain.endsWith('.')
  }

  private fun looksLikeJwt(word: String): Boolean = word.startsWith("eyJ") && word.count { it == '.' } == 2

  private fun splitKeepWhitespace(text: String): List<String> {
    val pieces = mutableListOf<String>()
    var start = 0
    for (index in 1 until text.length) {
      if (text[index].isWhitespace() != text[index - 1].isWhitespace()) {
        pieces += text.substring(start, index)
        start = index
      }
    }
    if (start < text.length) pieces += text.substring(start)
    return pieces
  }

  /** Quotes and brackets around a word, and trailing dots: lead, core, trail. */
  private fun splitPunctuation(word: String): Triple<String, String, String> {
    val coreStart = word.indexOfFirst { it !in WRAP }.let { if (it < 0) word.length else it }
    val rest = word.substring(coreStart)
    val coreEnd = rest.indexOfLast { it !in WRAP && it != '.' } + 1
    return Triple(word.substring(0, coreStart), rest.substring(0, coreEnd), rest.substring(coreEnd))
  }

  private fun truncate(text: String, maxChars: Int): String =
    if (text.codePointCount(0, text.length) <= maxChars) {
      text
    } else {
      text.substring(0, text.offsetByCodePoints(0, maxChars)) + "…"
    }

  private fun Char.isAsciiAlphanumeric(): Boolean = this in 'a'..'z' || this in 'A'..'Z' || this in '0'..'9'
}
