package de.zeitgleich.teamcraft.notifications

import android.content.Context
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import uniffi.teamcraft_ffi.MentionSettings

/**
 * Device-local notification preferences, as on desktop: keywords, the first name as a mention,
 * `@channel` notifications and when to confirm a `@channel` before sending. The server only sees
 * explicit mentions; these are applied on the device by the core (teamcraft-client-api `notify`).
 */
class MentionPrefs(context: Context) {
  private val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)
  private val state = MutableStateFlow(read())

  val settings: StateFlow<MentionSettings> = state

  /** Ask before `@channel` in conversations with more members than this (0 = never). */
  val confirmChannelOver: Int get() = prefs.getInt(KEY_CONFIRM_OVER, DEFAULT_CONFIRM_OVER)

  fun setConfirmChannelOver(count: Int) {
    prefs.edit().putInt(KEY_CONFIRM_OVER, count.coerceAtLeast(0)).apply()
  }

  fun update(settings: MentionSettings) {
    prefs.edit()
      .putBoolean(KEY_FIRST_NAME, settings.isFirstNameMention)
      .putBoolean(KEY_CHANNEL, settings.doChannelMentionsNotify)
      .putString(KEY_KEYWORDS, settings.keywords.joinToString(KEYWORD_SEPARATOR))
      .apply()
    state.value = settings
  }

  private fun read(): MentionSettings = MentionSettings(
    isFirstNameMention = prefs.getBoolean(KEY_FIRST_NAME, true),
    doChannelMentionsNotify = prefs.getBoolean(KEY_CHANNEL, true),
    keywords = parseKeywords(prefs.getString(KEY_KEYWORDS, "").orEmpty()),
  )

  companion object {
    private const val FILE = "mention_prefs"
    private const val KEY_FIRST_NAME = "first_name_is_mention"
    private const val KEY_CHANNEL = "channel_mentions_notify"
    private const val KEY_KEYWORDS = "keywords"
    private const val KEY_CONFIRM_OVER = "confirm_channel_over"
    private const val KEYWORD_SEPARATOR = "\n"
    /** Same default as desktop. */
    const val DEFAULT_CONFIRM_OVER = 10

    /** Keywords from a comma- or line-separated text field. */
    fun parseKeywords(text: String): List<String> =
      text.split(',', '\n').map { it.trim() }.filter { it.isNotEmpty() }.distinct()
  }
}
