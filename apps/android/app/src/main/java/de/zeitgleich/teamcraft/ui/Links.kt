package de.zeitgleich.teamcraft.ui

/** Web links in message text; scheme required, so plain words never turn into links. */
private val URL = Regex("""https?://[^\s<>"]+""", RegexOption.IGNORE_CASE)

/** Punctuation that ends a sentence rather than the link ("see https://example.com."). */
private const val TRAILING = ".,;:!?'\""

/** Where the web links in `text` are. Closing brackets stay only when the link opened one. */
fun findLinks(text: String): List<IntRange> = URL.findAll(text).map { match -> trimmed(match.value, match.range.first) }.toList()

private fun trimmed(value: String, start: Int): IntRange {
  var end = value.length
  while (end > 0) {
    val last = value[end - 1]
    val isUnbalanced = last == ')' && value.take(end).count { it == '(' } < value.take(end).count { it == ')' }
    if (last in TRAILING || isUnbalanced) end-- else break
  }
  return start until start + end
}
