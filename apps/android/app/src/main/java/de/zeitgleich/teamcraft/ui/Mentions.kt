package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.core.User
import uniffi.teamcraft_ffi.MentionableGroupItem
import uniffi.teamcraft_ffi.MentionableGroupKindItem

/** The `@name` being typed at the end of the composer text, and what picking a person does. */
object Mentions {
  /** The partial name after an `@` that starts the last word, or `null` when none is being typed. */
  fun activeQuery(text: String): String? {
    val start = text.lastIndexOfAny(charArrayOf(' ', '\n', '\t')) + 1
    val word = text.substring(start)
    return if (word.startsWith("@") && word.none { it == '@' && word.indexOf(it) > 0 }) word.drop(1) else null
  }

  /**
   * Members whose username or display name starts with the query, at most `limit`. Deactivated and
   * deleted accounts are never offered (issues #654, #751).
   */
  fun suggestions(query: String, people: List<User>, limit: Int = 6): List<User> =
    people
      .filter { !it.isBot && it.isOffered }
      .filter {
        it.username.startsWith(query, ignoreCase = true) ||
          it.displayName.split(' ').any { part -> part.startsWith(query, ignoreCase = true) }
      }
      .take(limit)

  /**
   * Groups whose handle or name starts with the query, at most `limit` (issue #206). The core only
   * offers those the user may mention here; [adminsName] is the app's name for the built-in `@admins`.
   */
  fun groupSuggestions(
    query: String,
    groups: List<MentionableGroupItem>,
    adminsName: String,
    limit: Int = 4,
  ): List<MentionableGroupItem> =
    groups
      .filter {
        it.handle.startsWith(query, ignoreCase = true) ||
          shownName(it, adminsName).split(' ').any { part -> part.startsWith(query, ignoreCase = true) }
      }
      .take(limit)

  /** The name a group shows with: its own, or the app's translation for a built-in one. */
  fun shownName(group: MentionableGroupItem, adminsName: String): String = when (group.kind) {
    MentionableGroupKindItem.CUSTOM -> group.name
    MentionableGroupKindItem.ADMINS -> adminsName
  }

  /** Replaces the typed `@query` with the person's `@username` (what the core reads as a mention). */
  fun complete(text: String, user: User): String = complete(text, user.username)

  /** Replaces the typed `@query` with `@handle` (a group's, which the core expands on sending). */
  fun complete(text: String, handle: String): String {
    val start = text.lastIndexOfAny(charArrayOf(' ', '\n', '\t')) + 1
    return text.substring(0, start) + "@" + handle + " "
  }
}
