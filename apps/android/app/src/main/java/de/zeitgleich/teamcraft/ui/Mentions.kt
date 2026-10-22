package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.core.User

/** The `@name` being typed at the end of the composer text, and what picking a person does. */
object Mentions {
  /** The partial name after an `@` that starts the last word, or `null` when none is being typed. */
  fun activeQuery(text: String): String? {
    val start = text.lastIndexOfAny(charArrayOf(' ', '\n', '\t')) + 1
    val word = text.substring(start)
    return if (word.startsWith("@") && word.none { it == '@' && word.indexOf(it) > 0 }) word.drop(1) else null
  }

  /** Members whose username or display name starts with the query, at most `limit`. */
  fun suggestions(query: String, people: List<User>, limit: Int = 6): List<User> =
    people
      .filter { !it.isBot }
      .filter {
        it.username.startsWith(query, ignoreCase = true) ||
          it.displayName.split(' ').any { part -> part.startsWith(query, ignoreCase = true) }
      }
      .take(limit)

  /** Replaces the typed `@query` with the person's `@username` (what the core reads as a mention). */
  fun complete(text: String, user: User): String {
    val start = text.lastIndexOfAny(charArrayOf(' ', '\n', '\t')) + 1
    return text.substring(0, start) + "@" + user.username + " "
  }
}
