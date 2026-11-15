package de.zeitgleich.teamcraft.ui

import android.content.Context

/**
 * Where the user stopped reading in each conversation (issue #2055): the message at the top of the
 * screen, kept on this device so it survives the app being killed. Only conversations read up from
 * the end have one; reaching the end forgets it. The newest [MAX_PLACES] are kept.
 */
class ReadingPlaces private constructor(context: Context) {
  private val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)

  /** The message to reopen [conversationId] at, if the user left it mid-history. */
  fun messageOf(conversationId: String): String? = entries().firstOrNull { it.first == conversationId }?.second

  /** Remembers [messageId] for [conversationId]; null forgets it (the user read to the end). */
  fun set(conversationId: String, maybeMessageId: String?) {
    val kept = entries().filter { it.first != conversationId }
    val next = if (maybeMessageId == null) kept else listOf(conversationId to maybeMessageId) + kept
    prefs.edit().putString(KEY, encode(next.take(MAX_PLACES))).apply()
  }

  private fun entries(): List<Pair<String, String>> = decode(prefs.getString(KEY, null))

  companion object {
    private const val FILE = "reading_places"
    private const val KEY = "places"
    const val MAX_PLACES = 60

    fun of(context: Context) = ReadingPlaces(context.applicationContext)

    /** One `conversation=message` per line, newest first. IDs are numbers, never containing `=` or a line break. */
    internal fun encode(entries: List<Pair<String, String>>): String = entries.joinToString("\n") { "${it.first}=${it.second}" }

    internal fun decode(text: String?): List<Pair<String, String>> =
      text.orEmpty().lines().mapNotNull { line ->
        val at = line.indexOf('=')
        if (at <= 0 || at == line.lastIndex) null else line.substring(0, at) to line.substring(at + 1)
      }
  }
}
