package de.zeitgleich.teamcraft.ui

/** Characters after `:` before the composer suggests emoji (`:) ` and `:D` stay emoticons). */
private const val MIN_QUERY_LENGTH = 2

/** The `:name` being typed at the cursor of the composer text, as in Slack; the catalog is the core's. */
object EmojiInput {
  /** The name after a `:` that starts the word before [caret], or `null` when none is being typed. */
  fun activeQuery(text: String, caret: Int = text.length): String? {
    val at = caret.coerceIn(0, text.length)
    val word = text.substring(wordBounds(text, at).first, at)
    if (!word.startsWith(":")) return null
    val name = word.drop(1)
    val isName = name.length >= MIN_QUERY_LENGTH && name.all { it.isLetterOrDigit() || it == '_' || it == '+' || it == '-' }
    return name.takeIf { isName }
  }

  /** Replaces the typed `:name` with the emoji. */
  fun complete(text: String, emoji: String, caret: Int = text.length): Completion = replaceWord(text, caret, emoji)
}
