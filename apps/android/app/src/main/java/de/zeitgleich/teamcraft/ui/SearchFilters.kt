package de.zeitgleich.teamcraft.ui

import java.time.LocalDate

/**
 * Builds and removes the filter tokens of a search query as plain text. The core parses them
 * (teamcraft-client-api `search`): `from:name`, `in:channel`, `has:file`, `has:link`,
 * `before:YYYY-MM-DD`, `after:YYYY-MM-DD` and `in-thread`. Values with spaces are quoted, as the
 * core's tokenizer keeps `"quoted phrases"` together.
 */
object SearchFilters {
  const val HAS_FILE = "has:file"
  const val HAS_LINK = "has:link"
  const val IN_THREAD = "in-thread"

  private const val MS_PER_DAY = 86_400_000L
  private val FILTER_KEYS = setOf("from", "in", "has", "before", "after")

  /** Keys that make sense once per query: a second `from:` or `before:` replaces the first. */
  private val SINGLE_KEYS = setOf("from", "in", "before", "after")

  fun from(username: String): String? = token("from", username)

  fun inConversation(name: String): String? = token("in", name)

  fun before(date: LocalDate): String = "before:$date"

  fun after(date: LocalDate): String = "after:$date"

  /** A date picker's selection (UTC midnight in Unix milliseconds) as a calendar day. */
  fun dayOf(utcMillis: Long): LocalDate = LocalDate.ofEpochDay(Math.floorDiv(utcMillis, MS_PER_DAY))

  /** `key:value`, quoted when the value has spaces; `null` when nothing usable is left. */
  fun token(key: String, value: String): String? {
    val clean = value.replace("\"", "").trim().trimStart('@', '#').trim()
    if (clean.isEmpty()) return null
    return if (clean.any { it.isWhitespace() }) "$key:\"$clean\"" else "$key:$clean"
  }

  /** The query with [token] appended; replaces an earlier token of a single-use key. */
  fun add(query: String, token: String): String {
    val maybeKey = keyOf(token)
    val kept = tokens(query).filterNot { existing ->
      existing.equals(token, ignoreCase = true) ||
        (maybeKey != null && maybeKey in SINGLE_KEYS && keyOf(existing) == maybeKey)
    }
    return (kept + token).joinToString(" ")
  }

  /** The query without every occurrence of [token]. */
  fun remove(query: String, token: String): String =
    tokens(query).filterNot { it.equals(token, ignoreCase = true) }.joinToString(" ")

  /** Whether [query] contains [token] as a whole token. */
  fun contains(query: String, token: String): Boolean = tokens(query).any { it.equals(token, ignoreCase = true) }

  /** The filter tokens of [query], in order, for showing them as removable chips. */
  fun active(query: String): List<String> = tokens(query).filter { isFilter(it) }

  /** Splits on whitespace but keeps `"quoted phrases"` together, like the core's tokenizer. */
  fun tokens(query: String): List<String> {
    val tokens = mutableListOf<String>()
    val current = StringBuilder()
    var isInQuotes = false
    for (char in query) {
      when {
        char == '"' -> {
          isInQuotes = !isInQuotes
          current.append(char)
        }
        char.isWhitespace() && !isInQuotes -> {
          if (current.isNotEmpty()) {
            tokens += current.toString()
            current.clear()
          }
        }
        else -> current.append(char)
      }
    }
    if (current.isNotEmpty()) tokens += current.toString()
    return tokens
  }

  private fun isFilter(token: String): Boolean = token.equals(IN_THREAD, ignoreCase = true) || keyOf(token) != null

  private fun keyOf(token: String): String? =
    token.substringBefore(':', missingDelimiterValue = "").lowercase().takeIf { it in FILTER_KEYS }
}
