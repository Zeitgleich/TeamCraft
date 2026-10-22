package de.zeitgleich.teamcraft.notifications

import android.content.Context
import de.zeitgleich.teamcraft.core.CoreRepository
import uniffi.teamcraft_ffi.MentionRulesItem

/**
 * Mention preferences, as on desktop: keywords, the first name as a mention and `@channel`
 * notifications live in the core's store (pushes are matched there, teamcraft-core
 * `notify_rules`); when to confirm a `@channel` before sending is a setting of this device.
 */
class MentionPrefs(context: Context, private val core: CoreRepository) {
  private val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)

  /** Ask before `@channel` in conversations with more members than this (0 = never). */
  val confirmChannelOver: Int get() = prefs.getInt(KEY_CONFIRM_OVER, DEFAULT_CONFIRM_OVER)

  fun setConfirmChannelOver(count: Int) {
    prefs.edit().putInt(KEY_CONFIRM_OVER, count.coerceAtLeast(0)).apply()
  }

  suspend fun rules(): MentionRulesItem = core.core().mentionRules()

  suspend fun setRules(rules: MentionRulesItem) = core.core().setMentionRules(rules)

  companion object {
    private const val FILE = "mention_prefs"
    private const val KEY_CONFIRM_OVER = "confirm_channel_over"
    /** Same default as desktop. */
    const val DEFAULT_CONFIRM_OVER = 10

    /** Keywords from a comma- or line-separated text field. */
    fun parseKeywords(text: String): List<String> =
      text.split(',', '\n').map { it.trim() }.filter { it.isNotEmpty() }.distinct()
  }
}
