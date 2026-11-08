package de.zeitgleich.teamcraft.ui

import android.content.Context
import de.zeitgleich.teamcraft.settings.LocalSettingChanges

/**
 * Composer preferences. "Send with Enter" suits hardware keyboards and tablets; off by default,
 * so Enter makes a new line as in most messengers on phones. It stays on this device: a desktop's
 * Enter-sends would break new lines on a touch keyboard.
 */
class ComposerPrefs private constructor(context: Context) {
  private val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)

  val isSendWithEnter: Boolean get() = prefs.getBoolean(KEY_SEND_WITH_ENTER, false)

  /** Whether others see "… is typing" while the user writes; on by default, as on desktop. Follows the account (#945). */
  val isSharingTyping: Boolean get() = prefs.getBoolean(KEY_SHARE_TYPING, true)

  fun setSharingTyping(isOn: Boolean) {
    prefs.edit().putBoolean(KEY_SHARE_TYPING, isOn).apply()
    LocalSettingChanges.changed()
  }

  fun setSendWithEnter(isOn: Boolean) {
    prefs.edit().putBoolean(KEY_SEND_WITH_ENTER, isOn).apply()
  }

  companion object {
    private const val FILE = "composer_prefs"
    private const val KEY_SEND_WITH_ENTER = "send_with_enter"
    private const val KEY_SHARE_TYPING = "share_typing"

    fun prefs(context: Context) = ComposerPrefs(context.applicationContext)
  }
}
