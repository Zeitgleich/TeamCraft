package de.zeitgleich.teamcraft.notifications

/**
 * Pure helpers for how a message notification looks: initials and the avatar hue of a sender
 * without a picture, and the `@mentions` to highlight in a line. Unit-tested; [PersonIcons] draws.
 */
object NotificationStyling {
  /** `@name`, `@first.last`, `@channel`: what the composer inserts for a mention. */
  private val MENTION = Regex("""(?<![\p{L}\p{N}_])@[\p{L}\p{N}_.\-]*[\p{L}\p{N}_]""")

  /** Up to two initials: first and last word ("Jan Kahrs" → "JK"), else the first letter. */
  fun initials(name: String): String {
    val words = name.trim().split(Regex("""\s+""")).filter { word -> word.any { it.isLetterOrDigit() } }
    val letters = when {
      words.isEmpty() -> return "?"
      words.size == 1 -> listOf(words.first())
      else -> listOf(words.first(), words.last())
    }.map { word -> word.first { it.isLetterOrDigit() } }
    return letters.joinToString("").uppercase()
  }

  /**
   * The avatar hue of a sender, the same as in the app (teamcraft-core: the user ID modulo 360);
   * a key that is no ID (a name) gets a stable hue from its hash.
   */
  fun hue(key: String): Int = key.toLongOrNull()?.let { Math.floorMod(it, HUES).toInt() } ?: Math.floorMod(key.hashCode(), HUES.toInt())

  private const val HUES = 360L

  /** Where the `@mentions` of [text] are, to set them in bold. */
  fun mentionRanges(text: String): List<IntRange> = MENTION.findAll(text).map { it.range }.toList()
}
