package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.core.Message
import de.zeitgleich.teamcraft.core.User
import uniffi.teamcraft_ffi.MentionableGroupItem
import uniffi.teamcraft_ffi.MentionableGroupKindItem

/** A composer text after a suggestion was picked, with the caret behind what was put in. */
data class Completion(val text: String, val caret: Int)

/** Where the word around [caret] starts and ends: whitespace separates words (issue #2051). */
internal fun wordBounds(text: String, caret: Int): IntRange {
  val at = caret.coerceIn(0, text.length)
  var start = at
  while (start > 0 && !text[start - 1].isWhitespace()) start--
  var end = at
  while (end < text.length && !text[end].isWhitespace()) end++
  return start until end
}

/** Puts [insert] and a space in place of the word around [caret] (the text after it stays). */
internal fun replaceWord(text: String, caret: Int, insert: String): Completion {
  val bounds = wordBounds(text, caret)
  val head = text.substring(0, bounds.first) + insert + " "
  val tail = text.substring(bounds.last + 1).removePrefix(" ")
  return Completion(head + tail, head.length)
}

/** The `@name` being typed at the cursor of the composer text, and what picking a person does. */
object Mentions {
  /**
   * The partial name after an `@` that starts the word before [caret], or `null` when none is
   * being typed. The word's end after the caret is not part of the query.
   */
  fun activeQuery(text: String, caret: Int = text.length): String? {
    val at = caret.coerceIn(0, text.length)
    val word = text.substring(wordBounds(text, at).first, at)
    return if (word.startsWith("@") && word.none { it == '@' && word.indexOf(it) > 0 }) word.drop(1) else null
  }

  /**
   * Members whose username or display name starts with the query, at most `limit`. Deactivated and
   * deleted accounts are never offered (issues #654, #751). People named in [priority] (usernames,
   * most relevant first: recent senders here, then recently mentioned) come first (issue #2052);
   * the rest keep the member order.
   */
  fun suggestions(query: String, people: List<User>, limit: Int = 6, priority: List<String> = emptyList()): List<User> {
    val rank = priority.map { it.lowercase() }.withIndex().associate { (index, name) -> name to index }
    return people
      .filter { !it.isBot && it.isOffered }
      .filter {
        it.username.startsWith(query, ignoreCase = true) ||
          it.displayName.split(' ').any { part -> part.startsWith(query, ignoreCase = true) }
      }
      // sortedBy is stable: people without a rank keep the member order.
      .sortedBy { rank[it.username.lowercase()] ?: Int.MAX_VALUE }
      .take(limit)
  }

  /**
   * The usernames to rank first: the authors of [messages] from the newest, without [myId],
   * then [recentlyMentioned].
   */
  fun priority(messages: List<Message>, myId: String, users: List<User>, recentlyMentioned: List<String>): List<String> {
    val byId = users.associateBy { it.id }
    val senders = messages.asReversed().map { it.author }.filter { it != myId }.distinct().mapNotNull { byId[it]?.username }
    return (senders + recentlyMentioned).distinct()
  }

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
  fun complete(text: String, user: User, caret: Int = text.length): Completion = complete(text, user.username, caret)

  /** Replaces the typed `@query` with `@handle` (a group's, which the core expands on sending). */
  fun complete(text: String, handle: String, caret: Int = text.length): Completion = replaceWord(text, caret, "@$handle")
}
