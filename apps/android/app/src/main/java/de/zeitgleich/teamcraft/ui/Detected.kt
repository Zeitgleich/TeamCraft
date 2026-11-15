package de.zeitgleich.teamcraft.ui

/** What a message's text names besides web links (issues #2047, #2048): a way to act on it. */
enum class DetectedKind { WEB, PHONE, EMAIL, ADDRESS }

/** A phone number, e-mail address, postal address or web link found in a message's text. */
data class Detected(val kind: DetectedKind, val range: IntRange, val value: String) {
  /** What an app is handed to open it: `tel:`, `mailto:`, a map search or the link itself. */
  val uri: String
    get() = when (kind) {
      DetectedKind.WEB -> value
      DetectedKind.PHONE -> "tel:" + value.filter { it.isDigit() || it == '+' }
      DetectedKind.EMAIL -> "mailto:$value"
      DetectedKind.ADDRESS -> "geo:0,0?q=" + java.net.URLEncoder.encode(value, "UTF-8")
    }
}

/**
 * What kind of thing a link annotation's tag is (the [Detected.uri] it was made from), or null for
 * others (message links, mentions). The value to act on is the link's own text, or the URL itself.
 */
fun detectedKindOfUri(uri: String): DetectedKind? = when {
  uri.startsWith("http://", ignoreCase = true) || uri.startsWith("https://", ignoreCase = true) -> DetectedKind.WEB
  uri.startsWith("tel:") -> DetectedKind.PHONE
  uri.startsWith("mailto:") -> DetectedKind.EMAIL
  uri.startsWith("geo:") -> DetectedKind.ADDRESS
  else -> null
}

private val PHONE = Regex("""(?<![\w+])(?:\+|0|\()\d?[\d ()/.\-]{5,18}\d(?![\w])""")
private val EMAIL = Regex("""[\w.+\-]+@[A-Za-z0-9\-]+(?:\.[A-Za-z0-9\-]+)+""")

/** "Street 12, 12345 City" and "12 Street Name, City, ST 12345": a street, a number and a postcode. */
private val ADDRESS = Regex(
  """\p{Lu}[\p{L}.\-]+(?: [\p{L}.\-]+){0,3} \d{1,4}[a-z]?,? \d{5} \p{Lu}[\p{L}\-]+""",
)

/** A date or a time of day, which the phone pattern must not take for a number. */
private val DATE_LIKE = Regex("""\d{1,2}[./]\d{1,2}[./]\d{2,4}|\d{4}-\d{2}-\d{2}""")

/**
 * Phone numbers, e-mail addresses and postal addresses in [text], in order, never inside code (an
 * inline span or a fenced block) and never inside a web link. Web links are [findLinks]'.
 */
fun findDetected(text: String): List<Detected> {
  val plain = withoutCode(text)
  val links = findLinks(plain)
  fun isInLink(range: IntRange) = links.any { it.first <= range.last && range.first <= it.last }
  val found = mutableListOf<Detected>()
  for (match in EMAIL.findAll(plain)) {
    if (!isInLink(match.range)) found += Detected(DetectedKind.EMAIL, match.range, match.value)
  }
  for (match in ADDRESS.findAll(plain)) {
    if (!isInLink(match.range)) found += Detected(DetectedKind.ADDRESS, match.range, match.value)
  }
  for (match in PHONE.findAll(plain)) {
    val digits = match.value.count { it.isDigit() }
    val isNumber = digits in 7..15 && !DATE_LIKE.matches(match.value.trim())
    val isTaken = isInLink(match.range) || found.any { it.range.first <= match.range.last && match.range.first <= it.range.last }
    if (isNumber && !isTaken) found += Detected(DetectedKind.PHONE, match.range, match.value.trim())
  }
  return found.sortedBy { it.range.first }
}

/** Web links and everything [findDetected] finds, in order: what the message menu offers actions for. */
fun findActionable(text: String): List<Detected> {
  val plain = withoutCode(text)
  val links = findLinks(plain).map { Detected(DetectedKind.WEB, it, plain.substring(it)) }
  return (links + findDetected(text)).sortedBy { it.range.first }
}

/** [text] with fenced blocks and inline code replaced by spaces of the same length. */
internal fun withoutCode(text: String): String {
  val out = StringBuilder(text)
  for (match in FENCED.findAll(text)) blank(out, match.range)
  for (match in INLINE_CODE.findAll(out.toString())) blank(out, match.range)
  return out.toString()
}

private fun blank(out: StringBuilder, range: IntRange) {
  for (i in range) if (out[i] != '\n') out[i] = ' '
}

private val FENCED = Regex("""```[\s\S]*?(?:```|$)""")
private val INLINE_CODE = Regex("""`[^`\n]+`""")
