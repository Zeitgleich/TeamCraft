package de.zeitgleich.teamcraft.ui

/** Web links in message text; scheme required, so plain words never turn into links. */
private val URL = Regex("""https?://[^\s<>"]+""", RegexOption.IGNORE_CASE)

/** Links to a message (`teamcraft:/c/<conversation>/m/<message>[/t/<thread>]`, the core's format). */
private val PERMALINK = Regex("""teamcraft:/c/\d+/m/\d+(?:/t/\d+)?""")

/** `@word` at the start of the text or after a non-word character (not inside e-mail addresses). */
private val MENTION = Regex("""(?<![\w@])@([\w.\-]+)""")

/** The core's names for "everyone here" (teamcraft-client-api `notify::CHANNEL_MENTIONS`). */
internal val CHANNEL_MENTIONS = setOf("channel", "here", "all")

/** Punctuation that ends a sentence rather than the link ("see https://example.com."). */
private const val TRAILING = ".,;:!?'\""

/** A part of a message's text that is shown differently: a link or a mention. */
sealed interface TextSpan {
  val range: IntRange

  data class WebLink(override val range: IntRange, val url: String) : TextSpan
  data class Permalink(override val range: IntRange, val link: String) : TextSpan

  /** `isMe` for the user's own name and for `@channel`/`@here`/`@all`. */
  data class Mention(override val range: IntRange, val isMe: Boolean) : TextSpan
}

/** Where the web links in `text` are. Closing brackets stay only when the link opened one. */
fun findLinks(text: String): List<IntRange> = URL.findAll(text).map { match -> trimmed(match.value, match.range.first) }.toList()

/**
 * Links, message links and mentions of `text`, in order and without overlaps. Only `@names` of
 * known people (`usernames`, lower case) and the channel-wide names count as mentions.
 */
fun textSpans(text: String, usernames: Set<String>, myUsername: String): List<TextSpan> {
  val links = findLinks(text).map { TextSpan.WebLink(it, text.substring(it)) }
  val permalinks = PERMALINK.findAll(text).map { TextSpan.Permalink(it.range, it.value) }.toList()
  val taken = (links + permalinks).map { it.range }
  val mentions = MENTION.findAll(text).mapNotNull { match ->
    val name = match.groupValues[1].trimEnd('.', '-').lowercase()
    val range = match.range.first..match.range.first + name.length
    val isChannel = name in CHANNEL_MENTIONS
    when {
      taken.any { it.first <= range.last && range.first <= it.last } -> null
      isChannel || name == myUsername.lowercase() -> TextSpan.Mention(range, isMe = true)
      name in usernames -> TextSpan.Mention(range, isMe = false)
      else -> null
    }
  }
  return (links + permalinks + mentions).sortedBy { it.range.first }
}

private fun trimmed(value: String, start: Int): IntRange {
  var end = value.length
  while (end > 0) {
    val last = value[end - 1]
    val isUnbalanced = last == ')' && value.take(end).count { it == '(' } < value.take(end).count { it == ')' }
    if (last in TRAILING || isUnbalanced) end-- else break
  }
  return start until start + end
}
