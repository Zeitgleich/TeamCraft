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

  /** Whether the formatting toolbar shows above the field (#1051); off until the user turns it on. */
  val isFormatBarOpen: Boolean get() = prefs.getBoolean(KEY_FORMAT_BAR, false)

  fun setFormatBarOpen(isOpen: Boolean) {
    prefs.edit().putBoolean(KEY_FORMAT_BAR, isOpen).apply()
  }

  fun setSendWithEnter(isOn: Boolean) {
    prefs.edit().putBoolean(KEY_SEND_WITH_ENTER, isOn).apply()
  }

  companion object {
    private const val FILE = "composer_prefs"
    private const val KEY_SEND_WITH_ENTER = "send_with_enter"
    private const val KEY_SHARE_TYPING = "share_typing"
    private const val KEY_FORMAT_BAR = "format_bar"

    fun prefs(context: Context) = ComposerPrefs(context.applicationContext)
  }
}
